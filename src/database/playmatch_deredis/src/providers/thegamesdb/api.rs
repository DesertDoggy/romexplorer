use crate::config::http::REQWEST_DEFAULT_USER_AGENT;
use crate::providers::thegamesdb::TheGamesDbClient;
use crate::providers::thegamesdb::model::{
	TgdbApiGame, TgdbCountriesData, TgdbEnvelope, TgdbGameByIdData, TgdbImagesData, TgdbNamed, TgdbPlatform, TgdbPlatformsData, TgdbRegionsData, TgdbSearchData,
};
use anyhow::{Context, anyhow};
use log::debug;
use reqwest::header::HeaderMap;
use reqwest::{Method, StatusCode, Url};
use serde::de::DeserializeOwned;
use std::sync::atomic::Ordering;
use std::time::Instant;

pub const API_BASE: &str = "https://api.thegamesdb.net";

/// Returned by [`TheGamesDbClient::search_by_name`] when the per-cycle cap or
/// the cached remaining-allowance prevented the call. Callers treat this as
/// "no candidates", same as an empty hit, but the matcher uses the variant
/// to write a distinct rung outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchOutcome {
	Hit,
	Miss,
	QuotaExhausted,
}

pub struct SearchResult {
	pub outcome: SearchOutcome,
	pub games: Vec<TgdbApiGame>,
}

impl TheGamesDbClient {
	pub async fn search_by_name(&self, name: &str) -> anyhow::Result<SearchResult> {
		let Some(api_key) = self.api_key.as_deref() else {
			return Ok(SearchResult {
				outcome: SearchOutcome::Miss,
				games: vec![],
			});
		};

		if !self.try_acquire_cycle_slot() {
			record_outcome("quota_exhausted");
			return Ok(SearchResult {
				outcome: SearchOutcome::QuotaExhausted,
				games: vec![],
			});
		}

		let remaining = self.load_remaining_allowance(api_key).await?;
		if remaining <= 0 {
			record_outcome("quota_exhausted");
			return Ok(SearchResult {
				outcome: SearchOutcome::QuotaExhausted,
				games: vec![],
			});
		}

		let mut url = Url::parse(&format!("{API_BASE}/v1/Games/ByGameName"))?;
		url.query_pairs_mut()
			.append_pair("apikey", api_key)
			.append_pair("name", name)
			.append_pair("fields", "")
			.append_pair("include", "alternates");

		let env: TgdbEnvelope<TgdbSearchData> = self
			.execute_get(url, "search_by_name")
			.await
			.inspect_err(|_| {
				record_outcome("error");
			})?;

		self.refresh_remaining_allowance(&env).await;

		let games = env.data.games;
		let outcome = if games.is_empty() {
			record_outcome("miss");
			SearchOutcome::Miss
		} else {
			record_outcome("hit");
			SearchOutcome::Hit
		};
		Ok(SearchResult { outcome, games })
	}

	/// romexplorer addition: every platform TheGamesDB knows (`/v1/Platforms`), for mapping
	/// its platform ids. One call of the monthly allowance -- cache the result.
	pub async fn platforms(&self) -> anyhow::Result<Vec<TgdbPlatform>> {
		let api_key = self.api_key.as_deref().ok_or_else(|| anyhow!("thegamesdb: no api key"))?;
		let mut url = Url::parse(&format!("{API_BASE}/v1/Platforms"))?;
		url.query_pairs_mut().append_pair("apikey", api_key);
		let env: TgdbEnvelope<TgdbPlatformsData> = self.execute_get(url, "platforms").await?;
		self.refresh_remaining_allowance(&env).await;
		Ok(env.data.platforms.into_values().collect())
	}

	/// romexplorer addition: `/v1/Regions`, id -> name. One call of the monthly allowance --
	/// cache the result.
	pub async fn regions(&self) -> anyhow::Result<Vec<TgdbNamed>> {
		let api_key = self.api_key.as_deref().ok_or_else(|| anyhow!("thegamesdb: no api key"))?;
		let mut url = Url::parse(&format!("{API_BASE}/v1/Regions"))?;
		url.query_pairs_mut().append_pair("apikey", api_key);
		let env: TgdbEnvelope<TgdbRegionsData> = self.execute_get(url, "regions").await?;
		self.refresh_remaining_allowance(&env).await;
		Ok(env.data.regions.into_values().collect())
	}

	/// romexplorer addition: `/v1/Countries`, id -> name. One call -- cache the result.
	pub async fn countries(&self) -> anyhow::Result<Vec<TgdbNamed>> {
		let api_key = self.api_key.as_deref().ok_or_else(|| anyhow!("thegamesdb: no api key"))?;
		let mut url = Url::parse(&format!("{API_BASE}/v1/Countries"))?;
		url.query_pairs_mut().append_pair("apikey", api_key);
		let env: TgdbEnvelope<TgdbCountriesData> = self.execute_get(url, "countries").await?;
		self.refresh_remaining_allowance(&env).await;
		Ok(env.data.countries.into_values().collect())
	}

	/// romexplorer addition: one page of `/v1/Games/Images` for several games at once
	/// (`types`: `boxart`, `titlescreen`, `screenshot`, `clearlogo`, `fanart`, `banner`), and
	/// whether another page follows. One call of the monthly allowance per page, whatever the
	/// number of games; refuses once the known allowance is used up.
	pub async fn games_images(&self, ids: &[i64], types: &[&str], page: u32) -> anyhow::Result<(TgdbImagesData, bool)> {
		let api_key = self.api_key.as_deref().ok_or_else(|| anyhow!("thegamesdb: no api key"))?;
		if self.load_remaining_allowance(api_key).await? <= 0 {
			return Err(anyhow!("thegamesdb: monthly API allowance used up"));
		}
		let mut url = Url::parse(&format!("{API_BASE}/v1/Games/Images"))?;
		{
			let mut q = url.query_pairs_mut();
			q.append_pair("apikey", api_key).append_pair("games_id", &ids.iter().map(i64::to_string).collect::<Vec<_>>().join(","));
			if !types.is_empty() {
				q.append_pair("filter[type]", &types.join(","));
			}
			if page > 1 {
				q.append_pair("page", &page.to_string());
			}
		}
		let env: TgdbEnvelope<TgdbImagesData> = self.execute_get(url, "games_images").await?;
		self.refresh_remaining_allowance(&env).await;
		let more = env.pages.as_ref().and_then(|p| p.next.as_ref()).is_some_and(|n| !n.is_empty());
		Ok((env.data, more))
	}

	/// romexplorer addition: start from an allowance known from before (saved by the app),
	/// so a fresh process does not spend a call probing for it. Ignored once a call has
	/// reported the real figure.
	pub fn seed_remaining_allowance(&self, value: i32) {
		let _ = self.remaining_allowance.compare_exchange(i32::MIN, value, Ordering::Relaxed, Ordering::Relaxed);
	}

	/// romexplorer addition: one page (20 games) of `/v1/Games/ByPlatformID`, and whether
	/// another page follows. Each call uses the monthly allowance (not the per-cycle search
	/// cap); refuses once the known allowance is used up.
	pub async fn games_by_platform_page(&self, platform_id: i64, page: u32) -> anyhow::Result<(Vec<TgdbApiGame>, bool)> {
		let api_key = self.api_key.as_deref().ok_or_else(|| anyhow!("thegamesdb: no api key"))?;
		if self.load_remaining_allowance(api_key).await? <= 0 {
			return Err(anyhow!("thegamesdb: monthly API allowance used up"));
		}
		let mut url = Url::parse(&format!("{API_BASE}/v1/Games/ByPlatformID"))?;
		url.query_pairs_mut()
			.append_pair("apikey", api_key)
			.append_pair("id", &platform_id.to_string())
			.append_pair("fields", "alternates")
			.append_pair("page", &page.to_string());
		let env: TgdbEnvelope<TgdbSearchData> = self.execute_get(url, "games_by_platform").await?;
		self.refresh_remaining_allowance(&env).await;
		let more = env.pages.as_ref().and_then(|p| p.next.as_ref()).is_some_and(|n| !n.is_empty());
		Ok((env.data.games, more))
	}

	/// romexplorer addition: the last known remaining monthly allowance (incl. extra), if any
	/// call has reported it yet.
	pub fn known_remaining_allowance(&self) -> Option<i32> {
		let v = self.remaining_allowance.load(Ordering::Relaxed);
		(v != i32::MIN).then_some(v)
	}

	/// Probe the API for the current `remaining_monthly_allowance`. Used to
	/// rehydrate the Redis cache when it has expired or been flushed. Hits the
	/// smallest documented endpoint (`Games/ByGameID` for id 1 with empty
	/// fields) and returns the allowance reported in the response envelope.
	async fn probe_remaining_allowance(&self, api_key: &str) -> anyhow::Result<i32> {
		let mut url = Url::parse(&format!("{API_BASE}/v1/Games/ByGameID"))?;
		url.query_pairs_mut()
			.append_pair("apikey", api_key)
			.append_pair("id", "1")
			.append_pair("fields", "");

		let env: TgdbEnvelope<TgdbGameByIdData> = self.execute_get(url, "probe").await?;
		let remaining =
			env.remaining_monthly_allowance.unwrap_or(0) + env.extra_allowance.unwrap_or(0);
		self.write_remaining_allowance(remaining).await;
		record_outcome("probe");
		Ok(remaining)
	}

	/// De-redis: upstream falls back to a Redis-cached value between the in-process atomic
	/// and probing the API live, so the allowance survives a restart without an extra API
	/// call. Without Redis, a restart always re-probes once (one extra API call, not a
	/// correctness issue) instead of restoring the last known value. See patches/ for the
	/// exact upstream diff this removes.
	async fn load_remaining_allowance(&self, api_key: &str) -> anyhow::Result<i32> {
		let cached = self.remaining_allowance.load(Ordering::Relaxed);
		if cached != i32::MIN {
			return Ok(cached);
		}
		self.probe_remaining_allowance(api_key).await
	}

	async fn refresh_remaining_allowance<T>(&self, env: &TgdbEnvelope<T>) {
		if let Some(remaining) = env.remaining_monthly_allowance {
			let total = remaining + env.extra_allowance.unwrap_or(0);
			self.write_remaining_allowance(total).await;
		}
	}

	/// De-redis: see `load_remaining_allowance` -- the in-process atomic above is now the
	/// only store; upstream's Redis write-through (for cross-restart persistence) is removed.
	async fn write_remaining_allowance(&self, value: i32) {
		self.remaining_allowance.store(value, Ordering::Relaxed);
		crate::metrics::set_thegamesdb_remaining_allowance(value);
	}

	fn try_acquire_cycle_slot(&self) -> bool {
		let prev = self.per_cycle_calls.fetch_add(1, Ordering::Relaxed);
		if prev >= Self::PER_CYCLE_CAP {
			self.per_cycle_calls
				.store(Self::PER_CYCLE_CAP, Ordering::Relaxed);
			crate::metrics::set_thegamesdb_cycle_calls(Self::PER_CYCLE_CAP);
			false
		} else {
			crate::metrics::set_thegamesdb_cycle_calls(prev + 1);
			true
		}
	}

	async fn execute_get<T: DeserializeOwned>(
		&self,
		url: Url,
		endpoint_label: &'static str,
	) -> anyhow::Result<T> {
		let mut headers = HeaderMap::new();
		headers.insert("User-Agent", REQWEST_DEFAULT_USER_AGENT.parse()?);
		headers.insert("Accept", "application/json".parse()?);

		let url_for_log = url.clone();
		let req = self
			.http
			.request(Method::GET, url)
			.headers(headers)
			.build()?;

		debug!(
			"TheGamesDB request: {} {}",
			req.method(),
			url_for_log.path()
		);

		let started = Instant::now();
		let _inflight = crate::http::abstraction::InflightGuard::new("thegamesdb");
		let mut observed_code: Option<u16> = None;
		let result: anyhow::Result<T> = async {
			let res = self.execute(req).await?;
			let status = res.status();
			observed_code = Some(status.as_u16());
			let body = res.text().await?;
			if !status.is_success() {
				return Err(anyhow!(
					"thegamesdb returned non-success status {status} from {url_for_log}"
				));
			}
			if status == StatusCode::NO_CONTENT || body.is_empty() {
				return Err(anyhow!(
					"thegamesdb returned empty body on success from {url_for_log}"
				));
			}
			serde_json::from_str::<T>(&body)
				.with_context(|| format!("failed to parse thegamesdb response from {url_for_log}"))
		}
		.await;

		let (status_class, status_code) = match observed_code {
			Some(code) => (
				crate::http::abstraction::classify_status(code),
				code.to_string(),
			),
			None => ("network_error", "none".to_string()),
		};
		crate::metrics::record_metadata_request(
			"thegamesdb",
			endpoint_label,
			status_class,
			&status_code,
			started.elapsed().as_secs_f64(),
		);
		result
	}
}

fn record_outcome(outcome: &str) {
	crate::metrics::record_thegamesdb_api_call(outcome);
}
