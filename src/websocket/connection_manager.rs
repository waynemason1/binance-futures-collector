use anyhow::{Context, Result};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use log::{debug, error, info, warn};
use rustc_hash::FxHashMap;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{interval, sleep, timeout};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Bytes, Error as WsError, Message};
use tokio_tungstenite::{connect_async_with_config, MaybeTlsStream, WebSocketStream};

use crate::config::Config;
use crate::models::*;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Manages multiple WebSocket connections for different symbol batches
pub struct WebSocketManager {
    config: Arc<Config>,
    connections: Arc<DashMap<usize, ConnectionState>>,
    message_tx: mpsc::Sender<WebSocketMessage>,
    // Set on shutdown to stop ingest: read loops close and the reconnect loop
    // exits instead of retrying, so the message channel can drain before flush.
    shutdown: Arc<AtomicBool>,
    reconnect_attempts: Arc<DashMap<usize, u32>>,
    batch_shutdown_flags: Arc<DashMap<usize, Arc<AtomicBool>>>,

    // Ring buffers with maxlen to prevent unbounded memory growth
    symbol_buffers: Arc<DashMap<String, Arc<Mutex<VecDeque<DepthUpdateWs>>>>>,

    // Track message counts for stability checks
    symbol_message_counts: Arc<DashMap<String, usize>>,
}

/// State of a single WebSocket connection
struct ConnectionState {
    #[allow(dead_code)] // Used for batch identification in logs
    batch_id: usize,
    symbols: Vec<String>,
    #[allow(dead_code)] // Stored for reconnection purposes
    url: String,
    is_connected: bool,
    last_pong: std::time::Instant,
    message_count: u64,
}

/// WebSocket message types
#[derive(Debug, Clone)]
pub enum WebSocketMessage {
    DepthUpdate(DepthUpdateWs, std::time::Instant), // data, received_at
    Trade(AggTradeWs, std::time::Instant),          // data, received_at
    Kline(KlineWs, std::time::Instant),             // data, received_at
    Liquidation(LiquidationWs, std::time::Instant), // data, received_at
    Error(usize, String),                           // batch_id, error
    Connected(usize),                               // conn id
    Disconnected(usize),                            // conn id
}

/// Binance futures WebSocket routing (post-2026-04-23 endpoint split).
/// Diff-depth is served on `/public`; aggTrade, kline and forceOrder on `/market`.
/// Legacy unrouted URLs were decommissioned and no longer push `/market` streams,
/// so each batch opens one connection per route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Public,
    Market,
}

impl Route {
    fn path(&self) -> &'static str {
        match self {
            Route::Public => "public",
            Route::Market => "market",
        }
    }
    /// Stable per-route offset so (batch, route) maps to a unique connection id.
    fn idx(&self) -> usize {
        match self {
            Route::Public => 0,
            Route::Market => 1,
        }
    }
}

/// Unique connection key for a (logical batch, route) pair.
fn conn_id(batch_id: usize, route: Route) -> usize {
    batch_id * 2 + route.idx()
}

/// Exponential reconnect backoff for a 1-based `attempt`, doubling from `initial`
/// and capped at `max`. The exponent is bounded at 16 so `2^attempt` cannot
/// overflow once the attempt count grows large during a prolonged outage.
fn backoff_for(attempt: u32, initial: Duration, max: Duration) -> Duration {
    std::cmp::min(initial * 2u32.pow(attempt.min(16)), max)
}

impl WebSocketManager {
    pub fn new(
        config: Arc<Config>,
        message_tx: mpsc::Sender<WebSocketMessage>,
        symbols: &[String],
    ) -> Self {
        let symbol_buffers = Arc::new(DashMap::new());
        for symbol in symbols {
            symbol_buffers.insert(
                symbol.clone(),
                Arc::new(Mutex::new(VecDeque::with_capacity(
                    config.websocket.ring_buffer_capacity,
                ))),
            );
        }

        Self {
            config,
            connections: Arc::new(DashMap::new()),
            message_tx,
            shutdown: Arc::new(AtomicBool::new(false)),
            reconnect_attempts: Arc::new(DashMap::new()),
            batch_shutdown_flags: Arc::new(DashMap::new()),
            symbol_buffers,
            symbol_message_counts: Arc::new(DashMap::new()),
        }
    }

    /// Signal all connections to stop. Read loops close and the reconnect loop
    /// exits rather than retrying, letting the message channel drain before the
    /// caller flushes buffers to disk.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        for flag in self.batch_shutdown_flags.iter() {
            flag.value().store(true, Ordering::Relaxed);
        }
    }

    /// Start WebSocket connections for a specific batch (COORDINATED STARTUP)
    /// This is called by the orderbook manager to coordinate staged startup.
    pub async fn start_batch(&self, symbols_batch: Vec<String>, batch_id: usize) -> Result<()> {
        if symbols_batch.is_empty() {
            warn!("Batch {}: No symbols to start.", batch_id);
            return Ok(());
        }

        info!(
            "Batch {}: Starting WebSocket connections for {} symbols...",
            batch_id,
            symbols_batch.len()
        );

        // Start heartbeat monitor on first batch (ensures pong timeout detection)
        if batch_id == 0 {
            self.start_heartbeat_monitor().await;
            info!("Started heartbeat monitor for connection health checks");
        }

        // One connection per route: /public (depth) + /market (trades, klines, liquidations).
        let manager = self.clone();
        let symbols = symbols_batch.clone();

        let public = manager.clone();
        let public_syms = symbols.clone();
        tokio::spawn(async move {
            if let Err(e) = public
                .run_connection(batch_id, public_syms, Route::Public)
                .await
            {
                error!("Batch {} [public] connection error: {}", batch_id, e);
            }
        });
        tokio::spawn(async move {
            if let Err(e) = manager
                .run_connection(batch_id, symbols, Route::Market)
                .await
            {
                error!("Batch {} [market] connection error: {}", batch_id, e);
            }
        });

        // Brief delay to ensure connection starts before returning
        sleep(Duration::from_millis(100)).await;

        Ok(())
    }

    /// Atomically drains and returns the buffer for a given symbol.
    /// This is the designated, thread-safe interface for the OrderbookManager.
    pub async fn drain_buffer(&self, symbol: &str) -> Vec<DepthUpdateWs> {
        if let Some(buffer_arc) = self.symbol_buffers.get(symbol) {
            let mut buffer = buffer_arc.lock().await;
            let drained: Vec<_> = buffer.drain(..).collect();
            drained
        } else {
            Vec::new()
        }
    }

    /// Get the number of messages processed for a symbol (for stability checks)
    pub fn get_symbol_message_count(&self, symbol: &str) -> usize {
        self.symbol_message_counts
            .get(symbol)
            .map(|r| *r)
            .unwrap_or(0)
    }

    /// Check if all symbols in a batch have processed at least min_messages
    pub fn check_batch_stability(&self, symbols: &[String], min_messages: usize) -> bool {
        symbols
            .iter()
            .all(|s| self.get_symbol_message_count(s) >= min_messages)
    }

    /// Run a single WebSocket connection with reconnection logic
    async fn run_connection(
        &self,
        batch_id: usize,
        symbols: Vec<String>,
        route: Route,
    ) -> Result<()> {
        let cid = conn_id(batch_id, route);
        let initial_backoff = Duration::from_millis(self.config.websocket.reconnect_delay_ms);
        let max_backoff = Duration::from_millis(self.config.websocket.max_reconnect_delay_ms);
        // A connection that survives this long is treated as healthy: a later drop
        // resets the backoff instead of compounding an old attempt count.
        let stable_connection = Duration::from_secs(60);

        loop {
            if self.shutdown.load(Ordering::Relaxed) {
                info!("Batch {} [{}] stopping (shutdown)", batch_id, route.path());
                return Ok(());
            }
            let run_started = Instant::now();
            match self.connect_and_run(batch_id, &symbols, route).await {
                Ok(_) => {
                    if self.shutdown.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    // Graceful end (e.g. scheduled stream rotation): reconnect at once.
                    info!(
                        "Batch {} [{}] connection ended, reconnecting",
                        batch_id,
                        route.path()
                    );
                    self.reconnect_attempts.insert(cid, 0);
                    sleep(Duration::from_millis(500)).await;
                }
                Err(e) => {
                    if self.shutdown.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    error!(
                        "Batch {} [{}] connection failed: {}",
                        batch_id,
                        route.path(),
                        e
                    );

                    // A link that stayed up past the stable window is a fresh failure,
                    // not part of a tight loop — clear the count so a healthy connection
                    // that blips doesn't inherit an old backoff.
                    if run_started.elapsed() >= stable_connection {
                        self.reconnect_attempts.insert(cid, 0);
                    }
                    let attempts = *self
                        .reconnect_attempts
                        .entry(cid)
                        .and_modify(|a| *a += 1)
                        .or_insert(1);

                    // Escalate the log past the configured threshold, but never give up:
                    // an unattended collector must ride out outages longer than any fixed
                    // budget, or it silently drops this batch's symbols until a restart.
                    if attempts == self.config.websocket.max_reconnect_attempts {
                        error!(
                            "Batch {} [{}] still down after {} attempts; continuing to retry",
                            batch_id,
                            route.path(),
                            attempts
                        );
                    }

                    // Exponential backoff, capped (see backoff_for).
                    let backoff = backoff_for(attempts, initial_backoff, max_backoff);
                    let jitter = Duration::from_millis((rand::random::<f64>() * 1000.0) as u64);
                    warn!(
                        "Batch {} [{}] reconnecting in {:?} (attempt {})",
                        batch_id,
                        route.path(),
                        backoff + jitter,
                        attempts
                    );
                    sleep(backoff + jitter).await;
                }
            }
        }
    }

    /// Connect and run WebSocket until disconnection
    async fn connect_and_run(
        &self,
        batch_id: usize,
        symbols: &[String],
        route: Route,
    ) -> Result<()> {
        let cid = conn_id(batch_id, route);

        let shutdown_flag = Arc::new(AtomicBool::new(false));
        self.batch_shutdown_flags.insert(cid, shutdown_flag.clone());

        // Build the route-specific stream list. Per Binance's 2026 endpoint split,
        // diff-depth is served on /public; aggTrade, kline and forceOrder on /market.
        let mut streams: Vec<String> = match route {
            Route::Public => symbols
                .iter()
                .map(|symbol| {
                    format!(
                        "{}@depth@{}ms",
                        symbol.to_lowercase(),
                        self.config.orderbook.update_speed_ms
                    )
                })
                .collect(),
            Route::Market => symbols
                .iter()
                .flat_map(|symbol| {
                    let s = symbol.to_lowercase();
                    vec![format!("{}@aggTrade", s), format!("{}@kline_1m", s)]
                })
                .collect(),
        };

        // Global liquidation stream lives on /market; add once (on batch 0's market conn).
        if route == Route::Market && batch_id == 0 {
            streams.push("!forceOrder@arr".to_string());
        }

        let url = format!(
            "{}/{}/stream?streams={}",
            self.config.exchange.websocket_url,
            route.path(),
            streams.join("/")
        );

        info!(
            "Batch {} [{}] connecting to {} streams",
            batch_id,
            route.path(),
            streams.len()
        );

        // Configure WebSocket for high-throughput streaming
        // 8MB write buffer handles auto-pong responses
        // Binance futures sends high-frequency updates (480 symbols × 3 streams)
        // tokio-tungstenite auto-responds to server pings with pongs
        // Large buffer ensures pongs can be queued even during peak throughput
        // WebSocketConfig is #[non_exhaustive] upstream, so it has to be built from
        // the default and mutated rather than constructed literally.
        let mut ws_config = WebSocketConfig::default();
        ws_config.max_message_size = Some(64 * 1024 * 1024); // 64 MB (receive side)
        ws_config.max_frame_size = Some(16 * 1024 * 1024); // 16 MB (receive side)
        ws_config.write_buffer_size = 8 * 1024 * 1024; // 8 MB (send side), room for auto-pong at peak
        ws_config.accept_unmasked_frames = false; // enforce protocol compliance

        // Direct connection (no proxy) - WebSocket streams have no rate limits
        let (ws_stream, response) = timeout(
            Duration::from_secs(30),
            connect_async_with_config(&url, Some(ws_config), true),
        )
        .await
        .context("Connection timeout")?
        .context("Failed to connect")?;

        debug!(
            "Batch {} [{}] WebSocket handshake response: {:?}",
            batch_id,
            route.path(),
            response.status()
        );
        info!(
            "Batch {} [{}] connected ({} symbols, {} streams)",
            batch_id,
            route.path(),
            symbols.len(),
            streams.len()
        );

        self.connections.insert(
            cid,
            ConnectionState {
                batch_id: cid,
                symbols: symbols.to_vec(),
                url: url.clone(),
                is_connected: true,
                last_pong: std::time::Instant::now(),
                message_count: 0,
            },
        );

        let _ = self.message_tx.send(WebSocketMessage::Connected(cid)).await;

        let result = self.run_message_loop(cid, ws_stream, shutdown_flag).await;

        self.batch_shutdown_flags.remove(&cid);

        result
    }

    async fn run_message_loop(
        &self,
        batch_id: usize,
        ws_stream: WsStream,
        shutdown_flag: Arc<AtomicBool>,
    ) -> Result<()> {
        let (mut ws_tx, mut ws_rx) = ws_stream.split();

        // Create channel for ping messages to avoid mutex contention
        let (ping_tx, mut ping_rx) = tokio::sync::mpsc::channel::<()>(10);
        let ping_interval_secs = self.config.websocket.ping_interval_seconds;

        let shutdown_check = shutdown_flag.clone();
        let ping_timer = tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(ping_interval_secs));
            loop {
                interval.tick().await;

                if shutdown_check.load(Ordering::Relaxed) {
                    break;
                }

                if ping_tx.send(()).await.is_err() {
                    break; // Channel closed, exit
                }
            }
        });

        let mut had_error = false;
        let mut error_msg = String::new();

        // Track last message time for timeout detection
        let mut last_message_time = std::time::Instant::now();
        let timeout_threshold = Duration::from_secs(30);

        // Process incoming messages with proper async select pattern
        // This prevents tight CPU loops and allows proper concurrent handling
        loop {
            tokio::select! {
                // Send ping when timer signals (no mutex contention)
                Some(_) = ping_rx.recv() => {
                    if let Err(e) = ws_tx.send(Message::Ping(Bytes::new())).await {
                        error!("Batch {} ping send failed: {}", batch_id, e);
                        had_error = true;
                        error_msg = e.to_string();
                        break;
                    }
                }

                // Check for shutdown signal from heartbeat monitor
                _ = async {
                    loop {
                        if shutdown_flag.load(Ordering::Relaxed) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                } => {
                    info!("Batch {} shutting down due to pong timeout", batch_id);
                    had_error = true;
                    error_msg = "Pong timeout".to_string();
                    break;
                }

                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if last_message_time.elapsed() > timeout_threshold {
                        error!(
                            "Batch {} WebSocket timeout - no message in {:?}",
                            batch_id, timeout_threshold
                        );
                        had_error = true;
                        error_msg = format!("No message received in {:?}", timeout_threshold);
                        break;
                    }
                }

                // Receive next message (blocking await, not a tight loop)
                msg_result = ws_rx.next() => {
                    let msg_result = match msg_result {
                        Some(result) => {
                            last_message_time = std::time::Instant::now();
                            result
                        }
                        None => {
                            info!("Batch {} WebSocket stream ended", batch_id);
                            break;
                        }
                    };

                    match msg_result {
                        Ok(Message::Text(text)) => {
                            if let Err(e) = self.process_text_message(batch_id, &text).await {
                                error!("Batch {} message processing error: {}", batch_id, e);
                            }
                        }
                        Ok(Message::Binary(data)) => {
                            if let Ok(text) = String::from_utf8(data.to_vec()) {
                                if let Err(e) = self.process_text_message(batch_id, &text).await {
                                    error!("Batch {} binary message processing error: {}", batch_id, e);
                                }
                            }
                        }
                        Ok(Message::Ping(payload)) => {
                            // Manually send pong response and flush
                            // With split streams, tokio-tungstenite's auto-pong doesn't flush until next write
                            // Binance sends server pings every 3min, expects pong within 10min
                            // Must explicitly send pong and flush to avoid "Connection reset without closing handshake"
                            use futures_util::SinkExt;
                            if let Err(e) = ws_tx.send(Message::Pong(payload)).await {
                                error!("Batch {} failed to send pong: {}", batch_id, e);
                                had_error = true;
                                error_msg = format!("Pong send failed: {}", e);
                                break;
                            }
                            // Flush immediately to ensure pong reaches Binance server
                            if let Err(e) = ws_tx.flush().await {
                                error!("Batch {} failed to flush pong: {}", batch_id, e);
                                had_error = true;
                                error_msg = format!("Pong flush failed: {}", e);
                                break;
                            }
                            debug!("Batch {} received server ping, sent and flushed pong", batch_id);
                            if let Some(mut state) = self.connections.get_mut(&batch_id) {
                                state.last_pong = std::time::Instant::now();
                            }
                        }
                        Ok(Message::Pong(_)) => {
                            // Received pong response to our client ping
                            if let Some(mut state) = self.connections.get_mut(&batch_id) {
                                state.last_pong = std::time::Instant::now();
                            }
                            debug!("Batch {} received pong", batch_id);
                        }
                        Ok(Message::Close(_)) => {
                            info!("Batch {} received close frame", batch_id);
                            break;
                        }
                        Err(e) => {
                            let error_type = match &e {
                                WsError::Io(io_err) if io_err.kind() == std::io::ErrorKind::ConnectionReset => "Connection reset by peer",
                                WsError::Protocol(_) => "Protocol error",
                                WsError::Io(_) => "IO error",
                                _ => "Unknown error"
                            };
                            error!("Batch {} {} - {}", batch_id, error_type, e);
                            had_error = true;
                            error_msg = e.to_string();
                            let _ = self.message_tx.send(
                                WebSocketMessage::Error(batch_id, e.to_string())
                            ).await;
                            break;
                        }
                        _ => {}
                    }

                    if let Some(mut state) = self.connections.get_mut(&batch_id) {
                        state.message_count += 1;
                    }
                }
            } // End of tokio::select!
        }

        ping_timer.abort();

        if had_error {
            warn!("Batch {} disconnected with error: {}", batch_id, error_msg);
        } else {
            info!("Batch {} disconnected gracefully", batch_id);
        }

        self.connections.remove(&batch_id);
        let _ = self
            .message_tx
            .send(WebSocketMessage::Disconnected(batch_id))
            .await;

        // Return error if connection ended with error (triggers backoff)
        if had_error {
            Err(anyhow::anyhow!("WebSocket error: {}", error_msg))
        } else {
            Ok(())
        }
    }

    async fn process_text_message(&self, batch_id: usize, text: &str) -> Result<()> {
        // Capture timestamp when message is received for latency tracking
        let received_at = std::time::Instant::now();

        // Parse as wrapped message (Binance futures uses {"stream": "...", "data": {...}})
        #[derive(serde::Deserialize)]
        struct Wrapper {
            #[serde(default)]
            stream: String,
            data: Value,
        }

        let wrapper: Wrapper =
            serde_json::from_str(text).context("Failed to parse wrapped JSON")?;

        self.process_stream_data(batch_id, &wrapper.stream, wrapper.data, received_at)
            .await
    }

    async fn process_stream_data(
        &self,
        batch_id: usize,
        _stream_name: &str,
        mut data: Value,
        received_at: std::time::Instant,
    ) -> Result<()> {
        // Handle liquidation array from !forceOrder@arr stream
        if let Value::Array(orders) = data {
            for order_data in orders {
                if let Some("forceOrder") = order_data.get("e").and_then(|e| e.as_str()) {
                    let liquidation: LiquidationWs = serde_json::from_value(order_data)?;
                    let _ = self
                        .message_tx
                        .send(WebSocketMessage::Liquidation(liquidation, received_at))
                        .await;
                }
            }
            return Ok(());
        }

        let event_type = data.get("e").and_then(|e| e.as_str());

        match event_type {
            Some("depthUpdate") => {
                // Pre-convert floats for orderbook operations (8-10% CPU reduction)
                if let Some(bids_raw) = data.get("b").and_then(|b| b.as_array()) {
                    let b_floats: Vec<(f64, f64)> = bids_raw
                        .iter()
                        .filter_map(|item| {
                            let arr = item.as_array()?;
                            let p = arr.first()?.as_str()?.parse::<f64>().ok()?;
                            let q = arr.get(1)?.as_str()?.parse::<f64>().ok()?;
                            Some((p, q))
                        })
                        .collect();

                    if let Some(obj) = data.as_object_mut() {
                        obj.insert("b_floats".to_string(), serde_json::to_value(&b_floats)?);
                    }
                }

                if let Some(asks_raw) = data.get("a").and_then(|a| a.as_array()) {
                    let a_floats: Vec<(f64, f64)> = asks_raw
                        .iter()
                        .filter_map(|item| {
                            let arr = item.as_array()?;
                            let p = arr.first()?.as_str()?.parse::<f64>().ok()?;
                            let q = arr.get(1)?.as_str()?.parse::<f64>().ok()?;
                            Some((p, q))
                        })
                        .collect();

                    if let Some(obj) = data.as_object_mut() {
                        obj.insert("a_floats".to_string(), serde_json::to_value(&a_floats)?);
                    }
                }

                let update: DepthUpdateWs = serde_json::from_value(data)?;

                // Buffer the update (don't process yet - that's the orderbook manager's job)
                self.buffer_depth_update(update.clone()).await;

                // Send to message handler for CSV writing
                let _ = self
                    .message_tx
                    .send(WebSocketMessage::DepthUpdate(update, received_at))
                    .await;
            }
            Some("aggTrade") => {
                let trade: AggTradeWs = serde_json::from_value(data)?;
                let _ = self
                    .message_tx
                    .send(WebSocketMessage::Trade(trade, received_at))
                    .await;
            }
            Some("kline") => {
                let kline: KlineWs = serde_json::from_value(data)?;
                let _ = self
                    .message_tx
                    .send(WebSocketMessage::Kline(kline, received_at))
                    .await;
            }
            Some("forceOrder") => {
                let liquidation: LiquidationWs = serde_json::from_value(data)?;
                let _ = self
                    .message_tx
                    .send(WebSocketMessage::Liquidation(liquidation, received_at))
                    .await;
            }
            Some(event_type) => {
                debug!("Batch {} unknown event type: {}", batch_id, event_type);
            }
            None => {
                debug!("Batch {} message without event type", batch_id);
            }
        }

        Ok(())
    }

    /// Buffer depth update in ring buffer
    async fn buffer_depth_update(&self, update: DepthUpdateWs) {
        // Convert to lowercase to match buffer keys (Binance sends uppercase)
        let symbol = update.symbol.to_lowercase();

        debug!(
            "WS_DEPTH: Received update for {} (orig: {}), buffer exists: {}",
            symbol,
            update.symbol,
            self.symbol_buffers.contains_key(&symbol)
        );

        self.symbol_message_counts
            .entry(symbol.clone())
            .and_modify(|c| *c += 1)
            .or_insert(1);

        if let Some(buffer_arc) = self.symbol_buffers.get(&symbol) {
            let mut buffer = buffer_arc.lock().await;

            buffer.push_back(update.clone());

            // Maintain ring buffer constraints
            let capacity = self.config.websocket.ring_buffer_capacity;
            let max_seconds = self.config.websocket.ring_buffer_seconds;

            // Enforce capacity limit (evict oldest when over limit)
            while buffer.len() > capacity {
                buffer.pop_front();
            }

            // Enforce time limit (evict messages older than max_seconds)
            // Uses real time instead of message timestamps (robust against timestamp skew)
            if buffer.len() > 1 {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;

                while buffer.len() > 1 {
                    let first_ts = buffer.front().unwrap().event_time;
                    if (now - first_ts) > (max_seconds * 1000) as i64 {
                        buffer.pop_front();
                    } else {
                        break;
                    }
                }
            }
        }
    }

    async fn start_heartbeat_monitor(&self) {
        let connections = self.connections.clone();
        let shutdown_flags = self.batch_shutdown_flags.clone();
        let heartbeat_interval = Duration::from_secs(self.config.websocket.ping_interval_seconds);
        let miss_threshold = self.config.websocket.heartbeat_miss_reconnect;

        tokio::spawn(async move {
            let mut interval = interval(heartbeat_interval);

            loop {
                interval.tick().await;

                for entry in connections.iter() {
                    let batch_id = *entry.key();
                    let state = entry.value();

                    let since_pong = state.last_pong.elapsed();
                    let max_allowed = heartbeat_interval * miss_threshold;

                    if since_pong > max_allowed {
                        error!(
                            "Batch {} pong timeout ({:?} > {:?}), triggering reconnect",
                            batch_id, since_pong, max_allowed
                        );

                        // Signal shutdown to trigger reconnection
                        if let Some(flag) = shutdown_flags.get(&batch_id) {
                            flag.store(true, Ordering::Relaxed);
                        }
                    }
                }
            }
        });
    }

    /// Get connection statistics
    pub async fn get_stats(&self) -> FxHashMap<usize, ConnectionStats> {
        let mut stats = FxHashMap::default();

        for entry in self.connections.iter() {
            let batch_id = *entry.key();
            let state = entry.value();

            stats.insert(
                batch_id,
                ConnectionStats {
                    batch_id,
                    is_connected: state.is_connected,
                    symbols_count: state.symbols.len(),
                    message_count: state.message_count,
                    last_pong_ago: state.last_pong.elapsed(),
                    reconnect_attempts: self
                        .reconnect_attempts
                        .get(&batch_id)
                        .map(|r| *r)
                        .unwrap_or(0),
                },
            );
        }

        stats
    }
}

/// Connection statistics
#[derive(Debug, Clone)]
pub struct ConnectionStats {
    pub batch_id: usize,
    pub is_connected: bool,
    pub symbols_count: usize,
    pub message_count: u64,
    pub last_pong_ago: Duration,
    pub reconnect_attempts: u32,
}

impl Clone for WebSocketManager {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            connections: self.connections.clone(),
            message_tx: self.message_tx.clone(),
            shutdown: self.shutdown.clone(),
            reconnect_attempts: self.reconnect_attempts.clone(),
            batch_shutdown_flags: self.batch_shutdown_flags.clone(),
            symbol_buffers: self.symbol_buffers.clone(),
            symbol_message_counts: self.symbol_message_counts.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::backoff_for;
    use std::time::Duration;

    #[test]
    fn backoff_doubles_from_initial() {
        let init = Duration::from_millis(2000);
        let max = Duration::from_secs(600);
        assert_eq!(backoff_for(1, init, max), Duration::from_millis(4000));
        assert_eq!(backoff_for(2, init, max), Duration::from_millis(8000));
        assert_eq!(backoff_for(3, init, max), Duration::from_millis(16000));
    }

    #[test]
    fn backoff_is_capped_at_max() {
        let init = Duration::from_millis(2000);
        let max = Duration::from_millis(60000);
        // 2000 * 2^5 = 64000 > 60000, so it caps.
        assert_eq!(backoff_for(5, init, max), max);
        assert_eq!(backoff_for(10, init, max), max);
    }

    #[test]
    fn backoff_shift_never_overflows_at_high_attempts() {
        let init = Duration::from_millis(2000);
        let max = Duration::from_millis(60000);
        // A prolonged outage pushes the attempt count high; the .min(16) bound must
        // keep 2^attempt from overflowing u32 — this must not panic and stays capped.
        assert_eq!(backoff_for(1_000_000, init, max), max);
        assert_eq!(backoff_for(u32::MAX, init, max), max);
    }
}
