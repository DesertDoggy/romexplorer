//! No-op stand-in for playmatch's real `metrics.rs` (838 lines of prometheus counter/
//! histogram registration and recording). The vendored provider clients call these
//! functions for pure observability -- none of their actual request/response logic depends
//! on the results, so every function here is a deliberate no-op rather than a real prometheus
//! wire-up. Signatures are copied verbatim from upstream so the provider client files don't
//! need touching. Add real metrics later (this project's own choice of exporter, if any)
//! without changing any call site.

pub fn record_cache_hit(_cache: &str, _lookup: &str) {}
pub fn set_cache_l1_entries(_lookup: &str, _entries: u64) {}
pub fn record_cache_miss(_cache: &str, _lookup: &str) {}
pub fn record_identify_attempt(_hash_type: &str, _hit: bool) {}
pub fn record_service_error(_variant: &str) {}
pub fn record_metadata_auto_match(_provider: &str, _entity_type: &str, _result: &str, _reason: &str) {}
pub fn record_match_rung(_provider: &str, _rung: &str, _outcome: &str) {}
pub fn record_metadata_token_refresh(_provider: &str, _trigger: &str, _result: &str) {}
pub fn record_background_job(_job: &str, _result: &str, _duration_seconds: f64) {}
pub fn record_metadata_request(_provider: &str, _path: &str, _status_class: &str, _status_code: &str, _duration_seconds: f64) {}
pub fn record_dat_ingestion_file(_source: &str, _outcome: &str) {}
pub fn record_clone_of_resolution(_result: &str) {}
pub fn record_user_action(_entity_type: &str, _action: &str) {}
pub fn record_user_agent(_product: &str, _version: &str) {}
pub fn record_launchbox_import_records(_record_type: &str, _outcome: &str, _n: u64) {}
pub fn record_openvgdb_import_records(_record_type: &str, _outcome: &str, _n: u64) {}
pub fn record_retroachievements_import_records(_record_type: &str, _outcome: &str, _n: u64) {}
pub fn record_screenscraper_quota_exhaustion(_trigger: &str) {}
pub fn metadata_request_inflight_inc(_provider: &str) {}
pub fn metadata_request_inflight_dec(_provider: &str) {}
pub fn record_metadata_request_attempt(_provider: &str, _outcome: &str) {}
pub fn set_provider_concurrency_configured(_provider: &str, _value: i64) {}
pub fn record_cross_match_attempt(_provider: &str, _outcome: &str) {}
pub fn set_thegamesdb_cycle_calls(_value: u32) {}
pub fn set_thegamesdb_remaining_allowance(_value: i32) {}
pub fn set_external_suggestion_queue_depth(_depth: i64) {}
pub fn record_http_rate_limit_rejected(_client_classification: &str) {}
pub fn http_requests_inflight_inc(_route: &str, _method: &str) {}
pub fn http_requests_inflight_dec(_route: &str, _method: &str) {}
pub fn set_db_pool_connections(_state: &str, _value: i64) {}
pub fn observe_identify_latency(_result: &str, _duration_seconds: f64) {}
pub fn record_identify_hit_position(_hash_type: &str) {}
pub fn record_auth_attempt(_outcome: &str) {}
pub fn observe_auth_latency(_outcome: &str, _duration_seconds: f64) {}
pub fn record_upstream_unavailable(_provider: &str) {}
pub fn record_builder_error(_builder: &str, _field: &str) {}
pub fn record_thegamesdb_api_call(_outcome: &str) {}
pub fn record_bulk_identify_item(_outcome: &str) {}
pub fn observe_bulk_identify_batch_size(_endpoint: &str, _size: usize) {}
pub fn record_bulk_by_id_item(_resource: &str, _outcome: &str) {}
pub fn observe_bulk_by_id_batch_size(_resource: &str, _size: usize) {}
