// On Linux, use jemalloc as the global allocator. Its background_thread + decay config
// (.cargo/config.toml) returns freed pages to the OS on a timer, keeping RSS bounded for
// this long-running high-churn collector; glibc's arenas otherwise ratchet up over days.
// See docs/SOAK_TEST.md. Gated to Linux; other targets use the system allocator.
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use anyhow::Result;
use flexi_logger::{Age, Cleanup, Criterion, FileSpec, Logger, Naming};
use log::{error, info, warn};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use tokio::signal;
use tokio::signal::unix::{signal as unix_signal, SignalKind};
use tokio::sync::{mpsc, Notify};

use binance_futures_collector::{
    collectors::{OrderbookManager, RestCollector, SymbolDiscovery, SymbolSetTracker},
    rest::{ProxyManager, RestClient},
    utils::{CsvWriter, GapRegistry, StatsExporter},
    websocket::{MessageHandler, WebSocketManager},
    Config,
};

/// Get current memory usage in MB by reading /proc/self/status (Linux only)
fn get_memory_usage_mb() -> Result<u64> {
    let status = std::fs::read_to_string("/proc/self/status")?;

    for line in status.lines() {
        if line.starts_with("VmRSS:") {
            // VmRSS is in kB, convert to MB
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let kb: u64 = parts[1].parse()?;
                return Ok(kb / 1024);
            }
        }
    }

    anyhow::bail!("Could not find VmRSS in /proc/self/status")
}

/// Raise the open-file-descriptor soft limit toward the hard limit.
///
/// The collector keeps one buffered writer open per (symbol × datatype). At a
/// handful of symbols that's fine, but a full-universe run (~500 symbols ×
/// ~12 datatypes) needs several thousand descriptors and slams into the common
/// default soft limit of 1024, after which every new file then fails to open with EMFILE
/// ("too many open files"), silently dropping symbols. Rather than make the
/// operator remember `ulimit -n`, we lift our own soft limit at startup.
/// Non-fatal: if it can't be raised we warn and continue.
#[cfg(unix)]
fn raise_fd_limit() {
    unsafe {
        let mut lim: libc::rlimit = std::mem::zeroed();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            warn!("Could not read RLIMIT_NOFILE; leaving open-file limit at OS default");
            return;
        }
        let old = lim.rlim_cur;
        if old >= lim.rlim_max {
            info!("Open-file limit (RLIMIT_NOFILE) already at max: {}", old);
            return;
        }
        // Prefer the hard limit; some platforms (macOS) reject RLIM_INFINITY,
        // so fall back to a concrete, generous target.
        lim.rlim_cur = lim.rlim_max;
        if libc::setrlimit(libc::RLIMIT_NOFILE, &lim) != 0 {
            lim.rlim_cur = 65536.max(old);
            if libc::setrlimit(libc::RLIMIT_NOFILE, &lim) != 0 {
                warn!("Failed to raise RLIMIT_NOFILE (soft {}); may hit 'too many open files' beyond ~80 symbols", old);
                return;
            }
        }
        info!(
            "Raised open-file limit (RLIMIT_NOFILE): {} → {}",
            old, lim.rlim_cur
        );
    }
}

#[cfg(not(unix))]
fn raise_fd_limit() {}

/// Log the shutdown signal and current memory before draining.
fn log_shutdown_signal(signal_name: &str) {
    let mem = get_memory_usage_mb()
        .map(|m| format!("{} MB", m))
        .unwrap_or_else(|_| "unknown".to_string());
    info!(
        "Received {}; beginning graceful shutdown (RSS {})",
        signal_name, mem
    );
}

// Async runtime sized for the websocket, REST, and writer task load.
#[tokio::main(worker_threads = 8)]
async fn main() -> Result<()> {
    // Answered before anything else is set up, so `--version` works even when the
    // config is missing or broken. Without a way to ask a running install what it
    // is, "am I up to date?" is unanswerable.
    match std::env::args().nth(1).as_deref() {
        Some("--version") | Some("-V") => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--help") | Some("-h") => {
            println!(
                "{} {}\n\n\
                 Captures the Binance USDT-M perpetual futures tape to CSV.\n\n\
                 Usage: binance-futures-collector [--version|--help]\n\n\
                 Configuration is read from ./config.toml, or from the path in\n\
                 the CONFIG_PATH environment variable. See README.md.",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION")
            );
            return Ok(());
        }
        _ => {}
    }

    // Set panic hook FIRST for visibility even if logger fails
    std::panic::set_hook(Box::new(|panic_info| {
        eprintln!("PANIC: {:?}", panic_info);
        // Also write to file in case stderr is lost
        let _ = std::fs::write(
            "/tmp/binance_futures_panic.txt",
            format!("{:?}", panic_info),
        );
    }));

    // Load configuration FIRST (needed for logger setup)
    let config_path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.toml".to_string());

    let config = Arc::new(Config::from_file(&config_path)?);

    let log_path = Path::new(&config.logging.log_file);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let log_basename = log_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("binance_futures_collector");

    // Clean log format: UTC timestamp, level, message. UTC (via .use_utc() below)
    // matches the data (every CSV column and stats.json field is UTC), so a log
    // line correlates directly with a data row; the level keeps it greppable by
    // severity, and the explicit "UTC" makes the zone unambiguous.
    let log_format = |writer: &mut dyn std::io::Write,
                      now: &mut flexi_logger::DeferredNow,
                      record: &log::Record| {
        write!(
            writer,
            "{} UTC {:<5} {}",
            now.format("%Y-%m-%d %H:%M:%S"),
            record.level(),
            record.args()
        )
    };

    Logger::try_with_str(&config.logging.level)?
        .use_utc() // stamp lines AND roll files at UTC midnight, not the host's local day
        .log_to_file(
            FileSpec::default()
                .directory(log_path.parent().unwrap_or(Path::new(".")))
                .basename(log_basename)
                .suffix("log"),
        )
        .rotate(
            Criterion::Age(Age::Day),
            Naming::Timestamps,
            Cleanup::KeepLogFiles(config.logging.max_log_files as usize),
        )
        .format(log_format) // Apply clean format to both file and stderr
        .duplicate_to_stderr(flexi_logger::Duplicate::Info)
        .start()?;

    info!("Starting Binance USDT-M futures collector");
    info!("Configuration loaded from {}", config_path);
    info!("Logging to {}", config.logging.log_file);

    // Lift our own open-file limit so a full-universe run doesn't hit EMFILE.
    raise_fd_limit();

    info!("Runtime configuration:");
    info!("  • Orderbook Stream: ENABLED");
    info!("  • REST Collectors: ENABLED");
    info!("  • WebSocket Batches: {}", config.proxy.num_ws_batches);

    let proxy_manager = Arc::new(ProxyManager::new(config.clone()));
    if proxy_manager.is_enabled() {
        info!(
            "Proxy manager initialized with {} proxies",
            proxy_manager.count()
        );
        proxy_manager.start_health_checker().await?;
        info!("Proxy manager background tasks started");
    }

    let rest_client = Arc::new(RestClient::new(config.clone(), proxy_manager.clone())?);

    // Use the configured symbol whitelist if set; otherwise discover from the exchange.
    let symbol_discovery = SymbolDiscovery::new(rest_client.clone()).await;
    let using_whitelist = !config.collection.symbols.is_empty();
    let symbols: Vec<String> = if using_whitelist {
        let list: Vec<String> = config
            .collection
            .symbols
            .iter()
            .map(|s| s.to_lowercase())
            .collect();
        info!(
            "Using {} configured symbols (discovery bypassed): {:?}",
            list.len(),
            list
        );
        list
    } else {
        let discovered = symbol_discovery.discover_symbols().await?;
        info!("Retrieved {} active futures symbols", discovered.len());
        discovered
    };

    // Raw trades/orderbook persistence can optionally be disabled per-symbol via
    // the [storage] config section, e.g. to skip a very high-volume symbol's depth
    // firehose and save disk. Klines and REST metrics are always collected. Report
    // only what is actually being skipped (nothing, by default).
    let skip_trades = &config.storage.disable_trade_persistence_for;
    let skip_depth = &config.storage.disable_orderbook_persistence_for;
    if !skip_trades.is_empty() || !skip_depth.is_empty() {
        info!(
            "Storage: raw-trade persistence disabled for {:?}; orderbook persistence disabled for {:?} (klines + REST still collected)",
            skip_trades, skip_depth
        );
    }

    let csv_writer = Arc::new(CsvWriter::new(config.clone())?);
    info!("CSV writer initialized");

    let gap_registry = Arc::new(GapRegistry::new(config.gaps.base_dir.clone().into()).await?);
    gap_registry.start_periodic_save().await?;
    info!("Gap registry initialized");

    // Start hourly file rotation for depth updates
    csv_writer.start_periodic_flush().await?;
    csv_writer.start_hourly_rotation().await?;
    info!("Hourly file rotation started");

    // Message channel: 10k slots is ample headroom at the 500ms update cadence.
    let (message_tx, message_rx) = mpsc::channel(10000);

    // Liquidations arrive on the GLOBAL !forceOrder@arr stream: the entire
    // futures market (USDC-margined, coin-margined, dated delivery, equity
    // perps), not just our subscriptions. Filter them to the USDT-M universe we
    // actually collect so nothing outside it ever lands on disk.
    let allowed_symbols: Arc<std::collections::HashSet<String>> =
        Arc::new(symbols.iter().cloned().collect());

    let message_handler = Arc::new(MessageHandler::new(csv_writer.clone(), allowed_symbols));

    // Start message handler. Keep a Notify + JoinHandle so shutdown can tell it
    // to drain the queue and then join it, instead of sleeping a fixed interval.
    let handler_shutdown = Arc::new(Notify::new());
    let handler = message_handler.clone();
    let handler_shutdown_run = handler_shutdown.clone();
    let handler_join = tokio::spawn(async move {
        if let Err(e) = handler.run(message_rx, handler_shutdown_run).await {
            error!("Message handler error: {}", e);
        }
    });

    // Initialize WebSocket manager FIRST (must buffer messages before snapshots)
    // Use full symbol list (BTCUSDT filtered at message handler level)
    let ws_manager = Arc::new(WebSocketManager::new(config.clone(), message_tx, &symbols));

    // Symbols confirmed delisted by periodic discovery (absent from two
    // consecutive exchangeInfo results). Discovery writes it; the order-book
    // task for a member exits and drops its state, and REST pollers skip it,
    // so a dead market stops consuming retries and rate-limit budget mid-run.
    let delisted: Arc<dashmap::DashSet<String>> = Arc::new(dashmap::DashSet::new());

    // New listings detected mid-run (symbol -> detection epoch ms). Discovery
    // writes it; the stats exporter surfaces it so the dashboard can alert,
    // capture for these begins at the next (graceful) restart by design.
    let new_listings: Arc<dashmap::DashMap<String, i64>> = Arc::new(dashmap::DashMap::new());

    // Initialize orderbook manager (with WebSocket manager for coordinated startup)
    let orderbook_manager = Arc::new(OrderbookManager::new(
        config.clone(),
        rest_client.clone(),
        csv_writer.clone(),
        ws_manager.clone(),
        gap_registry.clone(),
        delisted.clone(),
    ));

    // Extract BTCUSDT as priority symbol (will initialize first)
    let priority_symbol = if symbols.contains(&"btcusdt".to_string()) {
        Some("btcusdt".to_string())
    } else {
        None
    };

    // Start orderbook manager (will coordinate WebSocket startup)
    // BTCUSDT starts first (if present), then normal batched initialization
    info!("Starting orderbook manager with COORDINATED STAGED STARTUP...");
    if priority_symbol.is_some() {
        info!("Priority symbol: BTCUSDT will initialize first (klines, trades, orderbooks)");
    }
    let manager_clone = orderbook_manager.clone();
    let symbols_clone = symbols.clone();
    tokio::spawn(async move {
        if let Err(e) = manager_clone.start(symbols_clone, priority_symbol).await {
            error!("Orderbook manager initialization error: {}", e);
        } else {
            info!("All orderbook symbols initialized successfully");
        }
    });
    info!("Orderbook manager initialization spawned in background");

    let rest_collector = RestCollector::new(
        config.clone(),
        rest_client.clone(),
        csv_writer.clone(),
        delisted.clone(),
    );

    rest_collector.start(symbols.clone()).await?;

    // Sit stats/ alongside futures/ under the configured output root, so the
    // heartbeat always lives with the capture it describes.
    let stats_dir = Path::new(&config.output.base_dir)
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("stats");
    let stats_exporter = Arc::new(StatsExporter::new(
        "binance_futures".to_string(),
        ws_manager.clone(),
        message_handler.clone(),
        rest_client.clone(),
        new_listings.clone(),
        stats_dir.clone(),
    ));
    stats_exporter.clone().start().await;
    info!(
        "Stats exporter started (writing to {})",
        stats_dir.join("stats.json").display()
    );

    // Setup graceful shutdown (SIGINT or SIGTERM)
    let mut sigterm = unix_signal(SignalKind::terminate())?;
    let mut sighup = unix_signal(SignalKind::hangup())?; // Catch terminal hangup
    info!("System started. Listening for shutdown signals (SIGINT/SIGTERM/SIGHUP)...");

    let ws_manager_clone = ws_manager.clone();
    let orderbook_manager_clone = orderbook_manager.clone();
    let message_handler_clone = message_handler.clone();
    let proxy_manager_clone = proxy_manager.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;

            info!("───────────────────────────────────────");
            info!("Stats Report");

            let ws_stats = ws_manager_clone.get_stats().await;
            info!("  • WebSocket Connections: {}", ws_stats.len());

            let ob_stats = orderbook_manager_clone.get_stats();
            let live_count = ob_stats.values().filter(|s| s.state == "Live").count();
            info!("  • Orderbooks Live: {}/{}", live_count, ob_stats.len());

            let msg_stats = message_handler_clone.get_stats();
            info!(
                "  • Messages - Depth: {} | Trades: {} | Klines: {} | Liquidations: {} | Errors: {}",
                msg_stats.depth_updates,
                msg_stats.trades,
                msg_stats.klines,
                msg_stats.liquidations,
                msg_stats.errors
            );

            if proxy_manager_clone.is_enabled() {
                let proxy_stats = proxy_manager_clone.get_stats();
                let healthy_count = proxy_stats.values().filter(|s| s.health_score > 30).count();
                info!(
                    "  • Proxies Healthy: {}/{}",
                    healthy_count,
                    proxy_stats.len()
                );
            }

            // Memory usage (read from /proc/self/status on Linux)
            if let Ok(mem_mb) = get_memory_usage_mb() {
                info!("  • Memory Usage: {} MB", mem_mb);
            }

            info!("───────────────────────────────────────");
        }
    });

    // Periodic symbol discovery: detect newly listed and delisted symbols.
    // Skipped when a fixed [collection].symbols whitelist is configured.
    if !using_whitelist {
        let symbol_discovery_clone = symbol_discovery;
        let discovery_interval_minutes = config.collection.symbol_discovery_interval_minutes;
        let delisted_set = delisted.clone();
        let new_listings_reg = new_listings.clone();
        tokio::spawn(async move {
            let mut tracker = SymbolSetTracker::new(symbols.iter().cloned());
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                discovery_interval_minutes * 60,
            ));
            // Delay, not Burst: after a stall (VM pause, host suspend), missed
            // ticks must NOT fire back-to-back: the two-strike removal guard
            // depends on consecutive checks being a real interval apart.
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                interval.tick().await;

                match symbol_discovery_clone.discover_symbols().await {
                    Ok(discovered_symbols) => {
                        let discovered: HashSet<String> = discovered_symbols.into_iter().collect();

                        // Sanity floor: an exchange maintenance window (every
                        // contract leaves TRADING) or a degraded-but-200
                        // exchangeInfo response can drop most of the universe
                        // at once. Book teardown is restart-permanent, so
                        // refuse any result that lost >10% of the tracked set
                        // — skip the cycle instead of feeding the tracker.
                        if discovered.len() * 10 < tracker.tracked_len() * 9 {
                            error!(
                                "Discovery returned {} symbols vs {} tracked (>10% drop) \
                                 — treating as degraded/maintenance, skipping cycle",
                                discovered.len(),
                                tracker.tracked_len()
                            );
                            continue;
                        }

                        let diff = tracker.observe(&discovered);

                        if !diff.added.is_empty() {
                            info!(
                                "Discovered {} new symbols: {:?} — restart to capture",
                                diff.added.len(),
                                diff.added
                            );
                            // New listings are logged and surfaced on the
                            // dashboard; capture for them starts on the next
                            // (graceful, one-click) restart — attaching
                            // mid-run would need new WS connections, orderbook
                            // init, and REST registration. A RELISTED symbol
                            // does resume REST polling immediately, though.
                            let now_ms = chrono::Utc::now().timestamp_millis();
                            for symbol in &diff.added {
                                delisted_set.remove(symbol);
                                new_listings_reg.insert(symbol.clone(), now_ms);
                            }
                        }

                        if !diff.removed.is_empty() {
                            warn!(
                                "{} symbols left TRADING (absent from two consecutive \
                                 discovery cycles): {:?} — stopping their collection",
                                diff.removed.len(),
                                diff.removed
                            );
                            // Members are picked up by the order-book task
                            // (exits, drops state) and the REST pollers (skip).
                            // Also clear any pending new-listing alert — a pair
                            // that listed and then left TRADING mid-run must not
                            // keep prompting the operator to restart for it.
                            for symbol in diff.removed {
                                new_listings_reg.remove(&symbol);
                                delisted_set.insert(symbol);
                            }
                        }
                    }
                    Err(e) => {
                        // A failed cycle is skipped entirely — it never feeds
                        // the tracker, so it can't count toward a removal.
                        error!("Symbol discovery error: {}", e);
                    }
                }
            }
        });
    }

    tokio::select! {
        _ = signal::ctrl_c() => {
            log_shutdown_signal("SIGINT (Ctrl+C)");
        }
        _ = sigterm.recv() => {
            log_shutdown_signal("SIGTERM");
        }
        _ = sighup.recv() => {
            log_shutdown_signal("SIGHUP (Terminal Hangup)");
        }
    }

    info!("Shutdown signal received - Beginning graceful shutdown");

    // Stop ingest, then tell the handler to drain what's already queued and join
    // it, so in-flight updates are written before we flush, a deterministic
    // drain rather than a fixed sleep. The brief grace lets connection tasks'
    // final sends reach the channel; the timeout is a safety cap so shutdown can
    // never hang (the manager keeps a sender, so the channel won't close itself).
    info!("  • Stopping websocket ingest...");
    ws_manager.shutdown();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    handler_shutdown.notify_one();
    match tokio::time::timeout(std::time::Duration::from_secs(5), handler_join).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!("Message handler task ended abnormally: {}", e),
        Err(_) => warn!("Message handler did not drain within 5s; flushing anyway"),
    }

    info!("  • Flushing CSV buffers...");
    csv_writer.flush_all().await?;

    info!("  • Saving gap registry...");
    gap_registry.save_all_dirty().await?;

    info!("Shutdown complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_config_loading() {
        // The shipped example config must load (config.toml is user-local/gitignored).
        assert!(Config::from_file("config.example.toml").is_ok());
    }
}
