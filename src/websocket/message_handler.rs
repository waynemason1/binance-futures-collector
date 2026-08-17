use anyhow::Result;
use dashmap::DashMap;
use log::{debug, error, info, warn};
use parking_lot::{Mutex, RwLock};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::{mpsc, Notify};

use crate::constants;
use crate::models::*;
use crate::utils::CsvWriter;
use crate::websocket::connection_manager::WebSocketMessage;

/// Number of most-recent trade IDs retained per symbol for deduplication.
const TRADE_DEDUP_WINDOW: usize = 5000;

/// Bounded per-symbol trade-ID dedup window. Membership test and eviction of the
/// oldest ID are both O(1), so the trade hot path stays flat instead of sorting
/// the whole window on every insert once it fills.
#[derive(Default)]
struct TradeDedupWindow {
    seen: HashSet<i64>,
    order: VecDeque<i64>,
}

impl TradeDedupWindow {
    /// Record a trade ID. Returns `true` if it was new (and is now retained),
    /// `false` if it was already present (a duplicate).
    fn insert(&mut self, trade_id: i64) -> bool {
        if !self.seen.insert(trade_id) {
            return false;
        }
        self.order.push_back(trade_id);
        if self.order.len() > TRADE_DEDUP_WINDOW {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        true
    }
}

/// Handles processing of WebSocket messages
/// NOTE: Orderbook buffering is now handled by WebSocketManager (ring buffers)
/// This handler only writes to CSV
pub struct MessageHandler {
    csv_writer: Arc<CsvWriter>,
    trade_dedup: Arc<DashMap<String, Mutex<TradeDedupWindow>>>,
    trade_dedup_last_seen: Arc<DashMap<String, i64>>, // Track last trade time per symbol
    stats: Arc<RwLock<MessageStats>>,
    // The USDT-M universe we collect. Liquidations come from the market-wide
    // !forceOrder@arr stream, so they're filtered against this set.
    allowed_symbols: Arc<HashSet<String>>,
}

#[derive(Default)]
pub struct MessageStats {
    pub depth_updates: u64,
    pub trades: u64,
    pub klines: u64,
    pub liquidations: u64,
    pub errors: u64,
    // WS latency tracking (rolling average in milliseconds)
    pub ws_latency_ms: f64,
    pub total_latency_samples: u64,
}

impl MessageHandler {
    pub fn new(csv_writer: Arc<CsvWriter>, allowed_symbols: Arc<HashSet<String>>) -> Self {
        let handler = Self {
            csv_writer,
            trade_dedup: Arc::new(DashMap::new()),
            trade_dedup_last_seen: Arc::new(DashMap::new()),
            stats: Arc::new(RwLock::new(MessageStats::default())),
            allowed_symbols,
        };

        // Start periodic cleanup task (every 10 minutes)
        handler.start_periodic_cleanup();

        handler
    }

    /// Start message processing loop.
    ///
    /// Runs until the channel closes (all senders dropped) or `shutdown` is
    /// signalled. On shutdown it drains whatever is already queued so in-flight
    /// updates land on disk, then returns — the caller joins this task before
    /// flushing, giving a deterministic drain instead of a fixed sleep. (The
    /// manager keeps a sender alive, so the channel would not close on its own.)
    pub async fn run(
        &self,
        mut message_rx: mpsc::Receiver<WebSocketMessage>,
        shutdown: Arc<Notify>,
    ) -> Result<()> {
        info!("Message handler started");

        loop {
            tokio::select! {
                maybe = message_rx.recv() => match maybe {
                    Some(message) => self.handle_one(message).await,
                    None => break, // all senders dropped
                },
                _ = shutdown.notified() => {
                    while let Ok(message) = message_rx.try_recv() {
                        self.handle_one(message).await;
                    }
                    break;
                }
            }
        }

        info!("Message handler stopped");
        Ok(())
    }

    async fn handle_one(&self, message: WebSocketMessage) {
        if let Err(e) = self.process_message(message).await {
            error!("Message processing error: {}", e);
            self.stats.write().errors += 1;
        }
    }

    /// Start periodic cleanup task for trade deduplication map
    fn start_periodic_cleanup(&self) {
        let dedup_map = self.trade_dedup.clone();
        let last_seen_map = self.trade_dedup_last_seen.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(600)); // 10 minutes

            loop {
                interval.tick().await;

                let now_ms = chrono::Utc::now().timestamp_millis();
                const MAX_AGE_MS: i64 = 60 * 60 * 1000; // 1 hour

                let mut symbols_removed = 0;

                // Find and remove stale symbols from outer map
                let stale_symbols: Vec<String> = last_seen_map
                    .iter()
                    .filter(|entry| {
                        let last_seen = *entry.value();
                        now_ms - last_seen > MAX_AGE_MS
                    })
                    .map(|entry| entry.key().clone())
                    .collect();

                for symbol in &stale_symbols {
                    dedup_map.remove(symbol);
                    last_seen_map.remove(symbol);
                    symbols_removed += 1;
                }

                if symbols_removed > 0 {
                    info!(
                        "Cleaned up {} stale symbols from trade dedup map (symbols remaining: {})",
                        symbols_removed,
                        dedup_map.len()
                    );
                }
            }
        });
    }

    async fn process_message(&self, message: WebSocketMessage) -> Result<()> {
        match message {
            WebSocketMessage::DepthUpdate(update, received_at) => {
                self.update_latency(received_at);
                self.process_depth_update(update).await?;
            }
            WebSocketMessage::Trade(trade, received_at) => {
                self.update_latency(received_at);
                self.process_trade(trade).await?;
            }
            WebSocketMessage::Kline(kline, received_at) => {
                self.update_latency(received_at);
                self.process_kline(kline).await?;
            }
            WebSocketMessage::Liquidation(liquidation, received_at) => {
                self.update_latency(received_at);
                self.process_liquidation(liquidation).await?;
            }
            WebSocketMessage::Connected(batch_id) => {
                info!("Batch {} connected", batch_id);
            }
            WebSocketMessage::Disconnected(batch_id) => {
                warn!("Batch {} disconnected", batch_id);
            }
            WebSocketMessage::Error(batch_id, error) => {
                error!("Batch {} error: {}", batch_id, error);
                self.stats.write().errors += 1;
            }
        }

        Ok(())
    }

    /// Update rolling average latency
    fn update_latency(&self, received_at: std::time::Instant) {
        // received_at is stamped at socket-read, so this is INTERNAL processing
        // lag (channel queue + dispatch), NOT network latency to the exchange —
        // the dashboard labels it "proc lag" accordingly.
        let latency_ms = received_at.elapsed().as_micros() as f64 / 1000.0;

        let mut stats = self.stats.write();
        // Use exponential moving average: new_avg = old_avg * 0.95 + new_sample * 0.05
        // This gives more weight to recent samples while maintaining stability
        if stats.total_latency_samples == 0 {
            stats.ws_latency_ms = latency_ms;
        } else {
            stats.ws_latency_ms = stats.ws_latency_ms * 0.95 + latency_ms * 0.05;
        }
        stats.total_latency_samples += 1;
    }

    /// Process depth update
    /// NOTE: Buffering is now handled by WebSocketManager, this just writes to CSV
    async fn process_depth_update(&self, update: DepthUpdateWs) -> Result<()> {
        // Convert to CSV and write. Per-symbol persistence can be disabled via
        // the [storage] config; the storage layer enforces that.
        let csv_update = update.to_csv();
        self.csv_writer.write_depth_update(&csv_update).await?;

        self.stats.write().depth_updates += 1;

        Ok(())
    }

    /// Process trade with deduplication
    async fn process_trade(&self, trade: AggTradeWs) -> Result<()> {
        let symbol = &trade.symbol;
        let trade_id = trade.aggregate_trade_id;

        // Record the trade ID in the symbol's bounded dedup window. Scope the
        // map guard and the inner lock so both are dropped BEFORE the CSV
        // `.await` below — a DashMap `Ref`/`RefMut` held across an await can
        // park a tokio worker thread on that shard until the write completes.
        let is_new = {
            let window = self.trade_dedup.entry(symbol.to_string()).or_default();
            let new = window.lock().insert(trade_id);
            new
        };
        if !is_new {
            debug!("Duplicate trade {} for {}", trade_id, symbol);
            return Ok(());
        }

        // Update last seen time for this symbol
        self.trade_dedup_last_seen
            .insert(symbol.to_string(), chrono::Utc::now().timestamp_millis());

        let csv_trade = trade.to_csv();
        self.csv_writer.write_trade(&csv_trade).await?;

        self.stats.write().trades += 1;

        Ok(())
    }

    /// Process kline (only closed klines)
    async fn process_kline(&self, kline_msg: KlineWs) -> Result<()> {
        // Only process closed klines
        if !kline_msg.kline.is_closed {
            return Ok(());
        }

        let kline = &kline_msg.kline;

        let csv_kline = KlineCsv {
            exchange: constants::EXCHANGE.to_string(),
            market: constants::MARKET.to_string(),
            datatype: constants::DATATYPE_KLINES.to_string(),
            timestamp_ms: kline.open_time,
            symbol: kline.symbol.to_lowercase(), // Convert Binance UPPERCASE to lowercase
            open: format_decimal_str(&kline.open),
            high: format_decimal_str(&kline.high),
            low: format_decimal_str(&kline.low),
            close: format_decimal_str(&kline.close),
            volume: format_decimal_str(&kline.volume),
            quote_volume: format_decimal_str(&kline.quote_volume),
            trade_count: kline.trade_count,
            taker_buy_base: format_decimal_str(&kline.taker_buy_base_volume),
            taker_buy_quote: format_decimal_str(&kline.taker_buy_quote_volume),
        };

        self.csv_writer.write_kline(&csv_kline).await?;

        self.stats.write().klines += 1;

        Ok(())
    }

    async fn process_liquidation(&self, liquidation: LiquidationWs) -> Result<()> {
        let order = &liquidation.order;

        // !forceOrder@arr is market-wide: keep only our USDT-M universe so no
        // USDC / coin-margined / dated / equity-perp symbols land on disk.
        let symbol = order.symbol.to_lowercase();
        if !self.allowed_symbols.contains(&symbol) {
            return Ok(());
        }

        let price: f64 = order.price.parse()?;
        let quantity: f64 = order.quantity.parse()?;
        let value = price * quantity;

        let csv_liquidation = LiquidationCsv {
            exchange: constants::EXCHANGE.to_string(),
            market: constants::MARKET.to_string(),
            datatype: constants::DATATYPE_LIQUIDATIONS.to_string(),
            timestamp_ms: liquidation.event_time,
            symbol,                          // already lowercased + validated above
            side: order.side.to_lowercase(), // Convert SELL -> sell
            price: format_decimal_str(&order.price),
            quantity: format_decimal_str(&order.quantity),
            value: format_decimal(value),
        };

        self.csv_writer.write_liquidation(&csv_liquidation).await?;

        self.stats.write().liquidations += 1;

        Ok(())
    }

    /// Get message processing statistics
    pub fn get_stats(&self) -> MessageStats {
        self.stats.read().clone()
    }
}

impl Clone for MessageStats {
    fn clone(&self) -> Self {
        Self {
            depth_updates: self.depth_updates,
            trades: self.trades,
            klines: self.klines,
            liquidations: self.liquidations,
            errors: self.errors,
            ws_latency_ms: self.ws_latency_ms,
            total_latency_samples: self.total_latency_samples,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TradeDedupWindow, TRADE_DEDUP_WINDOW};

    #[test]
    fn window_rejects_duplicates_and_accepts_new_ids() {
        let mut w = TradeDedupWindow::default();
        assert!(w.insert(100), "first sighting is new");
        assert!(!w.insert(100), "same id again is a duplicate");
        assert!(w.insert(101), "a different id is new");
        assert!(!w.insert(101));
    }

    #[test]
    fn window_evicts_the_oldest_id_once_full() {
        let mut w = TradeDedupWindow::default();
        // Fill exactly to capacity with ids 0..WINDOW.
        for id in 0..TRADE_DEDUP_WINDOW as i64 {
            assert!(w.insert(id));
        }
        assert_eq!(w.seen.len(), TRADE_DEDUP_WINDOW);
        // One more evicts the oldest (id 0); the window never exceeds capacity.
        assert!(w.insert(TRADE_DEDUP_WINDOW as i64));
        assert_eq!(w.seen.len(), TRADE_DEDUP_WINDOW);
        // id 0 fell out of the window, so it now reads as new again.
        assert!(w.insert(0));
        assert_eq!(w.seen.len(), TRADE_DEDUP_WINDOW);
        // A still-resident id remains a duplicate.
        assert!(!w.insert(TRADE_DEDUP_WINDOW as i64 - 1));
    }

    #[test]
    fn window_evicts_by_insertion_order_not_id_value() {
        // Out-of-order ids: eviction follows FIFO insertion order, so the
        // longest-ago sighting leaves first regardless of numeric value.
        let mut w = TradeDedupWindow::default();
        for id in 0..TRADE_DEDUP_WINDOW as i64 {
            w.insert(id);
        }
        // Insert a LOW id last; it must survive, and the oldest (0) must go.
        assert!(w.insert(-1));
        assert!(w.insert(0), "id 0 was evicted, so it's new again");
        assert!(!w.insert(-1), "the low id inserted last is still resident");
    }
}
