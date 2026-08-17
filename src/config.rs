use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub exchange: ExchangeConfig,
    pub collection: CollectionConfig,
    pub orderbook: OrderbookConfig,
    pub websocket: WebSocketConfig,
    pub rest_intervals: RestIntervalsConfig,
    pub output: OutputConfig,
    pub gaps: GapsConfig,
    pub logging: LoggingConfig,
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub storage: StorageConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExchangeConfig {
    pub name: String,
    pub market: String,
    pub websocket_url: String,
    pub rest_api_url: String,
    pub rest_futures_data_url: String,
    /// Optional Binance API key. NOT required: this collector reads only public
    /// market-data endpoints, which need no authentication. When set, it is sent
    /// as the `X-MBX-APIKEY` header so requests are associated with your account.
    /// Use a READ-ONLY key (reading only, no trading or withdrawals, IP-restricted).
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CollectionConfig {
    // Number of symbols will be determined at runtime from exchange info
    #[serde(default = "default_symbol_discovery_interval_minutes")]
    pub symbol_discovery_interval_minutes: u64,

    /// Explicit symbol whitelist. If non-empty, ONLY these symbols are collected
    /// and exchange discovery is bypassed (useful for testing or targeted capture).
    /// Empty (default) = collect all discovered tradeable USDT-M perpetuals.
    #[serde(default)]
    pub symbols: Vec<String>,
}

fn default_symbol_discovery_interval_minutes() -> u64 {
    10
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrderbookConfig {
    pub depth_limit: u32,
    pub snapshot_interval_seconds: u64,
    pub update_speed_ms: u32,
    // Resync and attach settings
    pub resync_cooldown_seconds: u64,
    pub resync_backoff_factor: f64,
    pub max_resync_backoff_seconds: u64,
    pub attach_bridge_timeout_seconds: u64,
    pub snapshot_http_timeout_seconds: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WebSocketConfig {
    pub reconnect_delay_ms: u64,
    pub max_reconnect_delay_ms: u64,
    pub max_reconnect_attempts: u32,
    pub ping_interval_seconds: u64,
    pub heartbeat_miss_reconnect: u32,

    // Ring buffer constraints (for memory management)
    pub ring_buffer_capacity: usize,
    pub ring_buffer_seconds: u64,

    // Coordinated startup
    pub ws_buffer_delay_seconds: u64,
    pub batch_gap_seconds: u64,
    pub batch_stability_min_messages: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RestIntervalsConfig {
    pub funding_rates_seconds: u64,
    pub mark_price_klines_seconds: u64,
    pub index_price_klines_seconds: u64,
    pub open_interest_seconds: u64,
    pub long_short_ratio_seconds: u64,
    pub taker_buy_sell_ratio_seconds: u64,
    pub premium_index_klines_seconds: u64,
    /// Per-minute request WEIGHT budget for the `/fapi/*` REST limiter. Binance's
    /// per-IP limit is 2400/min; 2000 leaves headroom. Optional; defaults if absent.
    #[serde(default = "default_max_weight_per_min")]
    pub max_weight_per_min: u64,
    /// Request-COUNT budget over 5 minutes for the `/futures/data/*` endpoints
    /// (long/short, taker), which Binance limits separately and more tightly than
    /// the main weight budget. Optional; defaults if absent.
    #[serde(default = "default_max_data_requests_per_5min")]
    pub max_data_requests_per_5min: u64,
}

fn default_max_weight_per_min() -> u64 {
    2000
}

fn default_max_data_requests_per_5min() -> u64 {
    600
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OutputConfig {
    pub base_dir: String,
    /// Write-buffer size PER FILE in KB. Data flushes every second, so this stays
    /// small: at ~thousands of open files, a large per-file buffer is the main
    /// source of RSS growth. Optional; defaults if absent.
    #[serde(default = "default_buffer_size_kb")]
    pub buffer_size_kb: usize,
    pub flush_interval_seconds: u64,
    pub max_depth_buffer_rows: usize,
}

fn default_buffer_size_kb() -> usize {
    64
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GapsConfig {
    pub base_dir: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoggingConfig {
    pub level: String,
    pub log_file: String,
    pub max_log_files: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProxyConfig {
    pub enabled: bool,
    pub user: String,
    pub password: String,
    pub host: String,
    pub au_ports: Vec<u16>,
    pub jp_ports: Vec<u16>,
    pub dk_ports: Vec<u16>,
    #[serde(default)]
    pub uk_ports: Vec<u16>,
    pub num_ws_batches: usize,
    #[serde(default)]
    pub tracking_enabled: bool, // Enable detailed proxy usage tracking (memory intensive)
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct StorageConfig {
    #[serde(default)]
    pub disable_trade_persistence_for: Vec<String>,
    #[serde(default)]
    pub disable_orderbook_persistence_for: Vec<String>,
}

impl StorageConfig {
    pub fn should_persist_trades(&self, symbol: &str) -> bool {
        !self
            .disable_trade_persistence_for
            .iter()
            .any(|s| s == symbol)
    }

    pub fn should_persist_orderbook(&self, symbol: &str) -> bool {
        !self
            .disable_orderbook_persistence_for
            .iter()
            .any(|s| s == symbol)
    }
}

impl Config {
    /// Load configuration from a TOML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        // Name the file and say how to create it. The bare io::Error is just
        // "No such file or directory (os error 2)", which on a first run tells the
        // user nothing about what is missing or what to do next.
        let contents = std::fs::read_to_string(path).with_context(|| {
            format!(
                "could not read config file {}\n       \
                 create one with:  cp config.example.toml config.toml\n       \
                 or point CONFIG_PATH at an existing file",
                path.display()
            )
        })?;
        let config: Config = toml::from_str(&contents)
            .with_context(|| format!("could not parse {} as TOML", path.display()))?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        // The shipped example config must always load cleanly.
        let config =
            Config::from_file("config.example.toml").expect("config.example.toml should parse");
        assert_eq!(config.exchange.name, "binance");
        assert_eq!(config.exchange.market, "futures");
        assert!(!config.proxy.enabled);
    }
}
