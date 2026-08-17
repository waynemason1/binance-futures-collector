use anyhow::Result;
use log::{info, warn};
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;

use crate::rest::RestClient;

#[derive(Debug, Default, Deserialize)]
struct ExclusionConfig {
    #[serde(default)]
    excluded_stablecoins: Vec<String>,
    #[serde(default)]
    excluded_top30: Vec<String>,
    #[serde(default)]
    memecoin_exceptions: Vec<String>,
    #[serde(default)]
    excluded_symbols: Vec<String>,
}

/// Load the symbol-exclusion list from a TOML file.
///
/// Path resolution: the `BINANCE_EXCLUSIONS_PATH` env var if set, otherwise
/// `exclusions.toml` in the working directory. A missing or invalid file is
/// non-fatal: discovery proceeds with no exclusions (every tradeable symbol
/// is collected).
async fn load_exclusion_config() -> ExclusionConfig {
    let config_path =
        std::env::var("BINANCE_EXCLUSIONS_PATH").unwrap_or_else(|_| "exclusions.toml".to_string());

    match tokio::fs::read_to_string(&config_path).await {
        Ok(contents) => match toml::from_str(&contents) {
            Ok(config) => {
                info!("Loaded symbol exclusions from {}", config_path);
                config
            }
            Err(e) => {
                warn!(
                    "Failed to parse {} ({}); proceeding with no exclusions",
                    config_path, e
                );
                ExclusionConfig::default()
            }
        },
        Err(_) => {
            warn!(
                "No exclusions file at {}; proceeding with no exclusions",
                config_path
            );
            ExclusionConfig::default()
        }
    }
}

/// Extract base asset from symbol (handles multipliers like 1000SHIBUSDT)
/// Returns the base coin without multipliers (e.g., "1000SHIB" -> "SHIB")
fn extract_base_asset(base_asset: &str) -> String {
    // Handle multiplier symbols (e.g., "1000SHIB" -> "SHIB"). Test the LONGEST
    // prefix first, otherwise "1000" matches "10000"/"1000000" and mis-strips
    // (e.g. "10000SATS" -> "0SATS").
    if let Some(stripped) = base_asset.strip_prefix("1000000") {
        stripped.to_string()
    } else if let Some(stripped) = base_asset.strip_prefix("10000") {
        stripped.to_string()
    } else if let Some(stripped) = base_asset.strip_prefix("1000") {
        stripped.to_string()
    } else {
        base_asset.to_string()
    }
}

/// Discovers active futures symbols from the exchange
pub struct SymbolDiscovery {
    rest_client: Arc<RestClient>,
    excluded_base_assets: HashSet<String>,
    excluded_symbols: HashSet<String>,
}

impl SymbolDiscovery {
    pub async fn new(rest_client: Arc<RestClient>) -> Self {
        // Load exclusion configuration (non-fatal if absent)
        let exclusion_config = load_exclusion_config().await;

        // Build excluded base assets (stablecoins + top30 minus memecoin exceptions)
        let mut excluded_base_assets = HashSet::new();

        for coin in exclusion_config.excluded_stablecoins {
            excluded_base_assets.insert(coin.to_lowercase());
        }

        let memecoin_set: HashSet<String> = exclusion_config
            .memecoin_exceptions
            .iter()
            .map(|s| s.to_lowercase())
            .collect();

        for coin in exclusion_config.excluded_top30 {
            if !memecoin_set.contains(&coin.to_lowercase()) {
                excluded_base_assets.insert(coin.to_lowercase());
            }
        }

        let excluded_symbols: HashSet<String> = exclusion_config
            .excluded_symbols
            .iter()
            .map(|s| s.to_lowercase())
            .collect();

        Self {
            rest_client,
            excluded_base_assets,
            excluded_symbols,
        }
    }

    /// Discover all active perpetual futures symbols
    pub async fn discover_symbols(&self) -> Result<Vec<String>> {
        info!("Starting symbol discovery...");

        let exchange_info = self.rest_client.get_exchange_info().await?;

        let mut symbols = Vec::new();
        let mut excluded_count = 0;

        for symbol_info in &exchange_info.symbols {
            if symbol_info.status != "TRADING" {
                continue;
            }

            // Only perpetual contracts (not quarterly/bi-quarterly)
            if symbol_info.contract_type != "PERPETUAL" {
                continue;
            }

            // Skip pre-market contracts
            if let Some(underlying) = &symbol_info.underlying_type {
                if underlying == "PREMARKET" {
                    excluded_count += 1;
                    continue;
                }
            }

            // Only USDT-settled futures (reject USDC, BUSD, etc.)
            if symbol_info.quote_asset != "USDT" {
                excluded_count += 1;
                continue;
            }

            // Extract normalized base asset (handles multipliers like "1000SHIB" -> "SHIB")
            let normalized_base = extract_base_asset(&symbol_info.base_asset);

            if self
                .excluded_base_assets
                .contains(&normalized_base.to_lowercase())
            {
                excluded_count += 1;
                continue;
            }

            // Check if symbol is explicitly excluded (set is lowercased; the API
            // symbol is uppercase, so compare lowercased or the list never matches).
            if self
                .excluded_symbols
                .contains(&symbol_info.symbol.to_lowercase())
            {
                excluded_count += 1;
                continue;
            }

            symbols.push(symbol_info.symbol.to_lowercase());
        }

        // Anchor symbols the dashboard's replay/validation views assume exist:
        // always collected, overriding every exclusions.toml rule. (SHIB needs
        // no entry. Binance lists it as 1000SHIBUSDT, which survives exclusion
        // via memecoin_exceptions + multiplier normalisation.)
        let exceptions = ["btcusdt", "dogeusdt"];
        for exception in exceptions {
            if !symbols.contains(&exception.to_string()) {
                // Check if it exists in exchange info
                if exchange_info
                    .symbols
                    .iter()
                    .any(|s| s.symbol.to_lowercase() == exception && s.status == "TRADING")
                {
                    symbols.push(exception.to_string());
                }
            }
        }

        symbols.sort();

        info!(
            "Symbol discovery complete: {} active symbols found ({} excluded)",
            symbols.len(),
            excluded_count
        );

        Ok(symbols)
    }
}

/// What one discovery cycle changed, per [`SymbolSetTracker::observe`].
#[derive(Debug, Default)]
pub struct SymbolSetDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// Tracks the symbol universe across periodic discovery cycles and classifies
/// changes. Additions count immediately; a removal is only confirmed after a
/// symbol is absent from two CONSECUTIVE successful discovery results, so one
/// flaky `exchangeInfo` response can never tear down a live symbol. A failed
/// discovery call must simply not be fed to the tracker; a skipped cycle
/// leaves all strikes as they were.
pub struct SymbolSetTracker {
    known: HashSet<String>,
    missing_once: HashSet<String>,
}

impl SymbolSetTracker {
    pub fn new(initial: impl IntoIterator<Item = String>) -> Self {
        Self {
            known: initial.into_iter().collect(),
            missing_once: HashSet::new(),
        }
    }

    /// Number of symbols currently tracked as listed.
    pub fn tracked_len(&self) -> usize {
        self.known.len()
    }

    /// Feed one successful discovery result. Returns newly listed symbols
    /// (joined the known set now) and confirmed removals (absent twice in a
    /// row, dropped from the known set, so a later relisting reports as a
    /// fresh addition).
    pub fn observe(&mut self, discovered: &HashSet<String>) -> SymbolSetDiff {
        let mut added: Vec<String> = discovered
            .iter()
            .filter(|s| !self.known.contains(*s))
            .cloned()
            .collect();
        added.sort();
        for s in &added {
            self.known.insert(s.clone());
        }

        let mut removed = Vec::new();
        let mut missing_now = HashSet::new();
        for s in &self.known {
            if !discovered.contains(s) {
                if self.missing_once.contains(s) {
                    removed.push(s.clone()); // second consecutive absence
                } else {
                    missing_now.insert(s.clone()); // first strike
                }
            }
        }
        removed.sort();
        for s in &removed {
            self.known.remove(s);
        }
        // Symbols that reappeared shed their strike by not being re-inserted.
        self.missing_once = missing_now;

        SymbolSetDiff { added, removed }
    }
}

#[cfg(test)]
mod tests {
    use super::extract_base_asset;
    use super::SymbolSetTracker;
    use std::collections::HashSet;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn tracker_reports_new_listing_immediately() {
        let mut t = SymbolSetTracker::new(["btcusdt".to_string()]);
        let d = t.observe(&set(&["btcusdt", "newusdt"]));
        assert_eq!(d.added, vec!["newusdt"]);
        assert!(d.removed.is_empty());
        // Not re-reported on the next cycle.
        let d = t.observe(&set(&["btcusdt", "newusdt"]));
        assert!(d.added.is_empty() && d.removed.is_empty());
    }

    #[test]
    fn tracker_requires_two_consecutive_absences_to_remove() {
        let mut t = SymbolSetTracker::new(["btcusdt".to_string(), "oldusdt".to_string()]);
        // First absence: a strike, not a removal.
        let d = t.observe(&set(&["btcusdt"]));
        assert!(d.removed.is_empty());
        // Second consecutive absence: confirmed.
        let d = t.observe(&set(&["btcusdt"]));
        assert_eq!(d.removed, vec!["oldusdt"]);
        // Gone from the known set, not reported again.
        let d = t.observe(&set(&["btcusdt"]));
        assert!(d.removed.is_empty());
    }

    #[test]
    fn tracker_clears_strike_when_symbol_reappears() {
        let mut t = SymbolSetTracker::new(["btcusdt".to_string(), "flakyusdt".to_string()]);
        // strike 1
        assert!(t.observe(&set(&["btcusdt"])).removed.is_empty());
        // Reappears: strike cleared, nothing reported.
        let d = t.observe(&set(&["btcusdt", "flakyusdt"]));
        assert!(d.added.is_empty() && d.removed.is_empty());
        // A later absence starts over at strike 1.
        assert!(t.observe(&set(&["btcusdt"])).removed.is_empty());
        assert_eq!(t.observe(&set(&["btcusdt"])).removed, vec!["flakyusdt"]);
    }

    #[test]
    fn tracker_reports_relisting_as_a_fresh_addition() {
        let mut t = SymbolSetTracker::new(["goneusdt".to_string()]);
        t.observe(&set(&[]));
        assert_eq!(t.observe(&set(&[])).removed, vec!["goneusdt"]);
        let d = t.observe(&set(&["goneusdt"]));
        assert_eq!(d.added, vec!["goneusdt"]);
    }

    #[test]
    fn strips_multipliers_longest_prefix_first() {
        assert_eq!(extract_base_asset("BTC"), "BTC");
        assert_eq!(extract_base_asset("1000SHIB"), "SHIB");
        assert_eq!(extract_base_asset("10000SATS"), "SATS");
        assert_eq!(extract_base_asset("1000000MOG"), "MOG");
        // A ticker that merely begins with a digit is left intact.
        assert_eq!(extract_base_asset("1INCH"), "1INCH");
    }
}
