use crate::http::abstraction::RetryPolicy;
use reqwest::{Client, Request, Response};
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use tokio::sync::Mutex;
use tower::retry::Retry;
use tower::{Service, ServiceBuilder, ServiceExt};

pub mod api;
pub mod model;

/// Hard cap on TGDB API search calls per match cycle. Combined with the daily
/// cron and 1000-request-per-month free tier, this keeps monthly usage under
/// the 1000 cap with a comfortable buffer.
const PER_CYCLE_CAP_DEFAULT: u32 = 30;
const REMAINING_CACHE_TTL_SECS_DEFAULT: u64 = 24 * 60 * 60;
const REMAINING_CACHE_KEY_DEFAULT: &str = "playmatch:tgdb:remaining_monthly_allowance";

pub struct TheGamesDbClient {
	http: Client,
	service: Mutex<Retry<RetryPolicy, Client>>,
	api_key: Option<String>,
	per_cycle_calls: AtomicU32,
	/// Mirror of TheGamesDB's `remaining_monthly_allowance` for the current
	/// account. `i32::MIN` means "unknown, fetch from Redis or probe".
	remaining_allowance: AtomicI32,
}

impl TheGamesDbClient {
	pub const PER_CYCLE_CAP: u32 = PER_CYCLE_CAP_DEFAULT;
	pub const REMAINING_CACHE_TTL_SECS: u64 = REMAINING_CACHE_TTL_SECS_DEFAULT;
	pub const REMAINING_CACHE_KEY: &'static str = REMAINING_CACHE_KEY_DEFAULT;

	pub fn new(http: Client, api_key: Option<String>) -> anyhow::Result<Self> {
		let retry_layer = tower::retry::RetryLayer::new(RetryPolicy::new("thegamesdb"));
		let service = ServiceBuilder::new()
			.layer(retry_layer)
			.service(http.clone());

		crate::metrics::set_provider_concurrency_configured(
			"thegamesdb",
			crate::providers::DEFAULT_CHUNK_SIZE as i64,
		);

		Ok(Self {
			http,
			service: Mutex::new(service),
			api_key,
			per_cycle_calls: AtomicU32::new(0),
			remaining_allowance: AtomicI32::new(i32::MIN),
		})
	}

	pub(crate) async fn execute(&self, req: Request) -> reqwest::Result<Response> {
		let mut svc = self.service.lock().await;
		let svc = svc.ready().await?;
		svc.call(req).await
	}

	pub fn has_api_key(&self) -> bool {
		self.api_key.is_some()
	}

	pub(crate) fn reset_cycle_counter(&self) {
		self.per_cycle_calls.store(0, Ordering::Relaxed);
		crate::metrics::set_thegamesdb_cycle_calls(0);
	}
}

