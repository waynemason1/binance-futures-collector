use anyhow::{Context, Result};
use dashmap::{DashMap, DashSet};
use log::{debug, error, info, warn};
use rustc_hash::FxHashMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::{interval, sleep};

use crate::config::Config;
use crate::constants;
use crate::models::*;
use crate::rest::RestClient;
use crate::utils::{CsvWriter, GapRegistry};
use crate::websocket::connection_manager::WebSocketManager;

/// Manages orderbook state and gap detection for all symbols
pub struct OrderbookManager {
    config: Arc<Config>,
    rest_client: Arc<RestClient>,
    csv_writer: Arc<CsvWriter>,
    websocket_manager: Arc<WebSocketManager>,
    gap_registry: Arc<GapRegistry>,

    // All symbol keys are lowercase
    symbol_states: Arc<DashMap<String, OrderbookState>>,

    // Uses f64 for prices/quantities, not String
    orderbooks: Arc<DashMap<String, Orderbook>>,

    // Snapshot scheduling
    last_snapshot_time: Arc<DashMap<String, Instant>>,
    last_periodic_snapshot: Arc<DashMap<String, Instant>>,
    sync_start_time: Arc<DashMap<String, Instant>>,

    // Error recovery tracking
    resnapshot_backoff: Arc<DashMap<String, Duration>>,

    // Symbols confirmed delisted by periodic discovery (shared with main and
    // the REST pollers). A member's manage_symbol task exits and drops state.
    delisted: Arc<DashSet<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OrderbookState {
    Initializing,
    FetchingSnapshot,
    Syncing,
    Live,
}

/// Which rule matched when bridging buffered diffs onto a REST snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
enum BridgeRule {
    /// U <= snapshot_u + 1: the diff continues exactly from the snapshot.
    PerfectContinuity,
    /// pu <= snapshot_u < U: the snapshot lands inside the diff's range.
    PuAttach,
}

impl BridgeRule {
    fn label(self) -> &'static str {
        match self {
            BridgeRule::PerfectContinuity => "perfect continuity",
            BridgeRule::PuAttach => "pu-based attach",
        }
    }
}

/// Find the first buffered diff that bridges onto a REST snapshot whose
/// lastUpdateId is `snapshot_u`, and which rule matched. Diffs fully covered by
/// the snapshot (final_update_id <= snapshot_u) are skipped. Pure so the
/// bridge decision can be unit-tested independently of the async attach path.
fn find_bridge(snapshot_u: u64, diffs: &[DepthUpdateWs]) -> Option<(usize, BridgeRule)> {
    for (i, msg) in diffs.iter().enumerate() {
        // Skip anything the snapshot already covers.
        if msg.final_update_id <= snapshot_u {
            continue;
        }
        // Rule 1: perfect continuity.
        if msg.first_update_id <= snapshot_u + 1 {
            return Some((i, BridgeRule::PerfectContinuity));
        }
        // Rule 2: the snapshot falls inside this diff's range.
        if msg.prev_update_id <= snapshot_u && snapshot_u < msg.first_update_id {
            return Some((i, BridgeRule::PuAttach));
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct Orderbook {
    pub symbol: String,
    pub last_update_id: u64,
    pub bids: HashMap<u64, f64>, // price_bits -> quantity (using float bits for fast comparison)
    pub asks: HashMap<u64, f64>, // price_bits -> quantity
    pub last_processed_time: Instant,
}

impl OrderbookManager {
    pub fn new(
        config: Arc<Config>,
        rest_client: Arc<RestClient>,
        csv_writer: Arc<CsvWriter>,
        websocket_manager: Arc<WebSocketManager>,
        gap_registry: Arc<GapRegistry>,
        delisted: Arc<DashSet<String>>,
    ) -> Self {
        Self {
            config,
            rest_client,
            csv_writer,
            websocket_manager,
            gap_registry,
            symbol_states: Arc::new(DashMap::new()),
            orderbooks: Arc::new(DashMap::new()),
            last_snapshot_time: Arc::new(DashMap::new()),
            last_periodic_snapshot: Arc::new(DashMap::new()),
            sync_start_time: Arc::new(DashMap::new()),
            resnapshot_backoff: Arc::new(DashMap::new()),
            delisted,
        }
    }

    /// Start the orderbook management tasks with coordinated staged startup
    ///
    /// If priority_symbol is provided (e.g., "btcusdt"), it will be initialized first
    /// before the normal batched initialization begins.
    pub async fn start(&self, symbols: Vec<String>, priority_symbol: Option<String>) -> Result<()> {
        debug_assert!(
            symbols.iter().all(|s| s == &s.to_lowercase()),
            "All symbols must be lowercase"
        );

        info!(
            "Orderbook Manager starting for {} symbols with synchronized staged startup.",
            symbols.len()
        );

        const BATCH_SIZE: usize = 20;
        let batch_gap_seconds = self.config.websocket.batch_gap_seconds;
        let snapshot_spread_seconds = 60; // Spread snapshot fetches over 60s per batch to prevent rate limiting
        let ws_buffer_delay_s = self.config.websocket.ws_buffer_delay_seconds;
        let batch_stability_min_messages = self.config.websocket.batch_stability_min_messages;

        // Handle priority symbol initialization (e.g., BTCUSDT)
        let mut remaining_symbols = symbols.clone();
        let mut batch_id_offset = 0;

        if let Some(priority_sym) = priority_symbol {
            info!(
                "PRIORITY INITIALIZATION: {} will start first",
                priority_sym.to_uppercase()
            );

            // Remove priority symbol from main list if present
            remaining_symbols.retain(|s| s != &priority_sym);

            info!(
                "Batch 0 (PRIORITY): Starting WebSocket for {} (klines, trades, orderbooks)...",
                priority_sym.to_uppercase()
            );

            // Order matters: the socket must be open and buffering before the REST
            // snapshot is taken, or the window between them is lost for good;
            // Binance never replays missed diffs.
            self.websocket_manager
                .start_batch(vec![priority_sym.clone()], 0)
                .await?;

            info!(
                "Batch 0: Waiting {}s for WebSocket buffering...",
                ws_buffer_delay_s
            );
            sleep(Duration::from_secs(ws_buffer_delay_s)).await;

            info!("Batch 0: Initializing orderbook for {}...", priority_sym);
            let symbol_clone = priority_sym.clone();
            let manager = self.clone();
            tokio::spawn(async move {
                if let Err(e) = manager.manage_symbol(symbol_clone.clone()).await {
                    error!("[{}] Symbol management task failed: {}", symbol_clone, e);
                }
            });

            // Block until it is Live: the remaining batches are paced off the
            // assumption that the priority symbol is already settled.
            info!(
                "Batch 0: Waiting for {} to reach LIVE state...",
                priority_sym
            );
            let priority_timeout_s = 30; // 30 second timeout for priority symbol
            let mut elapsed = 0u64;
            let check_interval = 1; // Check every 1 second

            while elapsed < priority_timeout_s {
                let state = self
                    .symbol_states
                    .get(&priority_sym)
                    .map(|s| s.clone())
                    .unwrap_or(OrderbookState::Initializing);

                if state == OrderbookState::Live {
                    info!(
                        "Batch 0: {} is now LIVE after {}s",
                        priority_sym.to_uppercase(),
                        elapsed
                    );
                    break;
                }

                sleep(Duration::from_secs(check_interval)).await;
                elapsed += check_interval;
            }

            if elapsed >= priority_timeout_s {
                warn!(
                    " Batch 0: {} did not reach LIVE state within {}s. Proceeding anyway.",
                    priority_sym.to_uppercase(),
                    priority_timeout_s
                );
            }

            info!(
                "Batch 0 complete. Waiting {}s before starting normal batched initialization...",
                batch_gap_seconds
            );
            sleep(Duration::from_secs(batch_gap_seconds)).await;

            batch_id_offset = 1; // Normal batches start from batch 1
        }

        let symbol_batches: Vec<Vec<String>> = remaining_symbols
            .chunks(BATCH_SIZE)
            .map(|chunk| chunk.to_vec())
            .collect();

        info!(
            "Will initialize {} symbols in {} batches of {}",
            remaining_symbols.len(),
            symbol_batches.len(),
            BATCH_SIZE
        );
        info!(
            "  - WebSocket start → {}s buffer → Snapshot fetch",
            ws_buffer_delay_s
        );
        info!("  - {}s gap between batches", batch_gap_seconds);
        info!(
            "  - Snapshot fetches staggered over {}s per batch to prevent proxy overload",
            snapshot_spread_seconds
        );
        info!(
            "  - Stability check: Wait for {}+ messages per symbol before next batch",
            batch_stability_min_messages
        );

        let manager_clone = self.clone();
        let symbols_clone = symbols.clone();
        tokio::spawn(async move {
            manager_clone.periodic_snapshot_task(symbols_clone).await;
        });

        // Start live depth update loop (applies WebSocket updates to in-memory orderbooks)
        let manager_live = self.clone();
        tokio::spawn(async move {
            manager_live.run_live_depth_update_loop().await;
        });
        info!("Live depth update loop started (500ms interval)");

        // Start periodic cleanup task (every 5 minutes)
        let manager_cleanup = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(300)); // 5 min
            loop {
                interval.tick().await;
                manager_cleanup.cleanup_stale_symbols(std::time::Instant::now());
            }
        });
        info!("Stale symbol cleanup task started (5min interval)");

        for (batch_idx, batch) in symbol_batches.iter().enumerate() {
            let batch_id = batch_idx + batch_id_offset;
            let total_batches = symbol_batches.len() + batch_id_offset;
            info!(
                "Batch {}/{}: Starting WebSocket for {} symbols...",
                batch_id,
                total_batches,
                batch.len()
            );

            // Same ordering as batch 0: sockets first, snapshots second.
            self.websocket_manager
                .start_batch(batch.clone(), batch_id)
                .await?;

            info!(
                "Batch {}: Waiting {}s for WebSocket buffering...",
                batch_id, ws_buffer_delay_s
            );
            sleep(Duration::from_secs(ws_buffer_delay_s)).await;

            info!(
                "Batch {}: Initializing orderbooks with staggered snapshot fetches...",
                batch_id
            );
            let delay_per_symbol = snapshot_spread_seconds as f64 / batch.len() as f64;

            for (idx, symbol) in batch.iter().enumerate() {
                // Stagger snapshot fetches
                if idx > 0 {
                    sleep(Duration::from_secs_f64(delay_per_symbol)).await;
                }

                let symbol_clone = symbol.clone();
                let manager = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = manager.manage_symbol(symbol_clone.clone()).await {
                        error!("[{}] Symbol management task failed: {}", symbol_clone, e);
                    }
                });
            }

            // Let the batch settle before opening the next one, so a slow batch
            // can't stack its snapshot fetches on top of the following one.
            if batch_id < total_batches - 1 {
                info!("Batch {} complete. Checking stability (waiting for {}+ messages or {}s timeout)...",
                      batch_id, batch_stability_min_messages, batch_gap_seconds);

                let stability_check_interval = 2; // Check every 2 seconds
                let mut elapsed = 0u64;
                let mut stable = false;

                while elapsed < batch_gap_seconds {
                    if self
                        .websocket_manager
                        .check_batch_stability(batch, batch_stability_min_messages)
                    {
                        stable = true;
                        info!(
                            "Batch {} is stable after {}s. All symbols processed {}+ messages.",
                            batch_id, elapsed, batch_stability_min_messages
                        );
                        break;
                    }
                    sleep(Duration::from_secs(stability_check_interval)).await;
                    elapsed += stability_check_interval;
                }

                if !stable {
                    info!(
                        "Batch {} stability timeout after {}s. Proceeding to next batch.",
                        batch_id, batch_gap_seconds
                    );
                }

                // Ensure we always wait at least the remaining time to hit batch_gap_seconds
                let remaining = batch_gap_seconds.saturating_sub(elapsed);
                if remaining > 0 {
                    sleep(Duration::from_secs(remaining)).await;
                }
            } else {
                info!("Batch {} complete. All batches initialized.", batch_id);
            }
        }

        info!("All symbols initialized. System running in production mode.");
        Ok(())
    }

    /// Manage a single symbol's orderbook lifecycle
    async fn manage_symbol(&self, symbol: String) -> Result<()> {
        loop {
            // A confirmed delisting ends this symbol's lifecycle: drop its
            // state and exit, rather than retrying snapshots against a dead
            // market forever. (Without this, the loop below would resurrect
            // the state that cleanup dropped and error-loop until restart.)
            if self.delisted.contains(&symbol) {
                info!(
                    "[{}] Delisted: stopping order-book task and dropping state",
                    symbol
                );
                self.remove_symbol_state(&symbol);
                return Ok(());
            }

            let state = self
                .symbol_states
                .get(&symbol)
                .map(|s| s.clone())
                .unwrap_or(OrderbookState::Initializing);

            match state {
                OrderbookState::Initializing => {
                    info!(
                        "[{}] State: INITIALIZING. Proceeding to fetch snapshot.",
                        symbol
                    );
                    self.symbol_states
                        .insert(symbol.clone(), OrderbookState::FetchingSnapshot);
                }

                OrderbookState::FetchingSnapshot => {
                    // Check resync cooldown. Compute the remaining wait in its
                    // own scope so the DashMap guard is dropped BEFORE the
                    // sleep. Holding it across an await would block every
                    // writer to this shard (cleanup, delist teardown) for up
                    // to the whole cooldown, and can deadlock the runtime if
                    // all workers end up parked on that lock.
                    let cooldown =
                        Duration::from_secs(self.config.orderbook.resync_cooldown_seconds);
                    let wait = self
                        .last_snapshot_time
                        .get(&symbol)
                        .map(|last| cooldown.saturating_sub(last.elapsed()));
                    if let Some(wait) = wait.filter(|w| !w.is_zero()) {
                        sleep(wait).await;
                    }

                    info!("[{}] State: FETCHING_SNAPSHOT.", symbol);
                    self.last_snapshot_time
                        .insert(symbol.clone(), Instant::now());

                    match self
                        .fetch_and_apply_snapshot(&symbol, "depth_snapshot_initial")
                        .await
                    {
                        Ok(_) => {
                            self.symbol_states
                                .insert(symbol.clone(), OrderbookState::Syncing);
                            self.sync_start_time.insert(symbol.clone(), Instant::now());

                            self.resnapshot_backoff.insert(
                                symbol.clone(),
                                Duration::from_secs(self.config.orderbook.resync_cooldown_seconds),
                            );
                        }
                        Err(e) => {
                            error!(
                                "[{}] Failed to fetch snapshot: {}. Retrying with backoff.",
                                symbol, e
                            );

                            let current_backoff =
                                self.resnapshot_backoff.get(&symbol).map(|b| *b).unwrap_or(
                                    Duration::from_secs(
                                        self.config.orderbook.resync_cooldown_seconds,
                                    ),
                                );

                            let new_backoff = std::cmp::min(
                                current_backoff
                                    .mul_f64(self.config.orderbook.resync_backoff_factor),
                                Duration::from_secs(
                                    self.config.orderbook.max_resync_backoff_seconds,
                                ),
                            );

                            self.resnapshot_backoff.insert(symbol.clone(), new_backoff);
                            sleep(new_backoff).await;

                            self.symbol_states
                                .insert(symbol.clone(), OrderbookState::Initializing);
                        }
                    }
                }

                OrderbookState::Syncing => {
                    let elapsed = self
                        .sync_start_time
                        .get(&symbol)
                        .map(|t| t.elapsed())
                        .unwrap_or(Duration::from_secs(0));

                    let timeout =
                        Duration::from_secs(self.config.orderbook.attach_bridge_timeout_seconds);

                    if elapsed > timeout {
                        warn!(
                            "[{}] Bridge rule timeout after {}s. Forcing resync.",
                            symbol,
                            timeout.as_secs()
                        );
                        self.trigger_resnapshot(&symbol, "bridge_rule_timeout")
                            .await?;
                    } else {
                        match self.try_attach_to_stream(&symbol).await {
                            Ok(true) => {
                                info!(
                                    "[{}] Transitioned to LIVE state after successful attach",
                                    symbol
                                );
                                self.symbol_states
                                    .insert(symbol.clone(), OrderbookState::Live);

                                self.write_current_orderbook_snapshot(
                                    &symbol,
                                    "depth_snapshot_bridge",
                                )
                                .await?;
                                info!("[{}] depth_snapshot_bridge written after attach", symbol);
                            }
                            Ok(false) => {
                                // Not ready yet, wait and retry
                                sleep(Duration::from_millis(500)).await;
                            }
                            Err(e) => {
                                error!("[{}] Attach failed: {}", symbol, e);
                                sleep(Duration::from_millis(500)).await;
                            }
                        }
                    }
                }

                OrderbookState::Live => {
                    // In LIVE state, just monitor (updates are processed in background)
                    sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }

    async fn fetch_and_apply_snapshot(&self, symbol: &str, event_type: &str) -> Result<()> {
        info!("[{}] Fetching snapshot ({})", symbol, event_type);

        self.symbol_states
            .insert(symbol.to_string(), OrderbookState::FetchingSnapshot);

        let snapshot = tokio::time::timeout(
            Duration::from_secs(self.config.orderbook.snapshot_http_timeout_seconds),
            self.rest_client
                .get_depth_snapshot(symbol, self.config.orderbook.depth_limit),
        )
        .await
        .context("Snapshot fetch timeout")?
        .context("Snapshot fetch failed")?;

        // Create orderbook with float storage (not String)
        let mut bids = HashMap::new();
        for [price_str, qty_str] in &snapshot.bids {
            let price: f64 = price_str.parse()?;
            let qty: f64 = qty_str.parse()?;
            bids.insert(price.to_bits(), qty);
        }

        let mut asks = HashMap::new();
        for [price_str, qty_str] in &snapshot.asks {
            let price: f64 = price_str.parse()?;
            let qty: f64 = qty_str.parse()?;
            asks.insert(price.to_bits(), qty);
        }

        let orderbook = Orderbook {
            symbol: symbol.to_string(),
            last_update_id: snapshot.last_update_id,
            bids,
            asks,
            last_processed_time: Instant::now(),
        };

        self.orderbooks.insert(symbol.to_string(), orderbook);

        self.write_snapshot_csv(&snapshot, symbol, event_type, snapshot.last_update_id)
            .await?;

        Ok(())
    }

    /// Try to attach to live stream after snapshot
    async fn try_attach_to_stream(&self, symbol: &str) -> Result<bool> {
        let buffered = self.websocket_manager.drain_buffer(symbol).await;

        if buffered.is_empty() {
            debug!("[{}] No buffered updates yet for attach", symbol);
            return Ok(false);
        }

        let snapshot_u = self
            .orderbooks
            .get(symbol)
            .map(|b| b.last_update_id)
            .unwrap_or(0);

        let normalized = Self::normalize_messages_for_attach(buffered);

        if normalized.is_empty() {
            warn!("[{}] Normalization resulted in empty buffer", symbol);
            return Ok(false);
        }

        // Check if snapshot is too old to attach ANY buffered message
        let first_pu = normalized.first().map(|m| m.prev_update_id).unwrap_or(0);
        if snapshot_u < first_pu {
            let gap = first_pu - snapshot_u;
            warn!("[{}] Snapshot too old to attach ANY buffered message. snapshot_u={} < first_pu={}, gap={}. Triggering immediate resnapshot.",
                  symbol, snapshot_u, first_pu, gap);
            self.trigger_resnapshot(symbol, "snapshot_older_than_buffer")
                .await?;
            return Ok(false);
        }

        // Find the diff that bridges onto the snapshot (pure; see find_bridge).
        if let Some((idx, rule)) = find_bridge(snapshot_u, &normalized) {
            let m = &normalized[idx];
            info!(
                "[{}] Found bridging message at index {}: U={}, u={}, pu={}, snapshot_u={} ({})",
                symbol,
                idx,
                m.first_update_id,
                m.final_update_id,
                m.prev_update_id,
                snapshot_u,
                rule.label()
            );

            // Apply updates from bridge onwards
            let mut last_u = snapshot_u;
            let mut applied = 0;

            for (i, update) in normalized[idx..].iter().enumerate() {
                // Verify continuity using pu AFTER first message
                if i > 0 && update.prev_update_id != last_u {
                    warn!(
                        "[{}] Gap during attach: expected pu={}, got pu={} (U={}, u={})",
                        symbol,
                        last_u,
                        update.prev_update_id,
                        update.first_update_id,
                        update.final_update_id
                    );
                    return Ok(false);
                }

                // Skip if message is fully covered by snapshot
                if update.final_update_id <= snapshot_u {
                    continue;
                }

                self.apply_diff(symbol, update).await?;
                last_u = update.final_update_id;
                applied += 1;
            }

            if applied > 0 {
                info!(
                    "[{}] Attach successful: processed {} messages, last_u={}",
                    symbol, applied, last_u
                );
                return Ok(true);
            }
        } else {
            // No bridge found - check if snapshot is STALE
            if let Some(first_msg) = normalized.first() {
                let first_pu = first_msg.prev_update_id;
                if first_pu > 0 {
                    let gap_size = first_pu.saturating_sub(snapshot_u);

                    if gap_size > 10000 {
                        warn!("[{}] STALE SNAPSHOT DETECTED: snapshot_u={}, first_pu={}, gap={}. Triggering immediate resnapshot",
                              symbol, snapshot_u, first_pu, gap_size);
                        self.trigger_resnapshot(symbol, "stale_snapshot_unbridgeable")
                            .await?;
                        return Ok(false);
                    }
                }
            }

            debug!(
                "[{}] No bridging update found - snapshot may be too fresh, will retry",
                symbol
            );
        }

        Ok(false)
    }

    fn normalize_messages_for_attach(msgs: Vec<DepthUpdateWs>) -> Vec<DepthUpdateWs> {
        if msgs.is_empty() {
            return Vec::new();
        }

        let mut sorted = msgs;
        sorted.sort_by_key(|m| (m.first_update_id, m.final_update_id, m.event_time));

        // Deduplicate by 'u': keep record with largest 'E'
        let mut dedup: HashMap<u64, DepthUpdateWs> = HashMap::new();
        for msg in sorted {
            let u = msg.final_update_id;
            match dedup.get(&u) {
                Some(existing) if msg.event_time > existing.event_time => {
                    dedup.insert(u, msg);
                }
                None => {
                    dedup.insert(u, msg);
                }
                _ => {}
            }
        }

        let mut out: Vec<_> = dedup.into_values().collect();
        out.sort_by_key(|m| m.final_update_id);
        out
    }

    async fn apply_diff(&self, symbol: &str, update: &DepthUpdateWs) -> Result<()> {
        if let Some(mut book) = self.orderbooks.get_mut(symbol) {
            Self::apply_update_to_book(&mut book, update)?;

            // Cap each book to depth_limit levels so it can't grow unbounded
            // (rationale + numbers on trim_book_to_depth).
            Self::trim_book_to_depth(self.config.orderbook.depth_limit as usize, &mut book);
        }

        Ok(())
    }

    /// Apply one diff update to a book: quantity 0 deletes the level, anything
    /// else replaces it. Uses the pre-converted floats when present, falling
    /// back to parsing the wire strings.
    fn apply_update_to_book(book: &mut Orderbook, update: &DepthUpdateWs) -> Result<()> {
        if let Some(b_floats) = &update.b_floats {
            for (price, qty) in b_floats {
                if *qty == 0.0 {
                    book.bids.remove(&price.to_bits());
                } else {
                    book.bids.insert(price.to_bits(), *qty);
                }
            }
        } else {
            // Fallback: parse from strings
            for [price_str, qty_str] in &update.bids {
                let price: f64 = price_str.parse()?;
                let qty: f64 = qty_str.parse()?;
                if qty == 0.0 {
                    book.bids.remove(&price.to_bits());
                } else {
                    book.bids.insert(price.to_bits(), qty);
                }
            }
        }

        if let Some(a_floats) = &update.a_floats {
            for (price, qty) in a_floats {
                if *qty == 0.0 {
                    book.asks.remove(&price.to_bits());
                } else {
                    book.asks.insert(price.to_bits(), *qty);
                }
            }
        } else {
            // Fallback: parse from strings
            for [price_str, qty_str] in &update.asks {
                let price: f64 = price_str.parse()?;
                let qty: f64 = qty_str.parse()?;
                if qty == 0.0 {
                    book.asks.remove(&price.to_bits());
                } else {
                    book.asks.insert(price.to_bits(), qty);
                }
            }
        }

        book.last_update_id = update.final_update_id;
        book.last_processed_time = Instant::now();
        Ok(())
    }

    /// Trim orderbook to the given depth limit to prevent memory leak
    /// Without this, orderbooks accumulate all historical price levels
    /// Memory impact: 512 symbols × 10k levels × 32 bytes = ~163MB (plus HashMap overhead)
    /// After trim: 512 symbols × 100 levels × 32 bytes = ~1.6MB (99% reduction)
    fn trim_book_to_depth(depth: usize, book: &mut Orderbook) {
        // Trim bids: keep only top N (highest prices)
        if book.bids.len() > depth {
            // Convert to sorted vec (descending by price)
            let mut bid_vec: Vec<(u64, f64)> = book.bids.iter().map(|(k, v)| (*k, *v)).collect();

            bid_vec.sort_by(|a, b| {
                let price_a = f64::from_bits(a.0);
                let price_b = f64::from_bits(b.0);
                price_b.total_cmp(&price_a)
            });

            book.bids.clear();
            for (price_bits, qty) in bid_vec.into_iter().take(depth) {
                book.bids.insert(price_bits, qty);
            }
        }

        // Trim asks: keep only top N (lowest prices)
        if book.asks.len() > depth {
            // Convert to sorted vec (ascending by price)
            let mut ask_vec: Vec<(u64, f64)> = book.asks.iter().map(|(k, v)| (*k, *v)).collect();

            ask_vec.sort_by(|a, b| {
                let price_a = f64::from_bits(a.0);
                let price_b = f64::from_bits(b.0);
                price_a.total_cmp(&price_b)
            });

            book.asks.clear();
            for (price_bits, qty) in ask_vec.into_iter().take(depth) {
                book.asks.insert(price_bits, qty);
            }
        }
    }

    /// Remove stale symbols not updated in last 2 hours
    /// This prevents unbounded growth from delisted/inactive symbols
    fn cleanup_stale_symbols(&self, now: Instant) {
        const MAX_IDLE_SECS: u64 = 7200; // 2 hours

        // Find stale symbols (not updated in 2 hours)
        let stale: Vec<String> = self
            .orderbooks
            .iter()
            .filter_map(|entry| {
                let (symbol, book) = (entry.key(), entry.value());
                if now.duration_since(book.last_processed_time).as_secs() > MAX_IDLE_SECS {
                    Some(symbol.clone())
                } else {
                    None
                }
            })
            .collect();

        if stale.is_empty() {
            return;
        }

        for symbol in &stale {
            self.remove_symbol_state(symbol);
        }

        info!(
            "Cleaned up {} stale symbols from OrderbookManager: {:?}",
            stale.len(),
            stale
        );
    }

    /// Drop every piece of per-symbol state this manager holds. Used by the
    /// stale-symbol sweep and by a symbol's own task when it exits on delist.
    fn remove_symbol_state(&self, symbol: &str) {
        self.orderbooks.remove(symbol);
        self.symbol_states.remove(symbol);
        self.last_snapshot_time.remove(symbol);
        self.last_periodic_snapshot.remove(symbol);
        self.sync_start_time.remove(symbol);
        self.resnapshot_backoff.remove(symbol);
    }

    async fn trigger_resnapshot(&self, symbol: &str, reason: &str) -> Result<()> {
        warn!("[{}] Triggering resnapshot: {}", symbol, reason);

        self.gap_registry
            .record_resnapshot(symbol, reason.to_string())
            .await;

        // Reset to INITIALIZING (will refetch snapshot)
        self.symbol_states
            .insert(symbol.to_string(), OrderbookState::Initializing);

        Ok(())
    }

    /// Write current orderbook state to CSV (without fetching new snapshot)
    async fn write_current_orderbook_snapshot(&self, symbol: &str, event_type: &str) -> Result<()> {
        let (last_update_id, bids, asks) = match self.orderbooks.get(symbol) {
            Some(book) => {
                // Convert back to f64 for CSV writing
                let bids: Vec<(f64, f64)> = book
                    .bids
                    .iter()
                    .map(|(bits, qty)| (f64::from_bits(*bits), *qty))
                    .collect();
                let asks: Vec<(f64, f64)> = book
                    .asks
                    .iter()
                    .map(|(bits, qty)| (f64::from_bits(*bits), *qty))
                    .collect();
                (book.last_update_id, bids, asks)
            }
            None => {
                warn!("[{}] Cannot write snapshot - orderbook not found", symbol);
                return Ok(());
            }
        };

        let timestamp = chrono::Utc::now().timestamp_millis();
        let mut csv_rows = Vec::new();

        let mut bid_prices = bids;
        bid_prices.sort_by(|a, b| b.0.total_cmp(&a.0));

        for (price, qty) in bid_prices {
            csv_rows.push(DepthSnapshotCsv {
                exchange: constants::EXCHANGE.to_string(),
                market: constants::MARKET.to_string(),
                datatype: constants::DATATYPE_ORDERBOOKS.to_string(),
                timestamp_ms: timestamp,
                symbol: symbol.to_lowercase(), // Convert to lowercase for CSV
                event_type: event_type.to_string(),
                side: constants::SIDE_BID.to_string(),
                price: format_decimal(price),
                quantity: format_decimal(qty),
                last_update_id,
            });
        }

        let mut ask_prices = asks;
        ask_prices.sort_by(|a, b| a.0.total_cmp(&b.0));

        for (price, qty) in ask_prices {
            csv_rows.push(DepthSnapshotCsv {
                exchange: constants::EXCHANGE.to_string(),
                market: constants::MARKET.to_string(),
                datatype: constants::DATATYPE_ORDERBOOKS.to_string(),
                timestamp_ms: timestamp,
                symbol: symbol.to_lowercase(), // Convert to lowercase for CSV
                event_type: event_type.to_string(),
                side: constants::SIDE_ASK.to_string(),
                price: format_decimal(price),
                quantity: format_decimal(qty),
                last_update_id,
            });
        }

        self.csv_writer.write_depth_snapshot(csv_rows).await?;
        // Per-symbol detail at debug; at 500 symbols the periodic task emits one
        // aggregate line at info instead (see periodic_snapshot_task).
        debug!("[{}] Wrote {} snapshot to CSV", symbol, event_type);

        Ok(())
    }

    /// Write snapshot to CSV from REST response
    async fn write_snapshot_csv(
        &self,
        snapshot: &DepthSnapshotRest,
        symbol: &str,
        event_type: &str,
        last_update_id: u64,
    ) -> Result<()> {
        let timestamp = chrono::Utc::now().timestamp_millis();
        let mut csv_rows = Vec::new();

        let mut bid_prices: Vec<_> = snapshot.bids.iter().collect();
        bid_prices.sort_by(|a, b| {
            b[0].parse::<f64>()
                .unwrap_or(0.0)
                .total_cmp(&a[0].parse::<f64>().unwrap_or(0.0))
        });

        for [price, qty] in bid_prices {
            csv_rows.push(DepthSnapshotCsv {
                exchange: constants::EXCHANGE.to_string(),
                market: constants::MARKET.to_string(),
                datatype: constants::DATATYPE_ORDERBOOKS.to_string(),
                timestamp_ms: timestamp,
                symbol: symbol.to_lowercase(), // Convert to lowercase for CSV
                event_type: event_type.to_string(),
                side: constants::SIDE_BID.to_string(),
                price: format_decimal_str(price),
                quantity: format_decimal_str(qty),
                last_update_id,
            });
        }

        let mut ask_prices: Vec<_> = snapshot.asks.iter().collect();
        ask_prices.sort_by(|a, b| {
            a[0].parse::<f64>()
                .unwrap_or(0.0)
                .total_cmp(&b[0].parse::<f64>().unwrap_or(0.0))
        });

        for [price, qty] in ask_prices {
            csv_rows.push(DepthSnapshotCsv {
                exchange: constants::EXCHANGE.to_string(),
                market: constants::MARKET.to_string(),
                datatype: constants::DATATYPE_ORDERBOOKS.to_string(),
                timestamp_ms: timestamp,
                symbol: symbol.to_lowercase(), // Convert to lowercase for CSV
                event_type: event_type.to_string(),
                side: constants::SIDE_ASK.to_string(),
                price: format_decimal_str(price),
                quantity: format_decimal_str(qty),
                last_update_id,
            });
        }

        self.csv_writer.write_depth_snapshot(csv_rows).await?;
        Ok(())
    }

    async fn periodic_snapshot_task(&self, symbols: Vec<String>) {
        let snapshot_interval =
            Duration::from_secs(self.config.orderbook.snapshot_interval_seconds);
        let mut interval = interval(snapshot_interval);

        loop {
            interval.tick().await;

            let live_symbols: Vec<_> = symbols
                .iter()
                .filter(|s| {
                    self.symbol_states
                        .get(*s)
                        .map(|state| *state == OrderbookState::Live)
                        .unwrap_or(false)
                })
                .collect();

            if live_symbols.is_empty() {
                continue;
            }

            // Spread snapshots over 180 seconds
            let spread_window_s = 180.0;
            let mut attempted = live_symbols.len();
            let delay_per_symbol = spread_window_s / attempted as f64;
            let mut written = 0usize;

            for (i, symbol) in live_symbols.iter().enumerate() {
                if i > 0 && delay_per_symbol > 0.0 {
                    sleep(Duration::from_secs_f64(delay_per_symbol)).await;
                }

                // The live list was snapshotted at cycle start and this loop
                // spreads over minutes, so skip any symbol delisted meanwhile,
                // so its teardown isn't undone by a zombie map re-insert.
                if self.delisted.contains(symbol.as_str()) {
                    attempted -= 1;
                    continue;
                }

                match self
                    .write_current_orderbook_snapshot(symbol, "depth_snapshot")
                    .await
                {
                    Ok(()) => written += 1,
                    Err(e) => error!("[{}] Periodic snapshot write failed: {}", symbol, e),
                }

                self.last_periodic_snapshot
                    .insert(symbol.to_string(), Instant::now());
            }

            // One aggregate line per cycle at info (per-symbol detail is at debug);
            // escalate to warn if any symbol failed to write.
            if written < attempted {
                warn!(
                    "Snapshots: {}/{} written this cycle over {:.0}s ({} failed)",
                    written,
                    attempted,
                    spread_window_s,
                    attempted - written
                );
            } else {
                info!(
                    "Snapshots: {}/{} written this cycle over {:.0}s",
                    written, attempted, spread_window_s
                );
            }
        }
    }

    /// Continuously apply live depth updates to in-memory orderbooks
    /// This task runs after symbols reach OrderbookState::Live, ensuring
    /// orderbooks stay synchronized with the WebSocket stream.
    async fn run_live_depth_update_loop(&self) {
        use tokio::time::{interval, Duration};

        let mut ticker = interval(Duration::from_millis(500));

        loop {
            ticker.tick().await;

            // Get all symbols currently in LIVE state
            let live_symbols: Vec<String> = self
                .symbol_states
                .iter()
                .filter_map(|entry| {
                    let (sym, state) = (entry.key(), entry.value());
                    if *state == OrderbookState::Live {
                        Some(sym.clone())
                    } else {
                        None
                    }
                })
                .collect();

            // Apply updates for each live symbol
            for symbol in live_symbols {
                // Drain new updates from WebSocket ring buffer
                let updates = self.websocket_manager.drain_buffer(&symbol).await;

                if updates.is_empty() {
                    continue;
                }

                // Deliberately no pu==last_update_id continuity guard or resnapshot
                // here. A WS reconnect drops a whole batch of symbols at once, so
                // resnapshotting on each gap would fire hundreds of REST snapshots in
                // a burst, blowing the weight budget and cascading into a resync
                // storm. Instead the design absorbs a reconnect three ways:
                //   • the raw depth_updates CSV stays lossless, so a reconnect gap is
                //     a recorded pu-break the offline validator surfaces (the
                //     dashboard's `continuity_breaks`). It is detected, not silent;
                //   • the in-memory book self-heals: Binance diffs carry ABSOLUTE
                //     quantities, so every level is corrected on its next update;
                //   • reconstruction re-anchors on the newest snapshot, not this live
                //     book. Resync stays confined to init/bridge, rate-limited by
                //     cooldown + backoff.
                for update in &updates {
                    if let Err(e) = self.apply_diff(&symbol, update).await {
                        error!("[{}] Failed to apply live depth update: {}", symbol, e);
                    }
                }

                debug!("[{}] Applied {} live depth updates", symbol, updates.len());
            }
        }
    }

    /// Get orderbook statistics
    pub fn get_stats(&self) -> FxHashMap<String, OrderbookStats> {
        let mut stats = FxHashMap::default();

        for entry in self.symbol_states.iter() {
            let symbol = entry.key();
            let state = entry.value();

            let book_info = self
                .orderbooks
                .get(symbol)
                .map(|book| (book.last_update_id, book.bids.len(), book.asks.len()))
                .unwrap_or((0, 0, 0));

            stats.insert(
                symbol.clone(),
                OrderbookStats {
                    symbol: symbol.clone(),
                    state: format!("{:?}", state),
                    last_update_id: book_info.0,
                    bid_levels: book_info.1,
                    ask_levels: book_info.2,
                },
            );
        }

        stats
    }
}

#[derive(Debug, Clone)]
pub struct OrderbookStats {
    pub symbol: String,
    pub state: String,
    pub last_update_id: u64,
    pub bid_levels: usize,
    pub ask_levels: usize,
}

impl Clone for OrderbookManager {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            rest_client: self.rest_client.clone(),
            csv_writer: self.csv_writer.clone(),
            websocket_manager: self.websocket_manager.clone(),
            gap_registry: self.gap_registry.clone(),
            symbol_states: self.symbol_states.clone(),
            orderbooks: self.orderbooks.clone(),
            last_snapshot_time: self.last_snapshot_time.clone(),
            last_periodic_snapshot: self.last_periodic_snapshot.clone(),
            sync_start_time: self.sync_start_time.clone(),
            resnapshot_backoff: self.resnapshot_backoff.clone(),
            delisted: self.delisted.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_book() -> Orderbook {
        Orderbook {
            symbol: "testusdt".to_string(),
            last_update_id: 0,
            bids: HashMap::new(),
            asks: HashMap::new(),
            last_processed_time: Instant::now(),
        }
    }

    fn update(u: u64, bids: &[(f64, f64)], asks: &[(f64, f64)]) -> DepthUpdateWs {
        DepthUpdateWs {
            event_type: "depthUpdate".to_string(),
            event_time: u as i64,
            transaction_time: u as i64,
            symbol: "TESTUSDT".to_string(),
            first_update_id: u,
            final_update_id: u,
            prev_update_id: u.saturating_sub(1),
            bids: bids
                .iter()
                .map(|(p, q)| [p.to_string(), q.to_string()])
                .collect(),
            asks: asks
                .iter()
                .map(|(p, q)| [p.to_string(), q.to_string()])
                .collect(),
            b_floats: None,
            a_floats: None,
        }
    }

    // A diff with explicit U / u / pu for bridge-rule tests.
    fn bridge_diff(first_u: u64, final_u: u64, prev_u: u64) -> DepthUpdateWs {
        let mut d = update(final_u, &[], &[]);
        d.first_update_id = first_u;
        d.final_update_id = final_u;
        d.prev_update_id = prev_u;
        d
    }

    #[test]
    fn bridge_rule1_perfect_continuity() {
        // snapshot_u = 100; a diff starting at U = 101 (== snapshot_u + 1) bridges.
        let diffs = [bridge_diff(101, 110, 100)];
        assert_eq!(
            find_bridge(100, &diffs),
            Some((0, BridgeRule::PerfectContinuity))
        );
    }

    #[test]
    fn bridge_rule2_pu_attach() {
        // Rule 1 fails (U = 110 > snapshot_u + 1), but the snapshot at 105 falls
        // inside the pu..U window (pu = 100 <= 105 < 110) → pu-attach.
        let diffs = [bridge_diff(110, 120, 100)];
        assert_eq!(find_bridge(105, &diffs), Some((0, BridgeRule::PuAttach)));
    }

    #[test]
    fn bridge_skips_diffs_covered_by_snapshot() {
        // The first two diffs end at/before snapshot_u = 100; the third bridges.
        let diffs = [
            bridge_diff(90, 95, 89),
            bridge_diff(96, 100, 95),
            bridge_diff(101, 110, 100),
        ];
        assert_eq!(
            find_bridge(100, &diffs),
            Some((2, BridgeRule::PerfectContinuity))
        );
    }

    #[test]
    fn bridge_none_when_snapshot_unbridgeable() {
        // A hole: snapshot ends at 100 but the buffer starts at U = 200 (pu = 199),
        // so neither rule matches: the snapshot is too old to attach.
        let diffs = [bridge_diff(200, 210, 199)];
        assert_eq!(find_bridge(100, &diffs), None);
    }

    fn best_bid(book: &Orderbook) -> Option<f64> {
        book.bids
            .keys()
            .map(|b| f64::from_bits(*b))
            .max_by(|a, b| a.total_cmp(b))
    }

    fn best_ask(book: &Orderbook) -> Option<f64> {
        book.asks
            .keys()
            .map(|b| f64::from_bits(*b))
            .min_by(|a, b| a.total_cmp(b))
    }

    #[test]
    fn diff_inserts_updates_and_removes_levels() {
        let mut book = empty_book();

        // Insert two levels each side.
        let u1 = update(
            10,
            &[(100.0, 1.5), (99.5, 2.0)],
            &[(100.5, 1.0), (101.0, 3.0)],
        );
        OrderbookManager::apply_update_to_book(&mut book, &u1).unwrap();
        assert_eq!(book.bids.len(), 2);
        assert_eq!(book.asks.len(), 2);
        assert_eq!(book.last_update_id, 10);
        assert_eq!(best_bid(&book), Some(100.0));
        assert_eq!(best_ask(&book), Some(100.5));

        // Replace a quantity and delete a level with qty 0.
        let u2 = update(11, &[(100.0, 0.7), (99.5, 0.0)], &[]);
        OrderbookManager::apply_update_to_book(&mut book, &u2).unwrap();
        assert_eq!(book.bids.len(), 1);
        assert_eq!(book.bids[&100.0f64.to_bits()], 0.7);
        assert_eq!(book.last_update_id, 11);
    }

    #[test]
    fn diff_prefers_preconverted_floats() {
        let mut book = empty_book();
        let mut u = update(5, &[(1.0, 1.0)], &[]);
        // When floats are present, the string levels must be ignored.
        u.b_floats = Some(vec![(2.0, 9.0)]);
        OrderbookManager::apply_update_to_book(&mut book, &u).unwrap();
        assert_eq!(book.bids.len(), 1);
        assert_eq!(book.bids[&2.0f64.to_bits()], 9.0);
    }

    #[test]
    fn diff_rejects_unparseable_levels() {
        let mut book = empty_book();
        let mut u = update(5, &[], &[]);
        u.bids = vec![["not-a-price".to_string(), "1.0".to_string()]];
        assert!(OrderbookManager::apply_update_to_book(&mut book, &u).is_err());
    }

    #[test]
    fn trim_keeps_best_levels_on_both_sides() {
        let mut book = empty_book();
        // 10 bids at 90..99 and 10 asks at 101..110.
        for i in 0..10 {
            book.bids.insert((90.0 + i as f64).to_bits(), 1.0);
            book.asks.insert((101.0 + i as f64).to_bits(), 1.0);
        }

        OrderbookManager::trim_book_to_depth(3, &mut book);

        // Bids keep the HIGHEST prices, asks the LOWEST, so the touch survives.
        let mut bids: Vec<f64> = book.bids.keys().map(|b| f64::from_bits(*b)).collect();
        let mut asks: Vec<f64> = book.asks.keys().map(|b| f64::from_bits(*b)).collect();
        bids.sort_by(|a, b| a.total_cmp(b));
        asks.sort_by(|a, b| a.total_cmp(b));
        assert_eq!(bids, vec![97.0, 98.0, 99.0]);
        assert_eq!(asks, vec![101.0, 102.0, 103.0]);
        assert!(best_bid(&book).unwrap() < best_ask(&book).unwrap());
    }

    #[test]
    fn trim_is_noop_within_depth() {
        let mut book = empty_book();
        book.bids.insert(100.0f64.to_bits(), 1.0);
        book.asks.insert(101.0f64.to_bits(), 1.0);
        OrderbookManager::trim_book_to_depth(50, &mut book);
        assert_eq!(book.bids.len(), 1);
        assert_eq!(book.asks.len(), 1);
    }

    #[test]
    fn normalize_sorts_and_dedups_buffered_updates() {
        // Out of order, with a duplicate u=20 where the later event_time (E=201)
        // must win over the earlier one (E=200).
        let mut dup_early = update(20, &[(1.0, 1.0)], &[]);
        dup_early.event_time = 200;
        let mut dup_late = update(20, &[(2.0, 2.0)], &[]);
        dup_late.event_time = 201;

        let msgs = vec![
            update(30, &[], &[]),
            dup_early,
            update(10, &[], &[]),
            dup_late,
        ];
        let out = OrderbookManager::normalize_messages_for_attach(msgs);

        let ids: Vec<u64> = out.iter().map(|m| m.final_update_id).collect();
        assert_eq!(ids, vec![10, 20, 30]);
        let kept = out.iter().find(|m| m.final_update_id == 20).unwrap();
        assert_eq!(kept.event_time, 201);
        assert_eq!(kept.bids[0][0], "2");
    }

    #[test]
    fn normalize_empty_is_empty() {
        assert!(OrderbookManager::normalize_messages_for_attach(Vec::new()).is_empty());
    }
}
