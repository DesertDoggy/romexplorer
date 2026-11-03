//! Asking a language model, for any feature: [`ask`] = the shared answer cache, then the
//! chosen provider, then the cache again.
//!
//! **Providers** (`ai_provider`): Gemini with Google Search, Claude with the web search tool
//! (Anthropic Messages API), OpenAI with web search (Responses API), Perplexity Sonar. Each
//! takes its own API key (pay per use, `api.env` like the metadata providers' keys) and a
//! model setting (`ai_model_<provider>`; Gemini also reads the older `gemini_model`). The
//! question is sent as it is, with "Answer in <Language>." under it ([`language_line`]).
//!
//! **Cache** (`ai_answers`, migration 0078): one answer per purpose (`merge_review.differences`),
//! subject (stable ids -- IGDB ids, platform, variant -- never row ids, so a rebuilt catalogue
//! still finds them), language and question. An answer is kept until asked again (a pinned
//! one never is) or until it is older than `ai_cache_days` (unset: forever). Asking in a
//! language the cache does not have is the user's call ([`Lookup::other_language`]), never
//! automatic: each ask costs money.
//!
//! **Cost** is approximate: the provider's token counts times a per-model price table
//! ([`prices`], overridable with `ai_prices`), plus Claude's per-search fee; Perplexity's own
//! reported cost when it sends one.
//!
//! Not here: a local CLI backend (a signed-in `claude -p`, Gemini CLI or Codex CLI). It would
//! spend the user's subscription limits from a background server, and the vendors' terms for
//! scripted use of a subscription differ and change; the browser links of the Ask menu cover
//! that use without either problem.

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::database::settings as db_settings;

use super::{App, Error, Result};

/// The in-app providers, in the order the settings show them.
pub const PROVIDERS: [&str; 4] = ["gemini", "claude", "openai", "perplexity"];

/// Each provider's `api.env` key.
pub fn key_name(provider: &str) -> Option<&'static str> {
    match provider {
        "gemini" => Some("gemini_api_key"),
        "claude" => Some("anthropic_api_key"),
        "openai" => Some("openai_api_key"),
        "perplexity" => Some("perplexity_api_key"),
        _ => None,
    }
}

/// The model asked when the provider's model setting is unset.
pub fn default_model(provider: &str) -> &'static str {
    match provider {
        "claude" => "claude-opus-5-5",
        "openai" => "gpt-5",
        "perplexity" => "sonar",
        // Gemini: the newest Flash the key lists (`gemini_models`); this when it lists none.
        _ => GEMINI_FALLBACK,
    }
}

/// The browser links of the Ask menu: the question prefilled, answered with the user's own
/// sign-in to each service. `ai_ask_first` puts one of them first.
pub const ASK_LINKS: [(&str, &str); 4] = [("google", "Google AI Mode"), ("claude", "Claude"), ("chatgpt", "ChatGPT"), ("perplexity", "Perplexity")];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Source {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AskLink {
    pub id: &'static str,
    pub label: &'static str,
    pub url: String,
}

/// The Ask menu for `asked` (the question with its language line), `first` leading.
pub fn ask_links(asked: &str, first: &str) -> Vec<AskLink> {
    let url = |base: &str, extra: &[(&str, &str)]| {
        let mut pairs: Vec<(&str, &str)> = vec![("q", asked)];
        pairs.extend_from_slice(extra);
        reqwest::Url::parse_with_params(base, &pairs).map(|u| u.to_string()).unwrap_or_default()
    };
    let mut out: Vec<AskLink> = ASK_LINKS
        .iter()
        .map(|(id, label)| AskLink {
            id,
            label,
            url: match *id {
                "google" => url("https://www.google.com/search", &[("udm", "50")]),
                "claude" => url("https://claude.ai/new", &[]),
                "chatgpt" => url("https://chatgpt.com/", &[]),
                _ => url("https://www.perplexity.ai/search", &[]),
            },
        })
        .collect();
    if let Some(i) = out.iter().position(|l| l.id == first) {
        let link = out.remove(i);
        out.insert(0, link);
    }
    out
}

// ---------------------------------------------------------------------------
// language
// ---------------------------------------------------------------------------

/// The UI languages, as the name a prompt asks for an answer in.
const LANGUAGES: [(&str, &str); 9] = [("en", "English"), ("ja", "Japanese"), ("fr", "French"), ("es", "Spanish"), ("it", "Italian"), ("pt", "European Portuguese"), ("de", "German"), ("ko", "Korean"), ("pl", "Polish")];

/// A UI language code as this app has it (`de-DE` -> `de`), `None` for one it has not.
pub fn language_code(lang: Option<&str>) -> Option<&'static str> {
    let lang = lang?.trim().to_ascii_lowercase();
    let code = lang.split(['-', '_']).next().unwrap_or("");
    LANGUAGES.iter().find(|(c, _)| *c == code).map(|(c, _)| *c)
}

/// "Answer in <Language>." for UI language `lang`, `None` for one this app does not have.
/// The question itself stays English, which searches better; names stay as written.
pub fn language_line(lang: Option<&str>) -> Option<String> {
    let code = language_code(lang)?;
    LANGUAGES.iter().find(|(c, _)| *c == code).map(|(_, name)| format!("Answer in {name}."))
}

/// The question with the answer language's line under it: what a model is asked.
pub fn with_language(q: &str, lang: Option<&str>) -> String {
    match language_line(lang) {
        Some(line) => format!("{q}\n{line}"),
        None => q.to_string(),
    }
}

// ---------------------------------------------------------------------------
// settings
// ---------------------------------------------------------------------------

pub const KEY_PROVIDER: &str = "ai_provider";
pub const KEY_ASK_FIRST: &str = "ai_ask_first";
pub const KEY_CACHE_DAYS: &str = "ai_cache_days";
pub const KEY_PRICES: &str = "ai_prices";

pub fn model_key(provider: &str) -> String {
    format!("ai_model_{provider}")
}

pub async fn provider(app: &App) -> Result<String> {
    let p: Option<String> = db_settings::get(&app.pool, KEY_PROVIDER).await?;
    Ok(p.map(|p| p.trim().to_ascii_lowercase()).filter(|p| PROVIDERS.contains(&p.as_str())).unwrap_or_else(|| "gemini".to_string()))
}

pub async fn ask_first(app: &App) -> Result<String> {
    let p: Option<String> = db_settings::get(&app.pool, KEY_ASK_FIRST).await?;
    Ok(p.filter(|p| ASK_LINKS.iter().any(|(id, _)| id == p)).unwrap_or_else(|| "google".to_string()))
}

/// The model setting for `provider`, if set. Gemini also reads `gemini_model`, the setting
/// from before there was a choice of provider.
pub async fn model_setting(app: &App, provider: &str) -> Result<Option<String>> {
    let mut m: Option<String> = db_settings::get(&app.pool, &model_key(provider)).await?;
    if m.as_deref().is_none_or(|m| m.trim().is_empty()) && provider == "gemini" {
        m = db_settings::get(&app.pool, db_settings::KEY_GEMINI_MODEL).await?;
    }
    Ok(m.map(|m| m.trim().trim_start_matches("models/").to_string()).filter(|m| !m.is_empty()))
}

async fn api_key(app: &App, provider: &str) -> Option<String> {
    let name = key_name(provider)?;
    let mut creds = crate::database::ApiCredentials::load(&app.paths.api_env()).ok()?;
    creds.decrypt(&app.vault);
    creds.get(name).map(str::to_string)
}

// ---------------------------------------------------------------------------
// prices
// ---------------------------------------------------------------------------

/// US dollars per million tokens, in and out.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Price {
    #[serde(rename = "in")]
    pub input: f64,
    #[serde(rename = "out")]
    pub output: f64,
}

/// The built-in table: list prices as of this release, by model id prefix (the longest
/// matching prefix wins). `ai_prices` overrides or adds entries; a model in neither has no
/// cost shown.
const PRICES: [(&str, f64, f64); 12] = [
    ("gemini-2.5-flash", 0.30, 2.50),
    ("gemini-2.5-pro", 1.25, 10.0),
    ("gemini-3-flash", 0.50, 3.00),
    ("claude-opus-5-5", 4.0, 20.0),
    ("claude-opus-5", 5.0, 25.0),
    ("claude-sonnet-5-5", 2.0, 10.0),
    ("claude-haiku-5-5", 0.10, 0.50),
    ("gpt-5-mini", 0.25, 2.0),
    ("gpt-5", 1.25, 10.0),
    ("sonar-pro", 3.0, 15.0),
    ("sonar", 1.0, 1.0),
    ("claude-fable-5-1", 10.0, 50.0),
];

/// Claude's web search: US dollars per search, on top of the tokens.
const CLAUDE_SEARCH_FEE: f64 = 0.01;

/// The price table in use: the built-in one with `ai_prices` laid over it.
pub async fn prices(app: &App) -> Result<BTreeMap<String, Price>> {
    let mut out: BTreeMap<String, Price> = PRICES.iter().map(|(m, i, o)| (m.to_string(), Price { input: *i, output: *o })).collect();
    let overrides: Option<BTreeMap<String, Price>> = db_settings::get(&app.pool, KEY_PRICES).await.ok().flatten();
    out.extend(overrides.unwrap_or_default());
    Ok(out)
}

fn price_of(table: &BTreeMap<String, Price>, model: &str) -> Option<Price> {
    table.iter().filter(|(prefix, _)| model.starts_with(prefix.as_str())).max_by_key(|(prefix, _)| prefix.len()).map(|(_, p)| *p)
}

fn cost(table: &BTreeMap<String, Price>, reply: &Reply) -> Option<f64> {
    if let Some(c) = reply.reported_cost {
        return Some(c);
    }
    let p = price_of(table, &reply.model)?;
    let tokens = (reply.tokens_in as f64 * p.input + reply.tokens_out as f64 * p.output) / 1_000_000.0;
    Some(tokens + reply.searches as f64 * if reply.provider == "claude" { CLAUDE_SEARCH_FEE } else { 0.0 })
}

// ---------------------------------------------------------------------------
// the cache
// ---------------------------------------------------------------------------

/// A saved answer, as the UI shows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Answer {
    #[serde(default)]
    pub id: u64,
    pub purpose: String,
    pub subject_key: String,
    pub lang: String,
    /// For `source` api: gemini / claude / openai / perplexity. For browser: the Ask menu
    /// link it was pasted from (google / claude / chatgpt / perplexity).
    pub provider: String,
    /// `api` (asked by the app) or `browser` (pasted in from an Ask menu link).
    #[serde(default = "default_source")]
    pub source: String,
    pub model: String,
    pub prompt: String,
    pub answer: String,
    #[serde(default)]
    pub sources: Vec<Source>,
    #[serde(default)]
    pub tokens_in: u64,
    #[serde(default)]
    pub tokens_out: u64,
    /// Approximate.
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    /// When a browser answer was pasted in.
    #[serde(default)]
    pub pasted_at: Option<String>,
}

fn default_source() -> String {
    "api".into()
}

impl Answer {
    /// Its entry in `ai_source_priority`: `api:<provider>` or `browser:<site>`.
    pub fn source_key(&self) -> String {
        format!("{}:{}", self.source, self.provider)
    }
}

/// `ai_source_priority`: which saved answer is shown when one question has several, best
/// first -- `curated` (a feature's own curated text, where it has one, such as the
/// Definitive notes), `api:<provider>`, `browser:<site>`. Sources not listed come after,
/// newest first. Unset: curated first, then newest first.
pub const KEY_SOURCE_PRIORITY: &str = "ai_source_priority";

/// Every entry `ai_source_priority` can hold, in the default order.
pub fn source_keys() -> Vec<String> {
    let mut out = vec!["curated".to_string()];
    out.extend(PROVIDERS.iter().map(|p| format!("api:{p}")));
    out.extend(ASK_LINKS.iter().map(|(id, _)| format!("browser:{id}")));
    out
}

pub async fn source_priority(app: &App) -> Result<Vec<String>> {
    let list: Option<Vec<String>> = db_settings::get(&app.pool, KEY_SOURCE_PRIORITY).await.ok().flatten();
    Ok(list.filter(|l| !l.is_empty()).unwrap_or_else(|| vec!["curated".to_string()]))
}

/// Where `key` stands in `priority`: its index, or after every listed one.
pub fn rank_of(priority: &[String], key: &str) -> usize {
    priority.iter().position(|k| k == key).unwrap_or(priority.len())
}

/// What [`lookup`] found or asked.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Lookup {
    /// In the language asked for: from the cache, or just asked.
    pub answer: Option<Answer>,
    /// `answer` came from the cache rather than from this request.
    pub cached: bool,
    /// Nothing in this language, but an answer in this one is cached: shown instead, with an
    /// "Ask in <language>" button rather than a paid request nobody asked for.
    pub other_language: Option<Answer>,
    /// Why there is no answer (no key, the request failed, a pinned answer kept).
    pub error: Option<String>,
}

pub fn prompt_hash(prompt: &str) -> String {
    crate::database::import::sha1_hex(prompt)
}

const ANSWER_SELECT: &str = "SELECT id, purpose, subject_key, lang, provider, CAST(source AS CHAR) AS source, CAST(pasted_at AS CHAR) AS pasted_at, model, prompt, answer, CAST(sources AS CHAR) AS sources, tokens_in, tokens_out, CAST(cost_usd AS DOUBLE) AS cost_usd, CAST(created_at AS CHAR) AS created_at, pinned FROM ai_answers";

fn row_to_answer(r: &sqlx::mysql::MySqlRow) -> Result<Answer> {
    let sources: Option<String> = r.try_get("sources")?;
    Ok(Answer {
        id: r.try_get("id")?,
        purpose: r.try_get("purpose")?,
        subject_key: r.try_get("subject_key")?,
        lang: r.try_get("lang")?,
        provider: r.try_get("provider")?,
        source: r.try_get("source")?,
        pasted_at: r.try_get("pasted_at")?,
        model: r.try_get("model")?,
        prompt: r.try_get("prompt")?,
        answer: r.try_get("answer")?,
        sources: sources.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
        tokens_in: r.try_get::<u32, _>("tokens_in")? as u64,
        tokens_out: r.try_get::<u32, _>("tokens_out")? as u64,
        cost_usd: r.try_get("cost_usd")?,
        created_at: r.try_get("created_at")?,
        pinned: r.try_get("pinned")?,
    })
}

/// "Not expired" for a query on `ai_answers`: `ai_cache_days` old at most, a pinned answer
/// always.
async fn fresh_sql(app: &App) -> Result<String> {
    let days: Option<u32> = db_settings::get(&app.pool, KEY_CACHE_DAYS).await.ok().flatten();
    Ok(match days.filter(|d| *d > 0) {
        Some(d) => format!(" AND (pinned = TRUE OR created_at >= NOW() - INTERVAL {d} DAY)"),
        None => String::new(),
    })
}

/// Every saved answer to `prompt` about `subject_key` in `lang`, best first by
/// `ai_source_priority`, then newest.
pub async fn cached_all(app: &App, purpose: &str, subject_key: &str, lang: &str, prompt: &str) -> Result<Vec<Answer>> {
    let fresh = fresh_sql(app).await?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("{ANSWER_SELECT} WHERE purpose = ? AND subject_key = ? AND lang = ? AND prompt_hash = ?{fresh} ORDER BY created_at DESC, id DESC")))
        .bind(purpose)
        .bind(subject_key)
        .bind(lang)
        .bind(prompt_hash(prompt))
        .fetch_all(&app.pool)
        .await?;
    let mut list = rows.iter().map(row_to_answer).collect::<Result<Vec<_>>>()?;
    let priority = source_priority(app).await?;
    list.sort_by_key(|a| rank_of(&priority, &a.source_key()));
    Ok(list)
}

/// The saved answer to `prompt` about `subject_key` in `lang` that `ai_source_priority`
/// puts first, if any.
pub async fn cached(app: &App, purpose: &str, subject_key: &str, lang: &str, prompt: &str) -> Result<Option<Answer>> {
    Ok(cached_all(app, purpose, subject_key, lang, prompt).await?.into_iter().next())
}

/// The newest saved answer about `subject_key` in any other language.
async fn cached_elsewhere(app: &App, purpose: &str, subject_key: &str, lang: &str) -> Result<Option<Answer>> {
    let fresh = fresh_sql(app).await?;
    let row = sqlx::query(sqlx::AssertSqlSafe(format!("{ANSWER_SELECT} WHERE purpose = ? AND subject_key = ? AND lang <> ?{fresh} ORDER BY lang = 'en' DESC, created_at DESC LIMIT 1")))
        .bind(purpose)
        .bind(subject_key)
        .bind(lang)
        .fetch_optional(&app.pool)
        .await?;
    row.as_ref().map(row_to_answer).transpose()
}

async fn save(app: &App, a: &Answer) -> Result<u64> {
    let sources = serde_json::to_string(&a.sources).unwrap_or_else(|_| "[]".into());
    sqlx::query(
        "INSERT INTO ai_answers (purpose, subject_key, lang, provider, source, pasted_at, model, prompt_hash, prompt, answer, sources, tokens_in, tokens_out, cost_usd, pinned) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON DUPLICATE KEY UPDATE model = VALUES(model), prompt = VALUES(prompt), answer = VALUES(answer), sources = VALUES(sources), tokens_in = VALUES(tokens_in), tokens_out = VALUES(tokens_out), cost_usd = VALUES(cost_usd), pasted_at = VALUES(pasted_at), created_at = CURRENT_TIMESTAMP",
    )
    .bind(&a.purpose)
    .bind(&a.subject_key)
    .bind(&a.lang)
    .bind(&a.provider)
    .bind(&a.source)
    .bind(a.pasted_at.is_some().then(|| chrono::Utc::now().naive_utc()))
    .bind(&a.model)
    .bind(prompt_hash(&a.prompt))
    .bind(&a.prompt)
    .bind(&a.answer)
    .bind(sources)
    .bind(a.tokens_in.min(u32::MAX as u64) as u32)
    .bind(a.tokens_out.min(u32::MAX as u64) as u32)
    .bind(a.cost_usd)
    .bind(a.pinned)
    .execute(&app.pool)
    .await?;
    let id: u64 = sqlx::query_scalar("SELECT id FROM ai_answers WHERE purpose = ? AND subject_key = ? AND lang = ? AND prompt_hash = ? AND source = ? AND provider = ?")
        .bind(&a.purpose)
        .bind(&a.subject_key)
        .bind(&a.lang)
        .bind(prompt_hash(&a.prompt))
        .bind(&a.source)
        .bind(&a.provider)
        .fetch_one(&app.pool)
        .await?;
    Ok(id)
}

/// How [`lookup`] may spend money.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Spend {
    /// The cache only, never a request: what a page shows when it opens, for a question
    /// asked only on the user's click.
    Never,
    /// The cache; ask only when nothing about the subject is saved in any language.
    #[default]
    IfNothingSaved,
    /// Ask now in this language (the "Ask in <language>" button), unless it is saved.
    IfMissing,
    /// Ask again and replace what is saved ("Ask again"), unless that is pinned.
    Again,
}

/// The answer to `question` about `subject_key` in UI language `lang`: the cache first, then
/// the chosen provider (as `spend` allows), and the new answer saved. The one helper every
/// feature that asks a model goes through.
pub async fn lookup(app: &App, purpose: &str, subject_key: &str, lang: Option<&str>, question: &str, spend: Spend) -> Result<Lookup> {
    let lang = language_code(lang).unwrap_or("en");
    let prompt = with_language(question, Some(lang));
    let all = cached_all(app, purpose, subject_key, lang, &prompt).await?;
    let saved = all.first().cloned();
    let current = format!("api:{}", provider(app).await?);
    let mine = all.iter().find(|a| a.source_key() == current).cloned();
    let mut out = Lookup::default();
    match (&mine, spend) {
        (Some(a), Spend::Again) if a.pinned => {
            out.error = Some("this answer is pinned: unpin it to ask again".into());
            out.answer = mine;
            out.cached = true;
            return Ok(out);
        }
        (_, Spend::IfNothingSaved | Spend::IfMissing | Spend::Never) if saved.is_some() => {
            out.answer = saved;
            out.cached = true;
            return Ok(out);
        }
        _ => {}
    }
    if matches!(spend, Spend::IfNothingSaved | Spend::Never) {
        if let Some(other) = cached_elsewhere(app, purpose, subject_key, lang).await? {
            out.other_language = Some(other);
            return Ok(out);
        }
    }
    if spend == Spend::Never {
        return Ok(out);
    }
    let provider = provider(app).await?;
    match ask_provider(app, &provider, &prompt).await {
        Ok(reply) => {
            let table = prices(app).await?;
            let mut a = Answer {
                id: 0,
                purpose: purpose.to_string(),
                subject_key: subject_key.to_string(),
                lang: lang.to_string(),
                cost_usd: cost(&table, &reply),
                provider: reply.provider,
                model: reply.model,
                prompt,
                answer: reply.text,
                sources: reply.sources,
                tokens_in: reply.tokens_in,
                tokens_out: reply.tokens_out,
                created_at: None,
                pinned: false,
                source: "api".into(),
                pasted_at: None,
            };
            a.id = save(app, &a).await?;
            // The new answer, as saved (with its date), whatever the source order would show.
            out.answer = cached_all(app, purpose, subject_key, lang, &a.prompt).await?.into_iter().find(|x| x.id == a.id).or(Some(a));
        }
        Err(e) => {
            out.error = Some(e);
            out.answer = saved;
            out.cached = out.answer.is_some();
        }
    }
    Ok(out)
}

/// An answer the user copied from one of the Ask menu's browser links and pasted in: saved
/// like an API answer, as `browser` from `site`, replacing what was pasted from that site
/// for the same question before (a pinned one stays).
pub async fn save_pasted(app: &App, purpose: &str, subject_key: &str, lang: Option<&str>, question: &str, site: &str, text: &str) -> Result<Answer> {
    if !ASK_LINKS.iter().any(|(id, _)| *id == site) {
        return Err(Error::invalid(format!("site must be one of {}", ASK_LINKS.iter().map(|(id, _)| *id).collect::<Vec<_>>().join(", "))));
    }
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::invalid("nothing pasted"));
    }
    let lang = language_code(lang).unwrap_or("en");
    let prompt = with_language(question, Some(lang));
    if cached_all(app, purpose, subject_key, lang, &prompt).await?.iter().any(|a| a.source == "browser" && a.provider == site && a.pinned) {
        return Err(Error::conflict("the answer pasted from this site is pinned: unpin it to replace it"));
    }
    let mut a = Answer {
        id: 0,
        purpose: purpose.to_string(),
        subject_key: subject_key.to_string(),
        lang: lang.to_string(),
        provider: site.to_string(),
        source: "browser".into(),
        model: ASK_LINKS.iter().find(|(id, _)| *id == site).map(|(_, label)| label.to_string()).unwrap_or_default(),
        prompt,
        answer: text.to_string(),
        sources: Vec::new(),
        tokens_in: 0,
        tokens_out: 0,
        cost_usd: None,
        created_at: None,
        pinned: false,
        pasted_at: Some(String::new()),
    };
    a.id = save(app, &a).await?;
    Ok(cached_all(app, purpose, subject_key, lang, &a.prompt).await?.into_iter().find(|x| x.id == a.id).unwrap_or(a))
}

pub async fn set_pinned(app: &App, id: u64, pinned: bool) -> Result<()> {
    let n = sqlx::query("UPDATE ai_answers SET pinned = ? WHERE id = ?").bind(pinned).bind(id).execute(&app.pool).await?.rows_affected();
    if n == 0 && sqlx::query("SELECT 1 FROM ai_answers WHERE id = ?").bind(id).fetch_optional(&app.pool).await?.is_none() {
        return Err(Error::not_found(format!("no saved answer {id}")));
    }
    Ok(())
}

/// One in-app provider as Config › AI shows it.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderView {
    pub id: &'static str,
    /// The model setting, if set.
    pub model: Option<String>,
    /// What is asked when it is unset.
    pub default_model: &'static str,
    pub key_set: bool,
}

/// Config › AI: the choices and what each is set to.
#[derive(Debug, Clone, Serialize)]
pub struct Overview {
    pub provider: String,
    pub ask_first: String,
    pub ask_links: Vec<(&'static str, &'static str)>,
    pub cache_days: Option<u32>,
    pub providers: Vec<ProviderView>,
    pub prices: BTreeMap<String, Price>,
    pub stats: Stats,
    /// `ai_source_priority` as set (empty: the default), and every entry it can hold.
    pub source_priority: Vec<String>,
    pub source_keys: Vec<String>,
}

pub async fn overview(app: &App) -> Result<Overview> {
    let creds = crate::database::ApiCredentials::load(&app.paths.api_env()).ok();
    let mut providers = Vec::new();
    for id in PROVIDERS {
        let key_set = key_name(id).zip(creds.as_ref()).is_some_and(|(k, c)| c.get(k).is_some());
        providers.push(ProviderView { id, model: model_setting(app, id).await?, default_model: if id == "gemini" { "newest Flash" } else { default_model(id) }, key_set });
    }
    Ok(Overview {
        provider: provider(app).await?,
        ask_first: ask_first(app).await?,
        ask_links: ASK_LINKS.to_vec(),
        cache_days: db_settings::get(&app.pool, KEY_CACHE_DAYS).await.ok().flatten(),
        providers,
        prices: prices(app).await?,
        stats: stats(app).await?,
        source_priority: db_settings::get::<Vec<String>>(&app.pool, KEY_SOURCE_PRIORITY).await.ok().flatten().unwrap_or_default(),
        source_keys: source_keys(),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct PurposeCount {
    pub purpose: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub count: u64,
    pub pinned: u64,
    /// Text held, in bytes (prompts, answers and sources).
    pub bytes: u64,
    /// Approximate, from the answers saved.
    pub cost_month_usd: f64,
    pub cost_total_usd: f64,
    pub purposes: Vec<PurposeCount>,
}

pub async fn stats(app: &App) -> Result<Stats> {
    let r = sqlx::query(
        "SELECT COUNT(*) AS n, CAST(COALESCE(SUM(pinned), 0) AS SIGNED) AS pinned, CAST(COALESCE(SUM(LENGTH(prompt) + LENGTH(answer) + COALESCE(LENGTH(sources), 0)), 0) AS SIGNED) AS bytes, \
         CAST(COALESCE(SUM(cost_usd), 0) AS DOUBLE) AS total, CAST(COALESCE(SUM(CASE WHEN created_at >= DATE_FORMAT(NOW(), '%Y-%m-01') THEN cost_usd END), 0) AS DOUBLE) AS month FROM ai_answers",
    )
    .fetch_one(&app.pool)
    .await?;
    let purposes = sqlx::query("SELECT purpose, COUNT(*) AS n FROM ai_answers GROUP BY purpose ORDER BY purpose")
        .fetch_all(&app.pool)
        .await?
        .iter()
        .map(|p| Ok(PurposeCount { purpose: p.try_get("purpose")?, count: p.try_get::<i64, _>("n")?.max(0) as u64 }))
        .collect::<Result<_>>()?;
    Ok(Stats {
        count: r.try_get::<i64, _>("n")?.max(0) as u64,
        pinned: r.try_get::<i64, _>("pinned")?.max(0) as u64,
        bytes: r.try_get::<i64, _>("bytes")?.max(0) as u64,
        cost_month_usd: r.try_get("month")?,
        cost_total_usd: r.try_get("total")?,
        purposes,
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClearRequest {
    /// One feature's answers only; none: all.
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub keep_pinned: bool,
    /// Only `api` or `browser` answers; none: both.
    #[serde(default)]
    pub source: Option<String>,
    /// Only this provider's / site's answers (with `source`).
    #[serde(default)]
    pub provider: Option<String>,
}

pub async fn clear(app: &App, req: &ClearRequest) -> Result<u64> {
    let mut sql = "DELETE FROM ai_answers WHERE 1 = 1".to_string();
    if req.keep_pinned {
        sql.push_str(" AND pinned = FALSE");
    }
    let purpose = req.purpose.as_deref().map(str::trim).filter(|p| !p.is_empty());
    if purpose.is_some() {
        sql.push_str(" AND purpose = ?");
    }
    let source = req.source.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(s) = source {
        if !["api", "browser"].contains(&s) {
            return Err(Error::invalid("source must be api or browser"));
        }
        sql.push_str(" AND source = ?");
    }
    let provider = req.provider.as_deref().map(str::trim).filter(|p| !p.is_empty());
    if provider.is_some() {
        sql.push_str(" AND provider = ?");
    }
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
    if let Some(p) = purpose {
        q = q.bind(p);
    }
    if let Some(s) = source {
        q = q.bind(s);
    }
    if let Some(p) = provider {
        q = q.bind(p);
    }
    Ok(q.execute(&app.pool).await?.rows_affected())
}

/// Every saved answer, for a file another install can import.
pub async fn export(app: &App) -> Result<Vec<Answer>> {
    sqlx::query(sqlx::AssertSqlSafe(format!("{ANSWER_SELECT} ORDER BY id"))).fetch_all(&app.pool).await?.iter().map(row_to_answer).collect()
}

/// Answers from an export. One already saved for the same question is replaced, unless it is
/// pinned here.
pub async fn import(app: &App, answers: &[Answer]) -> Result<u64> {
    let mut n = 0;
    for a in answers {
        if a.purpose.is_empty() || a.subject_key.is_empty() || a.prompt.is_empty() || a.answer.is_empty() {
            continue;
        }
        let lang = language_code(Some(&a.lang)).unwrap_or("en");
        if cached(app, &a.purpose, &a.subject_key, lang, &a.prompt).await?.is_some_and(|c| c.pinned) {
            continue;
        }
        save(app, &Answer { lang: lang.to_string(), ..a.clone() }).await?;
        n += 1;
    }
    Ok(n)
}

// ---------------------------------------------------------------------------
// providers
// ---------------------------------------------------------------------------

/// One provider's answer.
#[derive(Debug, Clone, Default)]
pub struct Reply {
    pub provider: String,
    pub model: String,
    pub text: String,
    pub sources: Vec<Source>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Web searches billed by the search (Claude).
    pub searches: u64,
    /// The provider's own figure for the request, when it sends one (Perplexity).
    pub reported_cost: Option<f64>,
}

/// Asks `provider` with its key and model setting.
pub async fn ask_provider(app: &App, provider: &str, prompt: &str) -> std::result::Result<Reply, String> {
    let Some(key) = api_key(app, provider).await else {
        return Err(format!("no {provider} API key set (Config › AI): use the Ask menu"));
    };
    let model = model_setting(app, provider).await.ok().flatten();
    match provider {
        "gemini" => ask_gemini(app, &key, model, prompt).await,
        "claude" => ask_claude(app, &key, &model.unwrap_or_else(|| default_model("claude").into()), prompt).await,
        "openai" => ask_openai(app, &key, &model.unwrap_or_else(|| default_model("openai").into()), prompt).await,
        "perplexity" => ask_perplexity(app, &key, &model.unwrap_or_else(|| default_model("perplexity").into()), prompt).await,
        other => Err(format!("unknown AI provider {other:?}")),
    }
}

/// Config › AI's Test button: a one-word question, to see the key and the model work.
pub async fn test(app: &App, provider: &str) -> Result<serde_json::Value> {
    if !PROVIDERS.contains(&provider) {
        return Err(Error::invalid(format!("provider must be one of {}", PROVIDERS.join(", "))));
    }
    Ok(match ask_provider(app, provider, "Reply with the single word OK.").await {
        Ok(r) => serde_json::json!({ "ok": true, "model": r.model, "message": r.text.trim().chars().take(80).collect::<String>() }),
        Err(e) => serde_json::json!({ "ok": false, "message": e }),
    })
}

/// An error with every cause under it ("error sending request: ...: invalid peer
/// certificate: UnknownIssuer"), so a certificate, a timeout or a DNS failure says so
/// instead of hiding behind the outermost message.
pub fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        let text = s.to_string();
        if !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        source = s.source();
    }
    out
}

/// Sends, once more on a timeout: a slow first connection is common, a second rarely is.
async fn send(build: impl Fn() -> reqwest::RequestBuilder) -> std::result::Result<(reqwest::StatusCode, serde_json::Value), String> {
    let resp = match build().send().await {
        Err(e) if e.is_timeout() => build().send().await.map_err(|e| error_chain(&e))?,
        other => other.map_err(|e| error_chain(&e))?,
    };
    let status = resp.status();
    let v: serde_json::Value = resp.json().await.map_err(|e| error_chain(&e))?;
    Ok((status, v))
}

/// An error body as a message: the model, the message, and its `type` / `code`.
fn refusal_message(model: &str, status: reqwest::StatusCode, v: &serde_json::Value) -> String {
    let e = &v["error"];
    let message = e["message"].as_str().or_else(|| e.as_str()).map(str::to_string).unwrap_or_else(|| format!("answered {status}"));
    let kind = e["type"].as_str().or_else(|| e["code"].as_str()).unwrap_or("");
    if kind.is_empty() {
        format!("{model}: {message}")
    } else {
        format!("{model}: {message} [{kind}]")
    }
}

fn dedup(sources: Vec<Source>) -> Vec<Source> {
    let mut seen = HashSet::new();
    sources.into_iter().filter(|s| !s.url.is_empty() && seen.insert(s.url.clone())).collect()
}

// -- Gemini ------------------------------------------------------------------

const GEMINI: &str = "https://generativelanguage.googleapis.com/v1beta";

/// The model asked when the key's model list cannot be read (a TLS-intercepting proxy, a
/// timeout): a recent stable Flash.
const GEMINI_FALLBACK: &str = "gemini-2.5-flash";

lazy_static::lazy_static! {
    /// The Gemini model that last answered, for the rest of the process.
    static ref GEMINI_MODEL: Mutex<Option<String>> = Mutex::new(None);
}

/// What went wrong with a request to Gemini.
#[derive(Debug, Clone, PartialEq)]
enum GeminiError {
    Refused { message: String, quota: bool },
    Other(String),
}

impl GeminiError {
    fn message(&self) -> String {
        match self {
            Self::Refused { message, .. } | Self::Other(message) => message.clone(),
        }
    }
}

/// Google's error body as a message: the model, `error.message`, and what `error.details`
/// adds (the quota metric and its limit). A limit of 0 is a key without free quota for that
/// model, which is worth saying in so many words.
fn gemini_refusal(status: reqwest::StatusCode, model: &str, v: &serde_json::Value) -> GeminiError {
    let error = &v["error"];
    let base = error["message"].as_str().map(str::to_string).unwrap_or_else(|| format!("Gemini answered {status}"));
    let mut notes: Vec<String> = Vec::new();
    let mut limit_zero = base.contains("limit: 0");
    for d in error["details"].as_array().into_iter().flatten() {
        for violation in d["violations"].as_array().into_iter().flatten() {
            let metric = violation["quotaMetric"].as_str().or_else(|| violation["quotaId"].as_str()).unwrap_or("");
            let limit = violation["quotaValue"].as_str().map(str::to_string).or_else(|| violation["quotaValue"].as_u64().map(|n| n.to_string()));
            if limit.as_deref() == Some("0") {
                limit_zero = true;
            }
            let text = match (metric, limit) {
                ("", None) => continue,
                (m, Some(l)) => format!("{m} (limit {l})"),
                (m, None) => m.to_string(),
            };
            if !notes.contains(&text) {
                notes.push(text);
            }
        }
        if let Some(reason) = d["reason"].as_str() {
            if !notes.iter().any(|n| n == reason) {
                notes.push(reason.to_string());
            }
        }
    }
    let quota = status == reqwest::StatusCode::TOO_MANY_REQUESTS;
    if quota && limit_zero {
        return GeminiError::Refused { message: format!("this key has no free quota for {model} (limit 0) - use the Ask menu, or a key with billing"), quota };
    }
    let mut message = format!("{model}: {base}");
    if !notes.is_empty() {
        message.push_str(&format!(" [{}]", notes.join("; ")));
    }
    GeminiError::Refused { message, quota }
}

/// The usable Flash models of a `models` list, best first: stable before preview, newest
/// version first, the plain name before a dated one.
fn rank_flash_models(list: &serde_json::Value) -> Vec<String> {
    let version = |name: &str| -> Vec<u32> { name.split(|c: char| !c.is_ascii_digit()).filter_map(|d| d.parse().ok()).take(2).collect() };
    let mut models: Vec<String> = list["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["supportedGenerationMethods"].as_array().is_some_and(|a| a.iter().any(|x| x == "generateContent")))
        .filter_map(|m| m["name"].as_str().map(|n| n.trim_start_matches("models/").to_string()))
        .filter(|n| n.starts_with("gemini-") && n.contains("flash") && !["lite", "image", "tts", "live", "audio", "exp", "thinking", "embedding"].iter().any(|w| n.contains(w)))
        .collect();
    models.sort_by_key(|n| std::cmp::Reverse((!n.contains("preview"), version(n), std::cmp::Reverse(n.len()))));
    models
}

/// The Flash models to ask, best first: the one that last answered in this process, then
/// the newest stable Flash models the key can use. When the list cannot be read,
/// [`GEMINI_FALLBACK`] -- the question is still worth asking.
async fn gemini_models(app: &App, key: &str) -> Vec<String> {
    let mut out: Vec<String> = GEMINI_MODEL.lock().ok().and_then(|m| m.clone()).into_iter().collect();
    match send(|| app.http.get(format!("{GEMINI}/models?pageSize=1000")).header("x-goog-api-key", key).timeout(Duration::from_secs(20))).await {
        Ok((status, list)) if status.is_success() => out.extend(rank_flash_models(&list)),
        Ok((status, list)) => log::info!("gemini: could not list models ({}), asking {GEMINI_FALLBACK}", gemini_refusal(status, "models", &list).message()),
        Err(e) => log::info!("gemini: could not list models ({e}), asking {GEMINI_FALLBACK}"),
    }
    if out.is_empty() {
        out.push(GEMINI_FALLBACK.to_string());
    }
    let mut seen = HashSet::new();
    out.retain(|m| seen.insert(m.clone()));
    out
}

async fn gemini_once(app: &App, key: &str, model: &str, prompt: &str) -> std::result::Result<Reply, GeminiError> {
    let body = serde_json::json!({ "contents": [{ "parts": [{ "text": prompt }] }], "tools": [{ "google_search": {} }] });
    let (status, v) = send(|| app.http.post(format!("{GEMINI}/models/{model}:generateContent")).header("x-goog-api-key", key).json(&body).timeout(Duration::from_secs(60))).await.map_err(|e| GeminiError::Other(format!("{model}: {e}")))?;
    if !status.is_success() {
        return Err(gemini_refusal(status, model, &v));
    }
    let candidate = &v["candidates"][0];
    let text: String = candidate["content"]["parts"].as_array().into_iter().flatten().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("");
    let sources = candidate["groundingMetadata"]["groundingChunks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| Some(Source { title: c["web"]["title"].as_str().unwrap_or("").to_string(), url: c["web"]["uri"].as_str()?.to_string() }))
        .collect();
    if text.trim().is_empty() {
        return Err(GeminiError::Other(format!("{model} gave no answer")));
    }
    let usage = &v["usageMetadata"];
    Ok(Reply {
        provider: "gemini".into(),
        model: model.to_string(),
        text,
        sources: dedup(sources),
        tokens_in: usage["promptTokenCount"].as_u64().unwrap_or(0),
        tokens_out: usage["candidatesTokenCount"].as_u64().unwrap_or(0) + usage["thoughtsTokenCount"].as_u64().unwrap_or(0),
        ..Default::default()
    })
}

/// Gemini with Google Search: the model setting, else the best Flash model -- and on a quota
/// refusal (429) the next one, once. The model that answered is kept for the process.
async fn ask_gemini(app: &App, key: &str, model: Option<String>, prompt: &str) -> std::result::Result<Reply, String> {
    let models = match model {
        Some(m) => vec![m],
        None => gemini_models(app, key).await,
    };
    let mut last: Option<GeminiError> = None;
    for (n, model) in models.iter().enumerate().take(2) {
        match gemini_once(app, key, model, prompt).await {
            Ok(reply) => {
                if let Ok(mut m) = GEMINI_MODEL.lock() {
                    *m = Some(model.clone());
                }
                return Ok(reply);
            }
            Err(e @ GeminiError::Refused { quota: true, .. }) if n == 0 => {
                log::info!("gemini: {} -- trying the next Flash model", e.message());
                last = Some(e);
            }
            Err(e) => return Err(e.message()),
        }
    }
    Err(last.map(|e| e.message()).unwrap_or_else(|| "no Gemini model to ask".to_string()))
}

// -- Claude ------------------------------------------------------------------

/// Models that take the web search tool with dynamic filtering; older ones the basic one.
fn claude_search_tool(model: &str) -> &'static str {
    let dynamic = ["claude-opus-5", "claude-opus-4-8", "claude-opus-4-7", "claude-opus-4-6", "claude-sonnet-5", "claude-sonnet-4-6", "claude-fable"];
    if dynamic.iter().any(|m| model.starts_with(m)) {
        "web_search_20260209"
    } else {
        "web_search_20250305"
    }
}

/// Claude through the Messages API, with the web search tool. Low effort: a short factual
/// answer, not a research project. The current Opus and Sonnet models also take the
/// server-side fallback for a request the safety classifiers decline.
async fn ask_claude(app: &App, key: &str, model: &str, prompt: &str) -> std::result::Result<Reply, String> {
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": 16000,
        "messages": [{ "role": "user", "content": prompt }],
        "tools": [{ "type": claude_search_tool(model), "name": "web_search", "max_uses": 5 }],
    });
    let fallback = ["claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5", "claude-fable-5-1"].iter().any(|m| model == *m);
    if model.starts_with("claude-opus-5") || model.starts_with("claude-sonnet-5") || model.starts_with("claude-fable") || model.starts_with("claude-haiku-5") {
        body["output_config"] = serde_json::json!({ "effort": "low" });
    }
    if fallback {
        body["fallbacks"] = serde_json::json!("default");
    }
    let (status, v) = send(|| {
        let mut r = app.http.post("https://api.anthropic.com/v1/messages").header("x-api-key", key).header("anthropic-version", "2023-06-01").json(&body).timeout(Duration::from_secs(180));
        if fallback {
            r = r.header("anthropic-beta", "server-side-fallback-2026-07-01");
        }
        r
    })
    .await
    .map_err(|e| format!("{model}: {e}"))?;
    if !status.is_success() {
        return Err(refusal_message(model, status, &v));
    }
    parse_claude(model, &v)
}

fn parse_claude(model: &str, v: &serde_json::Value) -> std::result::Result<Reply, String> {
    if v["stop_reason"] == "refusal" {
        let category = v["stop_details"]["category"].as_str().unwrap_or("unspecified");
        return Err(format!("{model} declined to answer ({category})"));
    }
    let mut text = String::new();
    let mut sources = Vec::new();
    for block in v["content"].as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => {
                text.push_str(block["text"].as_str().unwrap_or(""));
                for c in block["citations"].as_array().into_iter().flatten() {
                    if let Some(url) = c["url"].as_str() {
                        sources.push(Source { title: c["title"].as_str().unwrap_or("").to_string(), url: url.to_string() });
                    }
                }
            }
            // An error comes back as an object instead of the list of results.
            Some("web_search_tool_result") => {
                for r in block["content"].as_array().into_iter().flatten() {
                    if let Some(url) = r["url"].as_str() {
                        sources.push(Source { title: r["title"].as_str().unwrap_or("").to_string(), url: url.to_string() });
                    }
                }
            }
            _ => {}
        }
    }
    if text.trim().is_empty() {
        return Err(format!("{model} gave no answer"));
    }
    let usage = &v["usage"];
    Ok(Reply {
        provider: "claude".into(),
        model: v["model"].as_str().unwrap_or(model).to_string(),
        text: text.trim().to_string(),
        sources: dedup(sources),
        tokens_in: usage["input_tokens"].as_u64().unwrap_or(0) + usage["cache_read_input_tokens"].as_u64().unwrap_or(0) + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0),
        tokens_out: usage["output_tokens"].as_u64().unwrap_or(0),
        searches: usage["server_tool_use"]["web_search_requests"].as_u64().unwrap_or(0),
        reported_cost: None,
    })
}

// -- OpenAI ------------------------------------------------------------------

/// OpenAI through the Responses API, with its web search tool.
async fn ask_openai(app: &App, key: &str, model: &str, prompt: &str) -> std::result::Result<Reply, String> {
    let body = serde_json::json!({ "model": model, "input": prompt, "tools": [{ "type": "web_search" }] });
    let (status, v) = send(|| app.http.post("https://api.openai.com/v1/responses").bearer_auth(key).json(&body).timeout(Duration::from_secs(180))).await.map_err(|e| format!("{model}: {e}"))?;
    if !status.is_success() {
        return Err(refusal_message(model, status, &v));
    }
    parse_openai(model, &v)
}

fn parse_openai(model: &str, v: &serde_json::Value) -> std::result::Result<Reply, String> {
    let mut text = String::new();
    let mut sources = Vec::new();
    for item in v["output"].as_array().into_iter().flatten().filter(|i| i["type"] == "message") {
        for part in item["content"].as_array().into_iter().flatten() {
            if part["type"] == "output_text" {
                text.push_str(part["text"].as_str().unwrap_or(""));
                for a in part["annotations"].as_array().into_iter().flatten().filter(|a| a["type"] == "url_citation") {
                    sources.push(Source { title: a["title"].as_str().unwrap_or("").to_string(), url: a["url"].as_str().unwrap_or("").to_string() });
                }
            } else if part["type"] == "refusal" {
                return Err(format!("{model} declined to answer: {}", part["refusal"].as_str().unwrap_or("")));
            }
        }
    }
    if text.trim().is_empty() {
        return Err(format!("{model} gave no answer"));
    }
    let usage = &v["usage"];
    Ok(Reply {
        provider: "openai".into(),
        model: v["model"].as_str().unwrap_or(model).to_string(),
        text: text.trim().to_string(),
        sources: dedup(sources),
        tokens_in: usage["input_tokens"].as_u64().unwrap_or(0),
        tokens_out: usage["output_tokens"].as_u64().unwrap_or(0),
        ..Default::default()
    })
}

// -- Perplexity --------------------------------------------------------------

/// Perplexity's Sonar: search is what it does, so no tool to ask for.
async fn ask_perplexity(app: &App, key: &str, model: &str, prompt: &str) -> std::result::Result<Reply, String> {
    let body = serde_json::json!({ "model": model, "messages": [{ "role": "user", "content": prompt }] });
    let (status, v) = send(|| app.http.post("https://api.perplexity.ai/chat/completions").bearer_auth(key).json(&body).timeout(Duration::from_secs(120))).await.map_err(|e| format!("{model}: {e}"))?;
    if !status.is_success() {
        return Err(refusal_message(model, status, &v));
    }
    parse_perplexity(model, &v)
}

fn parse_perplexity(model: &str, v: &serde_json::Value) -> std::result::Result<Reply, String> {
    let text = v["choices"][0]["message"]["content"].as_str().unwrap_or("").trim().to_string();
    if text.is_empty() {
        return Err(format!("{model} gave no answer"));
    }
    let mut sources: Vec<Source> = v["search_results"].as_array().into_iter().flatten().filter_map(|r| Some(Source { title: r["title"].as_str().unwrap_or("").to_string(), url: r["url"].as_str()?.to_string() })).collect();
    sources.extend(v["citations"].as_array().into_iter().flatten().filter_map(|u| u.as_str()).map(|u| Source { title: String::new(), url: u.to_string() }));
    let usage = &v["usage"];
    Ok(Reply {
        provider: "perplexity".into(),
        model: v["model"].as_str().unwrap_or(model).to_string(),
        text,
        sources: dedup(sources),
        tokens_in: usage["prompt_tokens"].as_u64().unwrap_or(0),
        tokens_out: usage["completion_tokens"].as_u64().unwrap_or(0),
        reported_cost: usage["cost"]["total_cost"].as_f64(),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_language_is_one_line_under_the_question() {
        assert_eq!(language_line(Some("ja")).as_deref(), Some("Answer in Japanese."));
        assert_eq!(language_line(Some("pt")).as_deref(), Some("Answer in European Portuguese."));
        assert_eq!(language_line(Some("de-DE")).as_deref(), Some("Answer in German."));
        assert_eq!(language_line(Some("xx")), None);
        for code in ["en", "ja", "fr", "es", "it", "pt", "de", "ko", "pl"] {
            assert!(language_line(Some(code)).is_some(), "{code}");
        }
        assert_eq!(with_language("What differs?", Some("ko")), "What differs?\nAnswer in Korean.");
        assert_eq!(with_language("What differs?", None), "What differs?");
    }

    #[test]
    fn the_ask_menu_puts_the_chosen_link_first_and_carries_the_question() {
        let links = ask_links("What differs?\nAnswer in Japanese.", "claude");
        assert_eq!(links.iter().map(|l| l.id).collect::<Vec<_>>(), vec!["claude", "google", "chatgpt", "perplexity"]);
        assert!(links[0].url.starts_with("https://claude.ai/new?q=What+differs%3F%0AAnswer+in+Japanese."), "{}", links[0].url);
        assert!(links[1].url.ends_with("&udm=50"));
        assert!(links[2].url.starts_with("https://chatgpt.com/?q="));
        assert!(links[3].url.starts_with("https://www.perplexity.ai/search?q="));
        assert_eq!(ask_links("q", "nonsense")[0].id, "google");
    }

    #[test]
    fn a_key_without_free_quota_is_named_as_such() {
        let body = serde_json::json!({ "error": { "code": 429, "message": "You exceeded your current quota.", "details": [{ "@type": "type.googleapis.com/google.rpc.QuotaFailure", "violations": [{ "quotaMetric": "generativelanguage.googleapis.com/generate_content_free_tier_requests", "quotaValue": "0" }] }] } });
        let e = gemini_refusal(reqwest::StatusCode::TOO_MANY_REQUESTS, "gemini-x-flash", &body);
        assert!(matches!(&e, GeminiError::Refused { quota: true, .. }), "{e:?}");
        assert!(e.message().contains("no free quota for gemini-x-flash (limit 0)"), "{}", e.message());
    }

    #[test]
    fn flash_models_rank_stable_and_newest_first() {
        let list = serde_json::json!({ "models": [
            { "name": "models/gemini-1-flash", "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/gemini-2-flash", "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/gemini-3-flash-preview", "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/gemini-2-flash-lite", "supportedGenerationMethods": ["generateContent"] },
        ] });
        assert_eq!(rank_flash_models(&list), vec!["gemini-2-flash", "gemini-1-flash", "gemini-3-flash-preview"]);
    }

    #[test]
    fn each_provider_answer_is_read_with_its_sources_and_tokens() {
        let claude = serde_json::json!({ "model": "claude-x", "stop_reason": "end_turn", "content": [
            { "type": "server_tool_use", "id": "a", "name": "web_search", "input": { "query": "q" } },
            { "type": "web_search_tool_result", "tool_use_id": "a", "content": [{ "type": "web_search_result", "url": "https://example.invalid/a", "title": "A" }] },
            { "type": "text", "text": "It differs.", "citations": [{ "type": "web_search_result_location", "url": "https://example.invalid/a", "title": "A", "cited_text": "..." }] },
        ], "usage": { "input_tokens": 100, "output_tokens": 20, "server_tool_use": { "web_search_requests": 2 } } });
        let r = parse_claude("claude-x", &claude).unwrap();
        assert_eq!((r.text.as_str(), r.sources.len(), r.tokens_in, r.tokens_out, r.searches), ("It differs.", 1, 100, 20, 2));
        assert!(parse_claude("claude-x", &serde_json::json!({ "stop_reason": "refusal", "stop_details": { "category": "cyber" }, "content": [] })).unwrap_err().contains("declined"));

        let openai = serde_json::json!({ "model": "gpt-x", "output": [
            { "type": "web_search_call", "status": "completed" },
            { "type": "message", "content": [{ "type": "output_text", "text": "It differs.", "annotations": [{ "type": "url_citation", "url": "https://example.invalid/b", "title": "B" }] }] },
        ], "usage": { "input_tokens": 50, "output_tokens": 10 } });
        let r = parse_openai("gpt-x", &openai).unwrap();
        assert_eq!((r.text.as_str(), r.sources[0].url.as_str(), r.tokens_in), ("It differs.", "https://example.invalid/b", 50));

        let pplx = serde_json::json!({ "model": "sonar", "choices": [{ "message": { "content": "It differs." } }], "search_results": [{ "title": "C", "url": "https://example.invalid/c" }], "citations": ["https://example.invalid/c", "https://example.invalid/d"], "usage": { "prompt_tokens": 9, "completion_tokens": 3, "cost": { "total_cost": 0.006 } } });
        let r = parse_perplexity("sonar", &pplx).unwrap();
        assert_eq!((r.sources.len(), r.reported_cost), (2, Some(0.006)));
    }

    #[test]
    fn cost_uses_the_longest_matching_price_and_the_search_fee() {
        let table: BTreeMap<String, Price> = PRICES.iter().map(|(m, i, o)| (m.to_string(), Price { input: *i, output: *o })).collect();
        assert_eq!(price_of(&table, "gpt-5-mini-2026").unwrap().input, 0.25);
        assert_eq!(price_of(&table, "gpt-5.1").unwrap().input, 1.25);
        assert!(price_of(&table, "unknown-model").is_none());
        let reply = Reply { provider: "claude".into(), model: "claude-sonnet-5-5".into(), tokens_in: 1_000_000, tokens_out: 100_000, searches: 3, ..Default::default() };
        let c = cost(&table, &reply).unwrap();
        assert!((c - (2.0 + 1.0 + 0.03)).abs() < 1e-9, "{c}");
        assert_eq!(cost(&table, &Reply { reported_cost: Some(0.5), ..reply }), Some(0.5));
    }
}
