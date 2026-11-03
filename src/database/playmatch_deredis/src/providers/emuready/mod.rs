use crate::config::http::REQWEST_DEFAULT_USER_AGENT;
use crate::http::abstraction::RetryPolicy;
use crate::providers::emuready::model::{
	EmuReadyEnvelope, EmuReadyGame, EmuReadyGamesPage, EmuReadySystem,
};
use anyhow::{Context, anyhow};
use log::debug;
use reqwest::header::HeaderMap;
use reqwest::{Client, Method, StatusCode, Url};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, OnceCell};
use tower::limit::{RateLimit, RateLimitLayer};
use tower::retry::Retry;
use tower::{Service, ServiceBuilder, ServiceExt};

pub mod model;

pub const API_URL: &str = "https://www.emuready.com/api/mobile/trpc";

const RATELIMIT_AMOUNT: u64 = 4;
const RATELIMIT_DURATION_MS: u64 = 1000;
const SEARCH_LIMIT: i64 = 50;

pub struct EmuReadyClient {
	client: Client,
	service: Mutex<RateLimit<Retry<RetryPolicy, Client>>>,
	systems_cache: OnceCell<Arc<Vec<EmuReadySystem>>>,
}

impl EmuReadyClient {
	pub fn new(
		client: Client,
	) -> anyhow::Result<Self> {
		let rate_limit_layer = RateLimitLayer::new(
			RATELIMIT_AMOUNT,
			Duration::from_millis(RATELIMIT_DURATION_MS),
		);
		let retry_layer = tower::retry::RetryLayer::new(RetryPolicy::new("emuready"));

		let service = ServiceBuilder::new()
			.layer(rate_limit_layer)
			.layer(retry_layer)
			.service(client.clone());

		crate::metrics::set_provider_concurrency_configured(
			"emuready",
			crate::providers::DEFAULT_CHUNK_SIZE as i64,
		);

		Ok(Self {
			client,
			service: Mutex::new(service),
			systems_cache: OnceCell::new(),
		})
	}

	pub async fn list_systems(&self) -> anyhow::Result<Arc<Vec<EmuReadySystem>>> {
		self.systems_cache
			.get_or_try_init(|| async {
				let url = build_url("general.systems", None)?;
				let env = self
					.execute_get::<EmuReadyEnvelope<Vec<EmuReadySystem>>>(url, "systems")
					.await?;
				Ok(Arc::new(env.result.data.json))
			})
			.await
			.cloned()
	}

	pub async fn search_games(
		&self,
		system_id: &str,
		query: &str,
	) -> anyhow::Result<Vec<EmuReadyGame>> {
		#[derive(Serialize)]
		struct Input<'a> {
			search: &'a str,
			#[serde(rename = "systemId")]
			system_id: &'a str,
			page: i64,
			limit: i64,
		}
		let payload = Input {
			search: query,
			system_id,
			page: 1,
			limit: SEARCH_LIMIT,
		};
		let input = build_trpc_input(&payload)?;
		let url = build_url("games.get", Some(&input))?;
		let env = self
			.execute_get::<EmuReadyEnvelope<EmuReadyGamesPage>>(url, "games_get")
			.await?;
		Ok(env.result.data.json.games)
	}

	/// romexplorer addition: one page of every game on a system (`games.get` without a
	/// search term), for the per-platform bulk sync. The response carries no total, so a
	/// page shorter than `limit` (max 50) is the last one.
	pub async fn list_games_page(
		&self,
		system_id: &str,
		page: i64,
		limit: i64,
	) -> anyhow::Result<Vec<EmuReadyGame>> {
		#[derive(Serialize)]
		struct Input<'a> {
			#[serde(rename = "systemId")]
			system_id: &'a str,
			page: i64,
			limit: i64,
		}
		let payload = Input {
			system_id,
			page,
			limit: limit.clamp(1, SEARCH_LIMIT),
		};
		let input = build_trpc_input(&payload)?;
		let url = build_url("games.get", Some(&input))?;
		let env = self
			.execute_get::<EmuReadyEnvelope<EmuReadyGamesPage>>(url, "games_list")
			.await?;
		Ok(env.result.data.json.games)
	}

	/// romexplorer addition: how many games a system has (`pagination.total` of a one-item
	/// page) -- the cheap "did it change?" check before paging through it.
	pub async fn count_games(&self, system_id: &str) -> anyhow::Result<Option<i64>> {
		#[derive(Serialize)]
		struct Input<'a> {
			#[serde(rename = "systemId")]
			system_id: &'a str,
			page: i64,
			limit: i64,
		}
		let input = build_trpc_input(&Input { system_id, page: 1, limit: 1 })?;
		let url = build_url("games.get", Some(&input))?;
		let env = self.execute_get::<EmuReadyEnvelope<EmuReadyGamesPage>>(url, "games_count").await?;
		Ok(env.result.data.json.pagination.and_then(|p| p.total))
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
			.client
			.request(Method::GET, url)
			.headers(headers)
			.build()?;

		debug!("EmuReady request: {} {}", req.method(), url_for_log.path());

		let started = std::time::Instant::now();
		let _inflight = crate::http::abstraction::InflightGuard::new("emuready");
		let mut observed_code: Option<u16> = None;
		let result: anyhow::Result<T> = async {
			let rate_limited_future = self.service.lock().await.ready().await?.call(req);
			let res = rate_limited_future.await?;
			let status = res.status();
			observed_code = Some(status.as_u16());
			let body = res.text().await?;
			if !status.is_success() {
				return Err(anyhow!(
					"emuready returned non-success status {status} from {url_for_log}"
				));
			}
			if status == StatusCode::NO_CONTENT || body.is_empty() {
				return Err(anyhow!(
					"emuready returned empty body on success from {url_for_log}"
				));
			}
			serde_json::from_str::<T>(&body)
				.with_context(|| format!("failed to parse emuready response from {url_for_log}"))
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
			"emuready",
			endpoint_label,
			status_class,
			&status_code,
			started.elapsed().as_secs_f64(),
		);
		result
	}
}

fn build_url(procedure: &str, input: Option<&str>) -> anyhow::Result<Url> {
	let mut url = Url::parse(API_URL)?;
	url.path_segments_mut()
		.map_err(|_| anyhow!("emuready base url cannot have path segments"))?
		.push(procedure);
	if let Some(input) = input {
		url.query_pairs_mut().append_pair("input", input);
	}
	Ok(url)
}

// tRPC GET procedures take their input as a JSON-encoded query param wrapped in {"json": ...}.
fn build_trpc_input<T: Serialize>(payload: &T) -> anyhow::Result<String> {
	Ok(serde_json::to_string(
		&serde_json::json!({ "json": payload }),
	)?)
}

