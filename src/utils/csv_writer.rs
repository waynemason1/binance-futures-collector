use anyhow::{Context, Result};
use chrono::{DateTime, Timelike, Utc};
use dashmap::DashMap;
use log::{debug, error};
use parking_lot::Mutex;
use std::collections::HashMap; // Keep HashMap for CSV file grouping
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::time::{interval, Duration};

use crate::config::Config;
use crate::constants;
use crate::models::*;

/// Row with sortable metadata for depth updates
#[derive(Clone)]
struct DepthUpdateRow {
    last_update_id: u64, // For sorting (column 8 in HEADER_DEPTH_UPDATES)
    csv_line: String,    // Pre-formatted CSV row
}

/// Buffer for accumulating CSV rows before writing
struct FileBuffer {
    writer: BufWriter<File>,
    row_count: usize,
    last_flush: std::time::Instant,
    #[allow(dead_code)] // Used for logging/debugging in error paths
    path: PathBuf,
    // In-memory buffer for depth_updates (sorted by last_update_id at flush time)
    depth_update_buffer: Option<Vec<DepthUpdateRow>>,
}

impl FileBuffer {
    /// Sort + write any buffered depth rows, then flush the writer. On a write
    /// failure the un-written rows are restored to the buffer so the next flush
    /// retries them instead of silently dropping data.
    fn flush_locked(&mut self) -> Result<()> {
        let mut rows: Vec<DepthUpdateRow> = Vec::new();
        if let Some(depth_buffer) = self.depth_update_buffer.as_mut() {
            if !depth_buffer.is_empty() {
                depth_buffer.sort_by_key(|r| r.last_update_id);
                rows = std::mem::take(depth_buffer);
            }
        }
        for i in 0..rows.len() {
            if let Err(e) = writeln!(self.writer, "{}", rows[i].csv_line) {
                if let Some(db) = self.depth_update_buffer.as_mut() {
                    db.extend(rows.drain(i..));
                }
                return Err(e.into());
            }
        }
        self.writer.flush()?;
        self.last_flush = std::time::Instant::now();
        Ok(())
    }
}

impl Drop for FileBuffer {
    fn drop(&mut self) {
        // Best-effort final drain so buffered depth rows survive an hourly rotation
        // or stale-buffer cleanup instead of being dropped with the Vec.
        let _ = self.flush_locked();
    }
}

/// Manages CSV file writing with buffering and atomic operations
#[derive(Clone)]
pub struct CsvWriter {
    #[allow(dead_code)] // Used for potential future config-driven features
    config: Arc<Config>,
    base_dir: PathBuf,
    buffers: Arc<DashMap<String, Arc<Mutex<FileBuffer>>>>,
    flush_interval: std::time::Duration,
    buffer_size_bytes: usize,
    max_depth_buffer_rows: usize,
}

impl CsvWriter {
    pub fn new(config: Arc<Config>) -> Result<Self> {
        let base_dir = PathBuf::from(&config.output.base_dir);

        // Create base directory if it doesn't exist
        std::fs::create_dir_all(&base_dir)?;

        Ok(Self {
            config: config.clone(),
            base_dir,
            buffers: Arc::new(DashMap::new()),
            flush_interval: std::time::Duration::from_secs(config.output.flush_interval_seconds),
            buffer_size_bytes: config.output.buffer_size_kb * 1024,
            max_depth_buffer_rows: config.output.max_depth_buffer_rows,
        })
    }

    /// Get or create file buffer for a specific file path (uses spawn_blocking)
    async fn get_or_create_buffer(&self, file_path: &Path) -> Result<Arc<Mutex<FileBuffer>>> {
        let path_str = file_path.to_string_lossy().into_owned();

        if let Some(buffer) = self.buffers.get(&path_str) {
            debug!(
                "get_or_create_buffer: Using CACHED buffer for {:?}",
                path_str
            );
            return Ok(buffer.clone());
        }

        debug!(
            "get_or_create_buffer: Creating NEW buffer for {:?}",
            path_str
        );
        let file_path_owned = file_path.to_path_buf();
        let buffer_size = self.buffer_size_bytes;
        let header = self.get_header_for_file(file_path).to_string();

        let buffer = tokio::task::spawn_blocking(move || -> Result<FileBuffer> {
            if let Some(parent) = file_path_owned.parent() {
                debug!("spawn_blocking: Creating parent directory {:?}", parent);
                std::fs::create_dir_all(parent)?;
            }

            debug!("spawn_blocking: Opening file {:?}", file_path_owned);
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file_path_owned)
                .with_context(|| format!("Failed to open file: {:?}", file_path_owned))?;
            debug!(
                "spawn_blocking: File opened successfully: {:?}",
                file_path_owned
            );

            // Check if file is empty to write header
            let metadata = file.metadata()?;
            let needs_header = metadata.len() == 0;

            let mut writer = BufWriter::with_capacity(buffer_size, file);

            if needs_header {
                writeln!(writer, "{}", header)?;
                // Flush immediately to prevent duplicate headers on crash/restart.
                // Without this, the header stays buffered; if the collector crashes before
                // flush the file is empty and the header is written again on restart.
                writer.flush()?;
            }

            // Initialize depth_update_buffer only for depth_updates files
            let is_depth_updates = file_path_owned.to_string_lossy().contains("depth_updates");

            Ok(FileBuffer {
                writer,
                row_count: 0,
                last_flush: std::time::Instant::now(),
                path: file_path_owned,
                depth_update_buffer: if is_depth_updates {
                    Some(Vec::new())
                } else {
                    None
                },
            })
        })
        .await??;

        // Two tasks can both miss the cache for the same brand-new path and both
        // open it (e.g. the periodic snapshot and the one-shot bridge snapshot for
        // one symbol resolve to the same daily file). entry() makes the
        // check-and-insert atomic: if another task cached this path while we were
        // in spawn_blocking, use its buffer and let ours drop, so exactly one
        // FileBuffer (one fd) ever writes the file, instead of two interleaving.
        use dashmap::mapref::entry::Entry;
        match self.buffers.entry(path_str) {
            Entry::Occupied(existing) => Ok(existing.get().clone()),
            Entry::Vacant(slot) => {
                let buffer_arc = Arc::new(Mutex::new(buffer));
                slot.insert(buffer_arc.clone());
                Ok(buffer_arc)
            }
        }
    }

    /// Get appropriate header based on file type (returns &'static str)
    fn get_header_for_file(&self, file_path: &Path) -> &'static str {
        let filename = file_path.file_name().unwrap_or_default().to_string_lossy();

        if filename.contains("depth_updates") {
            constants::HEADER_DEPTH_UPDATES
        } else if filename.contains("depth_snapshot") {
            constants::HEADER_DEPTH_SNAPSHOT
        } else if filename.contains("trades") {
            constants::HEADER_TRADES
        } else if filename.contains("klines")
            && !filename.contains("mark_price")
            && !filename.contains("index_price")
            && !filename.contains("premium_index")
        {
            constants::HEADER_KLINES
        } else if filename.contains("liquidations") {
            constants::HEADER_LIQUIDATIONS
        } else if filename.contains("funding_rates") {
            constants::HEADER_FUNDING_RATES
        } else if filename.contains("open_interest") {
            constants::HEADER_OPEN_INTEREST
        } else if filename.contains("taker_buy_sell_ratio") {
            constants::HEADER_TAKER_BUY_SELL_RATIO
        } else if filename.contains("long_short_ratio") {
            constants::HEADER_LONG_SHORT_RATIO
        } else if filename.contains("premium_index_klines") {
            constants::HEADER_PREMIUM_INDEX_KLINES
        } else if filename.contains("mark_price_klines") || filename.contains("index_price_klines")
        {
            constants::HEADER_MARK_INDEX_KLINES
        } else {
            "exchange,market,datatype,timestamp_ms,datetime_utc,symbol"
        }
    }

    /// Build file path for depth updates (hourly files)
    pub fn get_depth_update_path(&self, symbol: &str, timestamp: i64) -> PathBuf {
        let dt = DateTime::<Utc>::from_timestamp_millis(timestamp).unwrap_or_default();
        let date_str = dt.format("%Y-%m-%d").to_string();
        let hour = dt.hour();
        let next_hour = (hour + 1) % 24;

        let filename = format!(
            "{}_depth_updates_{}_{:02}00-to-{:02}00.csv",
            symbol, // Symbol already lowercase
            date_str,
            hour,
            next_hour
        );

        self.base_dir
            .join("orderbooks")
            .join(symbol) // Symbol already lowercase
            .join(&date_str)
            .join(filename)
    }

    /// Build file path for daily files
    pub fn get_daily_file_path(&self, data_type: &str, symbol: &str, timestamp: i64) -> PathBuf {
        let dt = DateTime::<Utc>::from_timestamp_millis(timestamp).unwrap_or_default();
        let date_str = dt.format("%Y-%m-%d").to_string();

        let filename = match data_type {
            "trades" => format!("{}_trades_{}.csv", symbol, date_str),
            "klines" => format!("{}_klines_{}.csv", symbol, date_str),
            "liquidations" => format!("{}_liquidations_{}.csv", symbol, date_str),
            "funding_rates" => format!("{}_funding_rates_{}.csv", symbol, date_str),
            "open_interest" => format!("{}_open_interest_{}.csv", symbol, date_str),
            "long_short_ratio" => format!("{}_long_short_ratio_{}.csv", symbol, date_str),
            "mark_price_klines" => format!("{}_mark_price_klines_{}.csv", symbol, date_str),
            "index_price_klines" => format!("{}_index_price_klines_{}.csv", symbol, date_str),
            "taker_buy_sell_ratio" => format!("{}_taker_buy_sell_ratio_{}.csv", symbol, date_str),
            "premium_index_klines" => format!("{}_premium_index_klines_{}.csv", symbol, date_str),
            _ => format!("{}_{}.csv", symbol, date_str),
        };

        self.base_dir
            .join(data_type)
            .join(symbol) // Symbol already lowercase
            .join(&date_str)
            .join(filename)
    }

    /// Build file path for depth snapshots
    pub fn get_snapshot_path(&self, symbol: &str, event_type: &str, timestamp: i64) -> PathBuf {
        let dt = DateTime::<Utc>::from_timestamp_millis(timestamp).unwrap_or_default();
        let date_str = dt.format("%Y-%m-%d").to_string();

        let filename = if event_type == "depth_snapshot_initial" {
            // Initial snapshots have timestamp in filename
            let ts_str = dt.format("%Y_%m_%d_%H%M%S").to_string();
            format!(
                "{}_depth_snapshot_initial_{}_{}.csv",
                symbol, // Symbol already lowercase
                ts_str,
                timestamp
            )
        } else {
            // Regular snapshots append to daily file
            format!("{}_depth_snapshot_{}.csv", symbol, date_str)
        };

        self.base_dir
            .join("orderbooks")
            .join(symbol) // Symbol already lowercase
            .join(&date_str)
            .join(filename)
    }

    /// Write depth update to CSV (buffered and sorted by last_update_id)
    /// ISO-8601 UTC (millisecond precision) for a unix-ms timestamp.
    fn iso_utc(ms: i64) -> String {
        DateTime::<Utc>::from_timestamp_millis(ms)
            .unwrap_or_default()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    }

    pub async fn write_depth_update(&self, update: &DepthUpdateCsv) -> Result<()> {
        // Optionally skip raw disk persistence for symbols configured in [storage]
        // (klines + REST metrics are still collected for them).
        if !self.config.storage.should_persist_orderbook(&update.symbol) {
            return Ok(());
        }

        let file_path = self.get_depth_update_path(&update.symbol, update.timestamp_ms);
        let buffer = self.get_or_create_buffer(&file_path).await?;

        let row = format!(
            "{},{},{},{},{},{},{},{},{},\"{}\",\"{}\"",
            update.exchange,
            update.market,
            update.datatype,
            update.timestamp_ms,
            Self::iso_utc(update.timestamp_ms),
            update.symbol,
            update.first_update_id,
            update.last_update_id,
            update.prev_update_id,
            update.bids_json.replace("\"", "\"\""), // Escape quotes for CSV
            update.asks_json.replace("\"", "\"\"")  // Escape quotes for CSV
        );

        self.write_depth_update_buffered(buffer, update.last_update_id, &row)
            .await
    }

    /// Write depth snapshot to CSV (one row per price level)
    pub async fn write_depth_snapshot(&self, snapshots: Vec<DepthSnapshotCsv>) -> Result<()> {
        if snapshots.is_empty() {
            return Ok(());
        }

        let mut file_groups: HashMap<PathBuf, Vec<String>> = HashMap::new();

        for snapshot in snapshots {
            let file_path = self.get_snapshot_path(
                &snapshot.symbol,
                &snapshot.event_type,
                snapshot.timestamp_ms,
            );

            // Format price and quantity to 12 decimal places
            let price_formatted = format!("{:.12}", snapshot.price.parse::<f64>().unwrap_or(0.0));
            let quantity_formatted =
                format!("{:.12}", snapshot.quantity.parse::<f64>().unwrap_or(0.0));

            let row = format!(
                "{},{},{},{},{},{},{},{},{},{},{}",
                snapshot.exchange,
                snapshot.market,
                snapshot.datatype,
                snapshot.timestamp_ms,
                Self::iso_utc(snapshot.timestamp_ms),
                snapshot.symbol,
                snapshot.event_type,
                snapshot.side,
                price_formatted,
                quantity_formatted,
                snapshot.last_update_id
            );

            file_groups.entry(file_path).or_default().push(row);
        }

        for (file_path, rows) in file_groups {
            let buffer = self.get_or_create_buffer(&file_path).await?;
            for row in rows {
                self.write_row(buffer.clone(), &row).await?;
            }
        }

        Ok(())
    }

    pub async fn write_trade(&self, trade: &TradeCsv) -> Result<()> {
        // Optionally skip raw disk persistence for symbols configured in [storage]
        // (klines + REST metrics are still collected for them).
        if !self.config.storage.should_persist_trades(&trade.symbol) {
            return Ok(());
        }

        let file_path = self.get_daily_file_path("trades", &trade.symbol, trade.timestamp_ms);
        let buffer = self.get_or_create_buffer(&file_path).await?;

        // Format price and quantity to 12 decimal places
        let price_formatted = format!("{:.12}", trade.price.parse::<f64>().unwrap_or(0.0));
        let quantity_formatted = format!("{:.12}", trade.quantity.parse::<f64>().unwrap_or(0.0));

        let row = format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{}",
            trade.exchange,
            trade.market,
            trade.datatype,
            trade.timestamp_ms,
            Self::iso_utc(trade.timestamp_ms),
            trade.symbol,
            trade.trade_id,
            trade.side,
            price_formatted,
            quantity_formatted,
            trade.is_buyer_maker,
            trade.first_update_id,
            trade.last_update_id
        );

        self.write_row(buffer, &row).await
    }

    pub async fn write_kline(&self, kline: &KlineCsv) -> Result<()> {
        let file_path = self.get_daily_file_path("klines", &kline.symbol, kline.timestamp_ms);
        let buffer = self.get_or_create_buffer(&file_path).await?;

        // Format all numeric values to 12 decimal places
        let open_formatted = format!("{:.12}", kline.open.parse::<f64>().unwrap_or(0.0));
        let high_formatted = format!("{:.12}", kline.high.parse::<f64>().unwrap_or(0.0));
        let low_formatted = format!("{:.12}", kline.low.parse::<f64>().unwrap_or(0.0));
        let close_formatted = format!("{:.12}", kline.close.parse::<f64>().unwrap_or(0.0));
        let volume_formatted = format!("{:.12}", kline.volume.parse::<f64>().unwrap_or(0.0));
        let quote_volume_formatted =
            format!("{:.12}", kline.quote_volume.parse::<f64>().unwrap_or(0.0));
        let taker_buy_base_formatted =
            format!("{:.12}", kline.taker_buy_base.parse::<f64>().unwrap_or(0.0));
        let taker_buy_quote_formatted = format!(
            "{:.12}",
            kline.taker_buy_quote.parse::<f64>().unwrap_or(0.0)
        );

        let row = format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            kline.exchange,
            kline.market,
            kline.datatype,
            kline.timestamp_ms,
            Self::iso_utc(kline.timestamp_ms),
            kline.symbol,
            open_formatted,
            high_formatted,
            low_formatted,
            close_formatted,
            volume_formatted,
            quote_volume_formatted,
            kline.trade_count,
            taker_buy_base_formatted,
            taker_buy_quote_formatted
        );

        self.write_row(buffer, &row).await
    }

    pub async fn write_liquidation(&self, liquidation: &LiquidationCsv) -> Result<()> {
        let file_path = self.get_daily_file_path(
            "liquidations",
            &liquidation.symbol,
            liquidation.timestamp_ms,
        );
        let buffer = self.get_or_create_buffer(&file_path).await?;

        // Format all numeric values to 12 decimal places
        let price_formatted = format!("{:.12}", liquidation.price.parse::<f64>().unwrap_or(0.0));
        let quantity_formatted =
            format!("{:.12}", liquidation.quantity.parse::<f64>().unwrap_or(0.0));
        let value_formatted = format!("{:.12}", liquidation.value.parse::<f64>().unwrap_or(0.0));

        let row = format!(
            "{},{},{},{},{},{},{},{},{},{}",
            liquidation.exchange,
            liquidation.market,
            liquidation.datatype,
            liquidation.timestamp_ms,
            Self::iso_utc(liquidation.timestamp_ms),
            liquidation.symbol,
            liquidation.side,
            price_formatted,
            quantity_formatted,
            value_formatted
        );

        self.write_row(buffer, &row).await
    }

    pub async fn write_funding_rate(&self, funding_rate: &FundingRateCsv) -> Result<()> {
        debug!(
            "write_funding_rate() called for symbol={} at ts={}",
            funding_rate.symbol, funding_rate.timestamp_ms
        );
        let file_path = self.get_daily_file_path(
            "funding_rates",
            &funding_rate.symbol,
            funding_rate.timestamp_ms,
        );
        debug!("write_funding_rate() file_path={:?}", file_path);
        let buffer = self.get_or_create_buffer(&file_path).await?;

        // Format funding_rate to 12 decimal places
        let funding_rate_formatted = format!(
            "{:.12}",
            funding_rate.funding_rate.parse::<f64>().unwrap_or(0.0)
        );

        let row = format!(
            "{},{},{},{},{},{},{},{}",
            funding_rate.exchange,
            funding_rate.market,
            funding_rate.datatype,
            funding_rate.timestamp_ms,
            Self::iso_utc(funding_rate.timestamp_ms),
            funding_rate.symbol,
            funding_rate_formatted,
            funding_rate.next_funding_time
        );

        self.write_row(buffer, &row).await
    }

    pub async fn write_open_interest(&self, oi: &OpenInterestCsv) -> Result<()> {
        let file_path = self.get_daily_file_path("open_interest", &oi.symbol, oi.timestamp_ms);
        let buffer = self.get_or_create_buffer(&file_path).await?;

        // Format all numeric values to 12 decimal places
        let price_formatted = format!("{:.12}", oi.price.parse::<f64>().unwrap_or(0.0));
        let open_interest_formatted =
            format!("{:.12}", oi.open_interest.parse::<f64>().unwrap_or(0.0));
        let oi_value_formatted = format!("{:.12}", oi.oi_value.parse::<f64>().unwrap_or(0.0));

        let row = format!(
            "{},{},{},{},{},{},{},{},{}",
            oi.exchange,
            oi.market,
            oi.datatype,
            oi.timestamp_ms,
            Self::iso_utc(oi.timestamp_ms),
            oi.symbol,
            price_formatted,
            open_interest_formatted,
            oi_value_formatted
        );

        self.write_row(buffer, &row).await
    }

    pub async fn write_long_short_ratio(&self, ratio: &LongShortRatioCsv) -> Result<()> {
        let file_path =
            self.get_daily_file_path("long_short_ratio", &ratio.symbol, ratio.timestamp_ms);
        let buffer = self.get_or_create_buffer(&file_path).await?;

        // Format all ratio values to 12 decimal places
        let long_ratio_formatted =
            format!("{:.12}", ratio.long_ratio.parse::<f64>().unwrap_or(0.0));
        let short_ratio_formatted =
            format!("{:.12}", ratio.short_ratio.parse::<f64>().unwrap_or(0.0));
        let long_short_ratio_formatted = format!(
            "{:.12}",
            ratio.long_short_ratio.parse::<f64>().unwrap_or(0.0)
        );

        let row = format!(
            "{},{},{},{},{},{},{},{},{},{},{}",
            ratio.exchange,
            ratio.market,
            ratio.datatype,
            ratio.timestamp_ms,
            Self::iso_utc(ratio.timestamp_ms),
            ratio.symbol,
            ratio.period,
            ratio.ratio_type,
            long_ratio_formatted,
            short_ratio_formatted,
            long_short_ratio_formatted
        );

        self.write_row(buffer, &row).await
    }

    pub async fn write_mark_price_kline(&self, kline: &MarkPriceKlineCsv) -> Result<()> {
        let file_path =
            self.get_daily_file_path("mark_price_klines", &kline.symbol, kline.timestamp_ms);
        let buffer = self.get_or_create_buffer(&file_path).await?;

        // Format all numeric values to 12 decimal places
        let open_formatted = format!("{:.12}", kline.open.parse::<f64>().unwrap_or(0.0));
        let high_formatted = format!("{:.12}", kline.high.parse::<f64>().unwrap_or(0.0));
        let low_formatted = format!("{:.12}", kline.low.parse::<f64>().unwrap_or(0.0));
        let close_formatted = format!("{:.12}", kline.close.parse::<f64>().unwrap_or(0.0));

        let row = format!(
            "{},{},{},{},{},{},{},{},{},{}",
            kline.exchange,
            kline.market,
            kline.datatype,
            kline.timestamp_ms,
            Self::iso_utc(kline.timestamp_ms),
            kline.symbol,
            open_formatted,
            high_formatted,
            low_formatted,
            close_formatted
        );

        self.write_row(buffer, &row).await
    }

    pub async fn write_index_price_kline(&self, kline: &IndexPriceKlineCsv) -> Result<()> {
        let file_path =
            self.get_daily_file_path("index_price_klines", &kline.symbol, kline.timestamp_ms);
        let buffer = self.get_or_create_buffer(&file_path).await?;

        // Format all numeric values to 12 decimal places
        let open_formatted = format!("{:.12}", kline.open.parse::<f64>().unwrap_or(0.0));
        let high_formatted = format!("{:.12}", kline.high.parse::<f64>().unwrap_or(0.0));
        let low_formatted = format!("{:.12}", kline.low.parse::<f64>().unwrap_or(0.0));
        let close_formatted = format!("{:.12}", kline.close.parse::<f64>().unwrap_or(0.0));

        let row = format!(
            "{},{},{},{},{},{},{},{},{},{}",
            kline.exchange,
            kline.market,
            kline.datatype,
            kline.timestamp_ms,
            Self::iso_utc(kline.timestamp_ms),
            kline.symbol,
            open_formatted,
            high_formatted,
            low_formatted,
            close_formatted
        );

        self.write_row(buffer, &row).await
    }

    pub async fn write_taker_buy_sell_ratio(&self, ratio: &TakerBuySellVolumeCsv) -> Result<()> {
        let file_path =
            self.get_daily_file_path("taker_buy_sell_ratio", &ratio.symbol, ratio.timestamp_ms);
        let buffer = self.get_or_create_buffer(&file_path).await?;

        // Format all numeric values to 12 decimal places
        let buy_volume_formatted =
            format!("{:.12}", ratio.buy_volume.parse::<f64>().unwrap_or(0.0));
        let sell_volume_formatted =
            format!("{:.12}", ratio.sell_volume.parse::<f64>().unwrap_or(0.0));
        let buy_sell_ratio_formatted =
            format!("{:.12}", ratio.buy_sell_ratio.parse::<f64>().unwrap_or(0.0));

        let row = format!(
            "{},{},{},{},{},{},{},{},{},{}",
            ratio.exchange,
            ratio.market,
            ratio.datatype,
            ratio.timestamp_ms,
            Self::iso_utc(ratio.timestamp_ms),
            ratio.symbol,
            ratio.period,
            buy_volume_formatted,
            sell_volume_formatted,
            buy_sell_ratio_formatted
        );

        self.write_row(buffer, &row).await
    }

    pub async fn write_premium_index_kline(&self, kline: &PremiumIndexKlineCsv) -> Result<()> {
        let file_path =
            self.get_daily_file_path("premium_index_klines", &kline.symbol, kline.timestamp_ms);
        let buffer = self.get_or_create_buffer(&file_path).await?;

        // Format all numeric values to 12 decimal places
        let open_formatted = format!("{:.12}", kline.open.parse::<f64>().unwrap_or(0.0));
        let high_formatted = format!("{:.12}", kline.high.parse::<f64>().unwrap_or(0.0));
        let low_formatted = format!("{:.12}", kline.low.parse::<f64>().unwrap_or(0.0));
        let close_formatted = format!("{:.12}", kline.close.parse::<f64>().unwrap_or(0.0));

        let row = format!(
            "{},{},{},{},{},{},{},{},{},{}",
            kline.exchange,
            kline.market,
            kline.datatype,
            kline.timestamp_ms,
            Self::iso_utc(kline.timestamp_ms),
            kline.symbol,
            open_formatted,
            high_formatted,
            low_formatted,
            close_formatted
        );

        self.write_row(buffer, &row).await
    }

    /// Write depth update to in-memory buffer (sorted at flush time)
    /// Emergency flush triggered if buffer exceeds max_depth_buffer_rows
    async fn write_depth_update_buffered(
        &self,
        buffer: Arc<Mutex<FileBuffer>>,
        last_update_id: u64,
        row: &str,
    ) -> Result<()> {
        let row_owned = row.to_string();
        let max_rows = self.max_depth_buffer_rows;
        let buffer_clone = buffer.clone();

        let needs_emergency_flush = tokio::task::spawn_blocking(move || -> Result<bool> {
            let mut buf = buffer_clone.lock();

            // Add to in-memory buffer (unsorted for O(1) append)
            if let Some(ref mut depth_buffer) = buf.depth_update_buffer {
                depth_buffer.push(DepthUpdateRow {
                    last_update_id,
                    csv_line: row_owned,
                });

                // Check if we need emergency flush to prevent unbounded memory growth
                Ok(depth_buffer.len() >= max_rows)
            } else {
                Ok(false)
            }
        })
        .await??;

        // Emergency flush if buffer is too large (prevents memory leak)
        if needs_emergency_flush {
            log::warn!(
                "Emergency flush triggered: depth buffer exceeded {} rows",
                max_rows
            );
            self.flush_buffer(buffer).await?;
        }

        Ok(())
    }

    /// Flush a single buffer (extracted for reuse)
    async fn flush_buffer(&self, buffer: Arc<Mutex<FileBuffer>>) -> Result<()> {
        tokio::task::spawn_blocking(move || -> Result<()> { buffer.lock().flush_locked() })
            .await??;

        Ok(())
    }

    /// Write a row to the buffer (uses spawn_blocking) - for non-depth_update data
    async fn write_row(&self, buffer: Arc<Mutex<FileBuffer>>, row: &str) -> Result<()> {
        let row_owned = row.to_string();
        let flush_interval = self.flush_interval;

        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut buf = buffer.lock();
            writeln!(buf.writer, "{}", row_owned)?;
            buf.row_count += 1;

            let elapsed = buf.last_flush.elapsed();
            if elapsed >= flush_interval || buf.row_count >= 1000 {
                buf.writer.flush()?;
                buf.last_flush = std::time::Instant::now();
                buf.row_count = 0;
            }
            Ok(())
        })
        .await??;

        Ok(())
    }

    /// Force flush all buffers (uses spawn_blocking)
    pub async fn flush_all(&self) -> Result<()> {
        let buffers: Vec<_> = self.buffers.iter().map(|e| e.value().clone()).collect();

        // Log-and-continue: one bad file (ENOSPC/EIO) must not skip every buffer
        // ordered after it; that would strand their in-memory rows until a crash.
        for buffer in buffers {
            if let Err(e) = self.flush_buffer(buffer).await {
                error!("flush_all: a buffer failed to flush (continuing): {}", e);
            }
        }

        Ok(())
    }

    /// Flush specific symbol's buffers (uses spawn_blocking)
    pub async fn flush_symbol(&self, symbol: &str) -> Result<()> {
        let buffers: Vec<_> = self
            .buffers
            .iter()
            .filter(|e| e.key().contains(symbol)) // Symbol already lowercase
            .map(|e| e.value().clone())
            .collect();

        for buffer in buffers {
            self.flush_buffer(buffer).await?;
        }

        Ok(())
    }

    /// Close and finalize a file (used for hourly rotation) (uses spawn_blocking)
    pub async fn finalize_file(&self, file_path: &Path) -> Result<()> {
        let path_str = file_path.to_string_lossy().to_string();

        if let Some((_, buffer)) = self.buffers.remove(&path_str) {
            self.flush_buffer(buffer).await?;
        }

        Ok(())
    }

    /// Cleanup stale file buffers
    /// Removes buffers for:
    /// 1. Files from previous days (should be rotated)
    /// 2. Symbols not written to in last 30 minutes (delisted/inactive)
    pub async fn cleanup_stale_buffers(&self) -> Result<()> {
        let now = chrono::Utc::now();
        let current_date = now.format("%Y-%m-%d").to_string();

        let stale_paths: Vec<String> = self
            .buffers
            .iter()
            .filter_map(|entry| {
                let path_str = entry.key().clone();
                let buffer = entry.value();

                // Check 1: Is this from a previous day? Path layout is
                // <base>/<datatype>/<symbol>/<date>/<file>.csv, so the date is the
                // file's parent directory, so read it positionally and confirm it
                // parses as a date, rather than pattern-matching the first 10-char
                // "20…" segment (a symbol directory could match that first).
                if !path_str.contains(&current_date) {
                    if let Some(date_dir) = std::path::Path::new(&path_str)
                        .parent()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                    {
                        if date_dir != current_date
                            && chrono::NaiveDate::parse_from_str(date_dir, "%Y-%m-%d").is_ok()
                        {
                            log::info!("Marking old date buffer for cleanup: {}", path_str);
                            return Some(path_str);
                        }
                    }
                }

                // Check 2: Has this buffer been idle for 30+ minutes?
                if let Some(buf_guard) = buffer.try_lock() {
                    let elapsed = buf_guard.last_flush.elapsed();
                    if elapsed > std::time::Duration::from_secs(1800) {
                        // 30 min
                        log::info!(
                            "Marking idle buffer for cleanup (idle for {}s): {}",
                            elapsed.as_secs(),
                            path_str
                        );
                        return Some(path_str);
                    }
                }

                None
            })
            .collect();

        if stale_paths.is_empty() {
            log::debug!("No stale CSV buffers to clean up");
            return Ok(());
        }

        log::info!("Cleaning up {} stale CSV file buffers", stale_paths.len());

        for path_str in &stale_paths {
            if let Some((_, buffer)) = self.buffers.remove(path_str) {
                if let Err(e) = self.flush_buffer(buffer).await {
                    log::warn!(
                        "Failed to flush buffer before cleanup: {} - {:?}",
                        path_str,
                        e
                    );
                }
            }
        }

        log::info!("Cleaned up {} stale CSV buffers", stale_paths.len());

        // The pages freed by a mass eviction go back to the OS via the global
        // allocator: on Linux this is jemalloc with a background purge thread
        // (see main.rs and .cargo/config.toml), so no explicit trim is needed.

        Ok(())
    }

    pub async fn start_periodic_flush(&self) -> Result<()> {
        let writer = self.clone();
        let flush_interval = self.flush_interval;

        tokio::spawn(async move {
            let mut interval_timer = interval(flush_interval);

            loop {
                interval_timer.tick().await;

                if let Err(e) = writer.flush_all().await {
                    log::error!("Periodic flush failed: {}", e);
                }
            }
        });

        // Periodic cleanup task (every 2 minutes)
        let writer_cleanup = self.clone();
        tokio::spawn(async move {
            let mut cleanup_interval = tokio::time::interval(tokio::time::Duration::from_secs(120)); // 2 min

            loop {
                cleanup_interval.tick().await;

                if let Err(e) = writer_cleanup.cleanup_stale_buffers().await {
                    log::error!("CSV buffer cleanup failed: {:?}", e);
                }
            }
        });

        // Rotation-aware cleanup: the 2-minute tick can leave the whole old
        // day's buffer set (thousands of 64 KB writers) resident for up to two
        // minutes past midnight while the new day's set is already being
        // created: a double-residency burst that ratchets peak RSS. Run a
        // cleanup seconds after each UTC midnight instead of waiting.
        let writer_midnight = self.clone();
        tokio::spawn(async move {
            loop {
                let now = Utc::now();
                let next = (now + chrono::Duration::days(1))
                    .date_naive()
                    .and_hms_opt(0, 0, 5)
                    .expect("valid time")
                    .and_utc();
                let wait = (next - now).to_std().unwrap_or(Duration::from_secs(86_400));
                tokio::time::sleep(wait).await;

                log::info!("Midnight rotation: running immediate stale-buffer cleanup");
                if let Err(e) = writer_midnight.cleanup_stale_buffers().await {
                    log::error!("Midnight buffer cleanup failed: {:?}", e);
                }
            }
        });

        Ok(())
    }

    /// Start hourly rotation task for depth update files
    pub async fn start_hourly_rotation(&self) -> Result<()> {
        let writer = self.clone();

        tokio::spawn(async move {
            let now = Utc::now();
            let next_hour = (now + chrono::Duration::hours(1))
                .with_minute(0)
                .unwrap()
                .with_second(0)
                .unwrap()
                .with_nanosecond(0)
                .unwrap();

            let until_next_hour = (next_hour - now)
                .to_std()
                .unwrap_or(Duration::from_secs(3600));

            tokio::time::sleep(until_next_hour).await;

            let mut interval = interval(Duration::from_secs(3600));

            loop {
                interval.tick().await;

                // Find all depth update files for the previous hour
                let current_hour = Utc::now().hour();
                let prev_hour = if current_hour == 0 {
                    23
                } else {
                    current_hour - 1
                };

                let mut files_to_finalize = Vec::new();

                for entry in writer.buffers.iter() {
                    let path_str = entry.key();
                    // Check if it's a depth update file from previous hour
                    if path_str.contains("depth_updates")
                        && path_str
                            .contains(&format!("{:02}00-to-{:02}00", prev_hour, current_hour))
                    {
                        files_to_finalize.push(PathBuf::from(path_str.clone()));
                    }
                }

                for file_path in files_to_finalize {
                    if let Err(e) = writer.finalize_file(&file_path).await {
                        log::error!("Failed to finalize file {:?}: {}", file_path, e);
                    } else {
                        log::info!("Finalized hourly file: {:?}", file_path);
                    }
                }
                // The hour's freed buffer pages return to the OS via jemalloc's
                // background purge (see main.rs / .cargo/config.toml).
            }
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AggTradeWs, KlineCsv, OpenInterestCsv};

    /// 2023-11-14T22:13:20.000Z, a fixed, human-checkable instant.
    const TS: i64 = 1_700_000_000_000;

    fn test_writer(dir: &Path) -> CsvWriter {
        let mut config = Config::from_file("config.example.toml").unwrap();
        config.output.base_dir = dir.to_string_lossy().into_owned();
        CsvWriter::new(Arc::new(config)).unwrap()
    }

    fn agg_trade(is_buyer_maker: bool) -> AggTradeWs {
        AggTradeWs {
            event_type: "aggTrade".to_string(),
            event_time: TS,
            symbol: "BTCUSDT".to_string(),
            aggregate_trade_id: 42,
            price: "62456.60".to_string(),
            quantity: "0.017".to_string(),
            first_trade_id: 100,
            last_trade_id: 101,
            trade_time: TS,
            is_buyer_maker,
        }
    }

    #[test]
    fn side_derivation_buyer_maker_means_sell() {
        // is_buyer_maker=true → the taker sold; side must be lowercase.
        let sell = agg_trade(true).to_csv();
        assert_eq!(sell.side, "sell");
        assert_eq!(sell.is_buyer_maker, 1);
        let buy = agg_trade(false).to_csv();
        assert_eq!(buy.side, "buy");
        assert_eq!(buy.is_buyer_maker, 0);
        // Binance's UPPERCASE symbol is normalized on conversion.
        assert_eq!(buy.symbol, "btcusdt");
    }

    #[tokio::test]
    async fn trade_roundtrip_matches_schema_v2() {
        let dir = tempfile::tempdir().unwrap();
        let w = test_writer(dir.path());

        w.write_trade(&agg_trade(true).to_csv()).await.unwrap();
        w.flush_all().await.unwrap();

        let path = w.get_daily_file_path("trades", "btcusdt", TS);
        let content = std::fs::read_to_string(&path).unwrap();
        let mut lines = content.lines();

        // Header is the schema constant, verbatim.
        assert_eq!(lines.next().unwrap(), constants::HEADER_TRADES);

        // Row aligns column-for-column with the header.
        let row: Vec<&str> = lines.next().unwrap().split(',').collect();
        assert_eq!(row.len(), constants::HEADER_TRADES.split(',').count());
        assert_eq!(
            &row[..6],
            &[
                "binance",
                "futures",
                "trades",
                "1700000000000",
                "2023-11-14T22:13:20.000Z",
                "btcusdt"
            ]
        );
        assert_eq!(row[6], "42"); // trade_id
        assert_eq!(row[7], "sell"); // side sits BEFORE price/quantity
        assert!((row[8].parse::<f64>().unwrap() - 62456.60).abs() < 1e-9);
        assert!((row[9].parse::<f64>().unwrap() - 0.017).abs() < 1e-9);
    }

    #[tokio::test]
    async fn kline_roundtrip_matches_schema_v2() {
        let dir = tempfile::tempdir().unwrap();
        let w = test_writer(dir.path());

        let k = KlineCsv {
            exchange: "binance".to_string(),
            market: "futures".to_string(),
            datatype: "klines".to_string(),
            timestamp_ms: TS,
            symbol: "ethusdt".to_string(),
            open: "3000.1".to_string(),
            high: "3001.2".to_string(),
            low: "2999.3".to_string(),
            close: "3000.4".to_string(),
            volume: "12.5".to_string(),
            quote_volume: "37505.0".to_string(),
            trade_count: 87,
            taker_buy_base: "6.0".to_string(),
            taker_buy_quote: "18002.4".to_string(),
        };
        w.write_kline(&k).await.unwrap();
        w.flush_all().await.unwrap();

        let path = w.get_daily_file_path("klines", "ethusdt", TS);
        let content = std::fs::read_to_string(&path).unwrap();
        let mut lines = content.lines();
        assert_eq!(lines.next().unwrap(), constants::HEADER_KLINES);
        let row: Vec<&str> = lines.next().unwrap().split(',').collect();
        assert_eq!(row.len(), constants::HEADER_KLINES.split(',').count());
        assert_eq!(row[4], "2023-11-14T22:13:20.000Z");
        // OHLC land in header order.
        assert!((row[6].parse::<f64>().unwrap() - 3000.1).abs() < 1e-9);
        assert!((row[9].parse::<f64>().unwrap() - 3000.4).abs() < 1e-9);
    }

    #[tokio::test]
    async fn open_interest_roundtrip_has_mark_price_column() {
        let dir = tempfile::tempdir().unwrap();
        let w = test_writer(dir.path());

        let oi = OpenInterestCsv {
            exchange: "binance".to_string(),
            market: "futures".to_string(),
            datatype: "open_interest".to_string(),
            timestamp_ms: TS,
            symbol: "solusdt".to_string(),
            price: "150.0".to_string(),
            open_interest: "1000.0".to_string(),
            oi_value: "150000.0".to_string(),
        };
        w.write_open_interest(&oi).await.unwrap();
        w.flush_all().await.unwrap();

        let path = w.get_daily_file_path("open_interest", "solusdt", TS);
        let content = std::fs::read_to_string(&path).unwrap();
        let mut lines = content.lines();

        // The schema renamed `price` → `mark_price`; the file must say so.
        let header = lines.next().unwrap();
        assert_eq!(header, constants::HEADER_OPEN_INTEREST);
        assert!(header.contains(",mark_price,"));

        let row: Vec<&str> = lines.next().unwrap().split(',').collect();
        let mark: f64 = row[6].parse().unwrap();
        let qty: f64 = row[7].parse().unwrap();
        let value: f64 = row[8].parse().unwrap();
        assert!((mark * qty - value).abs() < 1e-6);
    }

    #[tokio::test]
    async fn appending_to_existing_file_writes_no_second_header() {
        let dir = tempfile::tempdir().unwrap();
        let w = test_writer(dir.path());

        w.write_trade(&agg_trade(true).to_csv()).await.unwrap();
        w.write_trade(&agg_trade(false).to_csv()).await.unwrap();
        w.flush_all().await.unwrap();

        let path = w.get_daily_file_path("trades", "btcusdt", TS);
        let content = std::fs::read_to_string(&path).unwrap();
        let headers = content
            .lines()
            .filter(|l| *l == constants::HEADER_TRADES)
            .count();
        assert_eq!(headers, 1);
        assert_eq!(content.lines().count(), 3); // header + 2 rows
    }

    #[tokio::test]
    async fn cleanup_evicts_previous_day_buffers_and_flushes_them() {
        let dir = tempfile::tempdir().unwrap();
        let w = test_writer(dir.path());

        // TS is a 2023 date, so this buffer belongs to a "previous day".
        w.write_trade(&agg_trade(true).to_csv()).await.unwrap();
        assert_eq!(w.buffers.len(), 1);

        w.cleanup_stale_buffers().await.unwrap();
        assert_eq!(w.buffers.len(), 0, "old-date buffer must be evicted");

        // The buffered row was flushed on the way out, not dropped.
        let content =
            std::fs::read_to_string(w.get_daily_file_path("trades", "btcusdt", TS)).unwrap();
        assert_eq!(content.lines().count(), 2); // header + row
    }

    #[test]
    fn iso_utc_formats_utc_millis() {
        assert_eq!(CsvWriter::iso_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            CsvWriter::iso_utc(1_700_000_000_123),
            "2023-11-14T22:13:20.123Z"
        );
    }

    #[test]
    fn daily_paths_partition_by_datatype_symbol_date() {
        let dir = tempfile::tempdir().unwrap();
        let w = test_writer(dir.path());
        let p = w.get_daily_file_path("trades", "btcusdt", TS);
        let s = p.to_string_lossy();
        assert!(s.ends_with("trades/btcusdt/2023-11-14/btcusdt_trades_2023-11-14.csv"));
    }
}
