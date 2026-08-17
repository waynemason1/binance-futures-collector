use anyhow::{bail, Context, Result};
use dashmap::DashMap;
use log::{debug, warn};
use parking_lot::Mutex;
use reqwest::{Client, ClientBuilder, Proxy};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::{sleep, timeout};

use crate::config::Config;
use crate::models::*;
use crate::rest::proxy_manager::{ProxyEndpoint, ProxyManager};
use crate::rest::proxy_usage_tracker::{ProxyUsageRecord, ProxyUsageTracker};

/// Shared circuit breaker. On any 429/418 (or a near-ceiling used-weight header),
/// it is tripped so *every* limiter (`/fapi` and `/futures/data` alike) pauses
/// together, instead of the other tasks hammering Binance into a longer 418 ban.
struct CircuitBreaker {
    blocked_until: AtomicU64, // nanos since `start`; 0 = open
    start: Instant,
}

impl CircuitBreaker {
    fn new() -> Self {
        Self {
            blocked_until: AtomicU64::new(0),
            start: Instant::now(),
        }
    }

    fn now_nanos(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }

    fn trip(&self, dur: Duration) {
        let until = self.now_nanos().saturating_add(dur.as_nanos() as u64);
        let mut cur = self.blocked_until.load(Ordering::Relaxed);
        while until > cur {
            match self.blocked_until.compare_exchange(
                cur,
                until,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(c) => cur = c,
            }
        }
    }

    /// Nanos to wait if currently blocked, else `None`.
    fn blocked_wait(&self) -> Option<u64> {
        let now = self.now_nanos();
        let b = self.blocked_until.load(Ordering::Relaxed);
        if b > now {
            Some(b - now)
        } else {
            None
        }
    }
}

/// A rolling-window rate limiter sharing a `CircuitBreaker`. Binance enforces TWO
/// independent per-IP limits, so we run one of these per endpoint group:
///   * `/fapi/*`         — a WEIGHT budget over 60s (depth=2, most others=1)
///   * `/futures/data/*` — a request-COUNT budget over 5min (its own tighter limit)
struct RateLimiter {
    window: Mutex<VecDeque<(u64, u32)>>, // (nanos_since_start, weight)
    max_per_window: u64,
    window_nanos: u64,
    breaker: Arc<CircuitBreaker>,
}

impl RateLimiter {
    fn new(max_per_window: u64, window_nanos: u64, breaker: Arc<CircuitBreaker>) -> Self {
        Self {
            window: Mutex::new(VecDeque::new()),
            max_per_window: max_per_window.max(1),
            window_nanos,
            breaker,
        }
    }

    /// Block until a request of `weight` fits the rolling budget and the breaker
    /// is open. No lock is held across an `.await`.
    async fn acquire(&self, weight: u32) {
        loop {
            if let Some(wait) = self.breaker.blocked_wait() {
                sleep(Duration::from_nanos(wait)).await;
                continue;
            }
            let wait_nanos = {
                let mut w = self.window.lock();
                let now = self.breaker.now_nanos();
                let cutoff = now.saturating_sub(self.window_nanos);
                while let Some(&(t, _)) = w.front() {
                    if t < cutoff {
                        w.pop_front();
                    } else {
                        break;
                    }
                }
                let sum: u64 = w.iter().map(|&(_, wt)| wt as u64).sum();
                // Admit when it fits, or when the window is already empty; a
                // single request whose weight exceeds the whole budget can't be
                // made to fit by waiting, so let it through alone rather than
                // spin forever (misconfigured max_weight < one request's weight).
                if sum + weight as u64 <= self.max_per_window || w.is_empty() {
                    w.push_back((now, weight));
                    0
                } else {
                    let front_t = w.front().map(|&(t, _)| t).unwrap_or(now);
                    front_t
                        .saturating_add(self.window_nanos)
                        .saturating_sub(now)
                        .max(1_000_000)
                }
            };
            if wait_nanos == 0 {
                return;
            }
            sleep(Duration::from_nanos(wait_nanos)).await;
        }
    }
}

/// A 429 (rate limited) or 418 (IP banned) response from Binance, carried as a
/// typed error so the retry layer can react without string-matching.
#[derive(Debug)]
struct RateLimited {
    status: u16,
    retry_after: Option<Duration>,
    banned: bool,
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HTTP {} rate limited (retry_after={:?}, banned={})",
            self.status, self.retry_after, self.banned
        )
    }
}
impl std::error::Error for RateLimited {}

/// Binance depth weight scales with the `limit` param; everything else we poll is
/// weight 1. Conservative: futures/data endpoints have their own limit but the
/// main weight budget is the binding constraint at our volume.
fn endpoint_weight(url: &str) -> u32 {
    if url.contains("/fapi/v1/depth") {
        let limit = url
            .split("limit=")
            .nth(1)
            .and_then(|s| s.split('&').next())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(50);
        match limit {
            0..=50 => 2,
            51..=100 => 5,
            101..=500 => 10,
            _ => 20,
        }
    } else {
        1
    }
}

/// REST API client with proxy support
pub struct RestClient {
    config: Arc<Config>,
    client_no_proxy: Client,
    proxy_manager: Arc<ProxyManager>,
    /// Cache of HTTP clients per proxy URL (prevents repeated TLS handshakes)
    proxy_clients: Arc<DashMap<String, Client>>,
    /// Proxy usage tracker for diagnostics
    proxy_tracker: Arc<ProxyUsageTracker>,
    /// Shared circuit breaker + two per-IP limiters: `limiter` for `/fapi/*`
    /// (weight budget / 60s), `data_limiter` for `/futures/data/*` (count / 5min).
    breaker: Arc<CircuitBreaker>,
    limiter: Arc<RateLimiter>,
    data_limiter: Arc<RateLimiter>,
}

/// Build the default header map for every REST client. Adds `X-MBX-APIKEY` only
/// when an api_key is configured; the public market-data endpoints this collector
/// uses need no auth, so an empty/absent key just means no header.
fn api_key_headers(config: &Config) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    let mut headers = HeaderMap::new();
    if let Some(key) = config.exchange.api_key.as_deref().filter(|k| !k.is_empty()) {
        if let Ok(val) = HeaderValue::from_str(key) {
            headers.insert(HeaderName::from_static("x-mbx-apikey"), val);
        }
    }
    headers
}

impl RestClient {
    pub fn new(config: Arc<Config>, proxy_manager: Arc<ProxyManager>) -> Result<Self> {
        // Create client without proxy for certain endpoints
        // Enable connection pooling to reuse TLS connections
        let client_no_proxy = ClientBuilder::new()
            .timeout(Duration::from_secs(30))
            .user_agent("binance-futures-collector/1.0")
            .default_headers(api_key_headers(&config))
            .pool_max_idle_per_host(50) // Keep up to 50 idle connections
            .pool_idle_timeout(Duration::from_secs(90)) // Keep alive for 90s
            .tcp_keepalive(Duration::from_secs(60)) // TCP keepalive
            .build()?;

        let breaker = Arc::new(CircuitBreaker::new());
        let limiter = Arc::new(RateLimiter::new(
            config.rest_intervals.max_weight_per_min,
            60_000_000_000,
            breaker.clone(),
        ));
        let data_limiter = Arc::new(RateLimiter::new(
            config.rest_intervals.max_data_requests_per_5min,
            300_000_000_000,
            breaker.clone(),
        ));

        Ok(Self {
            config: config.clone(),
            client_no_proxy,
            proxy_manager,
            proxy_clients: Arc::new(DashMap::new()),
            proxy_tracker: Arc::new(ProxyUsageTracker::new(config.proxy.tracking_enabled)),
            breaker,
            limiter,
            data_limiter,
        })
    }

    /// Get proxy usage statistics
    pub fn get_proxy_usage_stats(&self) -> Vec<ProxyUsageRecord> {
        self.proxy_tracker.get_usage_stats()
    }

    /// Get or create cached HTTP client for specific proxy
    /// Caches clients to reuse TLS connections.
    fn get_or_create_proxy_client(&self, proxy: &ProxyEndpoint) -> Result<Client> {
        // Check cache first - if client exists, reuse it
        if let Some(client) = self.proxy_clients.get(&proxy.url) {
            return Ok(client.clone());
        }

        // Create new client only if not cached
        let client = ClientBuilder::new()
            .proxy(Proxy::all(&proxy.url)?)
            .timeout(Duration::from_secs(30))
            .user_agent("binance-futures-collector/1.0")
            .default_headers(api_key_headers(&self.config))
            .pool_max_idle_per_host(20) // Keep 20 connections per proxy
            .pool_idle_timeout(Duration::from_secs(90)) // Keep alive for 90s
            .tcp_keepalive(Duration::from_secs(60)) // TCP keepalive
            .build()?;

        self.proxy_clients.insert(proxy.url.clone(), client.clone());

        Ok(client)
    }

    /// Make GET request with retry logic
    pub async fn get<T: DeserializeOwned>(&self, endpoint: &str) -> Result<T> {
        let url = format!("{}{}", self.config.exchange.rest_api_url, endpoint);
        let weight = endpoint_weight(&url);
        self.request_with_retry(&url, weight).await
    }

    /// Make GET request to futures data API
    pub async fn get_futures_data<T: DeserializeOwned>(&self, endpoint: &str) -> Result<T> {
        let url = format!("{}{}", self.config.exchange.rest_futures_data_url, endpoint);
        let weight = endpoint_weight(&url);
        self.request_with_retry(&url, weight).await
    }

    /// Internal request with retry logic. Rate limiting is handled by the shared
    /// weight limiter + circuit breaker: on a 429 the breaker is tripped (so the
    /// next `acquire()` waits it out) and we retry; on a 418 (IP ban) we do NOT
    /// retry; hammering a banned IP only extends the ban.
    async fn request_with_retry<T: DeserializeOwned>(&self, url: &str, weight: u32) -> Result<T> {
        let max_retries = 3;
        let mut last_error = None;

        for attempt in 0..max_retries {
            if attempt > 0 {
                let delay = Duration::from_millis(100 * (attempt as u64).pow(2));
                sleep(delay).await;
            }

            match self.make_request(url, weight).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if let Some(rl) = e.downcast_ref::<RateLimited>() {
                        if rl.banned {
                            warn!("HTTP 418 IP ban (retry_after={:?}); aborting request, not retrying: {}",
                                  rl.retry_after, url);
                            return Err(e);
                        }
                        // 429: breaker already tripped in make_request; retry after it lifts.
                        warn!(
                            "HTTP 429 rate limit (attempt {}, retry_after={:?}): {}",
                            attempt + 1,
                            rl.retry_after,
                            url
                        );
                    } else {
                        warn!("Request failed (attempt {}): {}", attempt + 1, e);
                    }
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("All retries failed")))
    }

    async fn make_request<T: DeserializeOwned>(&self, url: &str, weight: u32) -> Result<T> {
        // Acquire the right per-IP budget. Binance limits /fapi and /futures/data
        // separately, so this also waits out any active circuit-breaker pause.
        if url.contains("/futures/data/") {
            self.data_limiter.acquire(1).await;
        } else {
            self.limiter.acquire(weight).await;
        }

        let request_start = std::time::Instant::now();

        let (client, proxy_name) = if self.should_use_proxy(url) {
            if let Some(proxy) = self.proxy_manager.get_next_proxy() {
                let name = proxy.name.clone();
                let client = self.get_or_create_proxy_client(&proxy)?;
                (client, Some(name))
            } else {
                (self.client_no_proxy.clone(), None)
            }
        } else {
            (self.client_no_proxy.clone(), None)
        };

        debug!("Request: {} (proxy: {:?})", url, proxy_name);

        // Extract endpoint type and symbol from URL for tracking
        let (endpoint_type, symbol) = Self::parse_endpoint_info(url);

        let response = match timeout(Duration::from_secs(30), client.get(url).send()).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                warn!(
                    "HTTP request error for {}: {} (proxy: {:?})",
                    url, e, proxy_name
                );

                if let Some(name) = &proxy_name {
                    let error_str = e.to_string();
                    self.proxy_manager.report_failure(name, &error_str);

                    self.proxy_tracker.record_usage(ProxyUsageRecord {
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        symbol: symbol.clone(),
                        endpoint_type: endpoint_type.clone(),
                        proxy_name: name.clone(),
                        success: false,
                        latency_ms: Some(request_start.elapsed().as_millis() as f64),
                    });
                }

                return Err(anyhow::anyhow!("Request failed: {}", e));
            }
            Err(_) => {
                warn!(
                    "Request timeout (30s) for {}: (proxy: {:?})",
                    url, proxy_name
                );

                if let Some(name) = &proxy_name {
                    self.proxy_manager.report_failure(name, "Request timeout");

                    self.proxy_tracker.record_usage(ProxyUsageRecord {
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        symbol: symbol.clone(),
                        endpoint_type: endpoint_type.clone(),
                        proxy_name: name.clone(),
                        success: false,
                        latency_ms: Some(30000.0), // 30s timeout
                    });
                }

                return Err(anyhow::anyhow!("Request timeout"));
            }
        };

        let status = response.status();
        let latency_ms = request_start.elapsed().as_millis() as f64;

        // Proactive backpressure: if Binance reports we're near the per-minute
        // weight ceiling, briefly pause all requests before we cross into a 429.
        if let Some(used) = response
            .headers()
            .get("x-mbx-used-weight-1m")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
        {
            if used >= self.limiter.max_per_window.saturating_sub(200) {
                warn!(
                    "Binance used-weight-1m={} near cap {}; pausing 2s",
                    used, self.limiter.max_per_window
                );
                self.breaker.trip(Duration::from_secs(2));
            }
        }

        // Handle success/failure for proxy stats
        // Only report 5xx errors as proxy failures, not 4xx (client errors)
        if let Some(name) = &proxy_name {
            if status.is_success() {
                self.proxy_manager.report_success(name);

                self.proxy_tracker.record_usage(ProxyUsageRecord {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    symbol: symbol.clone(),
                    endpoint_type: endpoint_type.clone(),
                    proxy_name: name.clone(),
                    success: true,
                    latency_ms: Some(latency_ms),
                });
            } else if status.is_server_error() {
                // Only 5xx errors are proxy/server failures
                let reason = format!("HTTP {}", status);
                self.proxy_manager.report_failure(name, &reason);

                self.proxy_tracker.record_usage(ProxyUsageRecord {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    symbol: symbol.clone(),
                    endpoint_type: endpoint_type.clone(),
                    proxy_name: name.clone(),
                    success: false,
                    latency_ms: Some(latency_ms),
                });
            } else {
                // 4xx client errors: our fault, not proxy's fault
                // Report success for proxy health (it worked), but track as failed request
                self.proxy_manager.report_success(name);

                // Track client error (for debugging, but proxy is healthy)
                self.proxy_tracker.record_usage(ProxyUsageRecord {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    symbol: symbol.clone(),
                    endpoint_type: endpoint_type.clone(),
                    proxy_name: name.clone(),
                    success: false, // Request failed, but not proxy's fault
                    latency_ms: Some(latency_ms),
                });
            }
        }

        if !status.is_success() {
            let code = status.as_u16();
            // Rate limited (429) or IP banned (418): trip the shared breaker so
            // every task pauses, and return a typed error the retry layer reacts to.
            if code == 429 || code == 418 {
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .map(Duration::from_secs);
                let banned = code == 418;
                let pause =
                    retry_after.unwrap_or(Duration::from_secs(if banned { 120 } else { 5 }));
                self.breaker.trip(pause);
                warn!(
                    "Binance {} for {}: tripping circuit breaker for {:?}",
                    code, url, pause
                );
                return Err(anyhow::Error::new(RateLimited {
                    status: code,
                    retry_after,
                    banned,
                }));
            }

            let error_text = response.text().await.unwrap_or_default();
            bail!("HTTP {}: {}", status, error_text);
        }

        let text = response.text().await?;
        let result: T = serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse response: {}", text))?;

        Ok(result)
    }

    /// Determine if proxy should be used for URL
    fn should_use_proxy(&self, url: &str) -> bool {
        // Use proxy for most endpoints to distribute load
        // Don't use proxy for WebSocket or certain critical endpoints
        self.proxy_manager.is_enabled() && !url.contains("/ws") && !url.contains("/stream")
    }

    /// Parse endpoint type and symbol from URL for tracking
    fn parse_endpoint_info(url: &str) -> (String, String) {
        let endpoint_type = if url.contains("premiumIndexKlines") {
            "premium_index_klines".to_string()
        } else if url.contains("premiumIndex") {
            "funding_rates".to_string()
        } else if url.contains("openInterest") {
            "open_interest".to_string()
        } else if url.contains("takerlongshortRatio") {
            "taker_buy_sell_ratio".to_string()
        } else if url.contains("globalLongShortAccountRatio")
            || url.contains("topLongShortAccountRatio")
            || url.contains("topLongShortPositionRatio")
        {
            "long_short_ratio".to_string()
        } else if url.contains("markPriceKlines") {
            "mark_price_klines".to_string()
        } else if url.contains("indexPriceKlines") {
            "index_price_klines".to_string()
        } else if url.contains("depth") {
            "orderbooks".to_string()
        } else if url.contains("exchangeInfo") {
            "exchange_info".to_string()
        } else {
            "unknown".to_string()
        };

        let symbol = url
            .split("symbol=")
            .nth(1)
            .and_then(|s| s.split('&').next())
            .unwrap_or("ALL")
            .to_string();

        (endpoint_type, symbol)
    }

    /// Fetch exchange info
    pub async fn get_exchange_info(&self) -> Result<ExchangeInfo> {
        self.get("/fapi/v1/exchangeInfo").await
    }

    /// Fetch orderbook snapshot
    pub async fn get_depth_snapshot(&self, symbol: &str, limit: u32) -> Result<DepthSnapshotRest> {
        let endpoint = format!("/fapi/v1/depth?symbol={}&limit={}", symbol, limit);
        self.get(&endpoint).await
    }

    /// Fetch funding rate info (includes mark/index price)
    pub async fn get_premium_index(&self, symbol: Option<&str>) -> Result<Vec<FundingRateRest>> {
        let endpoint = if let Some(s) = symbol {
            format!("/fapi/v1/premiumIndex?symbol={}", s)
        } else {
            "/fapi/v1/premiumIndex".to_string()
        };

        // Handle both single and array responses
        let value: Value = self.get(&endpoint).await?;

        match value {
            Value::Array(arr) => serde_json::from_value(Value::Array(arr))
                .context("Failed to parse funding rate array"),
            Value::Object(_) => {
                let single: FundingRateRest = serde_json::from_value(value)?;
                Ok(vec![single])
            }
            _ => bail!("Unexpected response format for premiumIndex"),
        }
    }

    /// Fetch open interest
    pub async fn get_open_interest(&self, symbol: &str) -> Result<OpenInterestRest> {
        let endpoint = format!("/fapi/v1/openInterest?symbol={}", symbol);
        self.get(&endpoint).await
    }

    /// Fetch long/short ratio (global). `limit` buckets of `period` granularity.
    pub async fn get_global_long_short_ratio(
        &self,
        symbol: &str,
        period: &str,
        limit: u32,
    ) -> Result<Vec<LongShortRatioRest>> {
        let endpoint = format!(
            "/futures/data/globalLongShortAccountRatio?symbol={}&period={}&limit={}",
            symbol, period, limit
        );
        self.get_futures_data(&endpoint).await
    }

    /// Fetch top trader long/short ratio (account). `limit` buckets.
    pub async fn get_top_trader_account_ratio(
        &self,
        symbol: &str,
        period: &str,
        limit: u32,
    ) -> Result<Vec<LongShortRatioRest>> {
        let endpoint = format!(
            "/futures/data/topLongShortAccountRatio?symbol={}&period={}&limit={}",
            symbol, period, limit
        );
        self.get_futures_data(&endpoint).await
    }

    /// Fetch top trader long/short ratio (position). `limit` buckets.
    pub async fn get_top_trader_position_ratio(
        &self,
        symbol: &str,
        period: &str,
        limit: u32,
    ) -> Result<Vec<LongShortRatioRest>> {
        let endpoint = format!(
            "/futures/data/topLongShortPositionRatio?symbol={}&period={}&limit={}",
            symbol, period, limit
        );
        self.get_futures_data(&endpoint).await
    }

    /// Fetch mark price klines
    pub async fn get_mark_price_klines(
        &self,
        symbol: &str,
        interval: &str,
        limit: u32,
    ) -> Result<Vec<Vec<Value>>> {
        // For futures, mark/index price klines use 'symbol' parameter
        let endpoint = format!(
            "/fapi/v1/markPriceKlines?symbol={}&interval={}&limit={}",
            symbol, interval, limit
        );
        self.get(&endpoint).await
    }

    /// Fetch index price klines
    pub async fn get_index_price_klines(
        &self,
        symbol: &str,
        interval: &str,
        limit: u32,
    ) -> Result<Vec<Vec<Value>>> {
        // Index price klines use 'pair' parameter (different from mark price which uses 'symbol')
        let endpoint = format!(
            "/fapi/v1/indexPriceKlines?pair={}&interval={}&limit={}",
            symbol, interval, limit
        );
        self.get(&endpoint).await
    }

    /// Fetch taker buy/sell volume ratio. `limit` buckets of `period` granularity.
    pub async fn get_taker_buy_sell_ratio(
        &self,
        symbol: &str,
        period: &str,
        limit: u32,
    ) -> Result<Vec<TakerBuySellVolumeRest>> {
        let endpoint = format!(
            "/futures/data/takerlongshortRatio?symbol={}&period={}&limit={}",
            symbol, period, limit
        );
        self.get_futures_data(&endpoint).await
    }

    /// Fetch premium index klines
    pub async fn get_premium_index_klines(
        &self,
        symbol: &str,
        interval: &str,
        limit: u32,
    ) -> Result<Vec<Vec<Value>>> {
        let endpoint = format!(
            "/fapi/v1/premiumIndexKlines?symbol={}&interval={}&limit={}",
            symbol, interval, limit
        );
        self.get(&endpoint).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_parse_funding_rate() {
        // Test single response
        let single_json = r#"{
            "symbol": "BTCUSDT",
            "markPrice": "50000.00",
            "indexPrice": "49999.00",
            "estimatedSettlePrice": "50001.00",
            "lastFundingRate": "0.0001",
            "nextFundingTime": 1234567890000,
            "interestRate": "0.0003",
            "time": 1234567880000
        }"#;

        let _parsed: FundingRateRest = serde_json::from_str(single_json).unwrap();

        // Test array response
        let array_json = format!("[{}]", single_json);
        let _parsed: Vec<FundingRateRest> = serde_json::from_str(&array_json).unwrap();
    }

    #[test]
    fn depth_weight_scales_with_limit() {
        let w =
            |limit: u32| endpoint_weight(&format!("/fapi/v1/depth?symbol=BTCUSDT&limit={limit}"));
        assert_eq!(w(50), 2);
        assert_eq!(w(100), 5);
        assert_eq!(w(500), 10);
        assert_eq!(w(1000), 20);
        // Everything else is weight 1.
        assert_eq!(endpoint_weight("/fapi/v1/premiumIndex"), 1);
        assert_eq!(
            endpoint_weight("/futures/data/globalLongShortAccountRatio"),
            1
        );
    }

    #[test]
    fn breaker_is_open_until_tripped() {
        let b = CircuitBreaker::new();
        assert!(b.blocked_wait().is_none());
        b.trip(Duration::from_secs(30));
        assert!(b.blocked_wait().is_some());
    }

    #[tokio::test]
    async fn limiter_admits_under_budget_then_paces() {
        let breaker = Arc::new(CircuitBreaker::new());
        // Budget of 2 weight per 50 ms window.
        let limiter = RateLimiter::new(2, 50_000_000, breaker);

        let t = Instant::now();
        limiter.acquire(1).await;
        limiter.acquire(1).await;
        assert!(
            t.elapsed() < Duration::from_millis(20),
            "under-budget acquires must not block"
        );

        // The third exceeds the budget and must wait for the window to roll.
        let t = Instant::now();
        limiter.acquire(1).await;
        assert!(
            t.elapsed() >= Duration::from_millis(40),
            "over-budget acquire must pace"
        );
    }
}
