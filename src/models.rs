use crate::constants;
use serde::{Deserialize, Serialize};

/// WebSocket depth update message (incoming from Binance)
#[derive(Debug, Clone, Deserialize)]
pub struct DepthUpdateWs {
    #[serde(rename = "e")]
    pub event_type: String, // "depthUpdate"
    #[serde(rename = "E")]
    pub event_time: i64,
    #[serde(rename = "T")]
    pub transaction_time: i64,
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "U")]
    pub first_update_id: u64,
    #[serde(rename = "u")]
    pub final_update_id: u64,
    #[serde(rename = "pu")]
    pub prev_update_id: u64,
    #[serde(rename = "b")]
    pub bids: Vec<[String; 2]>, // [[price, quantity]]
    #[serde(rename = "a")]
    pub asks: Vec<[String; 2]>, // [[price, quantity]]

    // Pre-converted floats (added during processing, not from the wire)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b_floats: Option<Vec<(f64, f64)>>, // [(price, quantity)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a_floats: Option<Vec<(f64, f64)>>, // [(price, quantity)]
}

/// CSV format for depth updates
#[derive(Debug, Clone, Serialize)]
pub struct DepthUpdateCsv {
    pub exchange: String,
    pub market: String,
    pub datatype: String,
    pub timestamp_ms: i64,
    pub symbol: String,
    pub first_update_id: u64,
    pub last_update_id: u64,
    pub prev_update_id: u64,
    pub bids_json: String,
    pub asks_json: String,
}

/// REST API depth snapshot response
#[derive(Debug, Clone, Deserialize)]
pub struct DepthSnapshotRest {
    #[serde(rename = "lastUpdateId")]
    pub last_update_id: u64,
    pub bids: Vec<[String; 2]>,
    pub asks: Vec<[String; 2]>,
}

/// CSV format for depth snapshots (one row per price level)
#[derive(Debug, Clone, Serialize)]
pub struct DepthSnapshotCsv {
    pub exchange: String,
    pub market: String,
    pub datatype: String,
    pub timestamp_ms: i64,
    pub symbol: String,
    pub event_type: String, // depth_snapshot_initial, depth_snapshot, etc.
    pub side: String,       // "bid" or "ask"
    pub price: String,      // 12 decimal places
    pub quantity: String,   // 12 decimal places
    pub last_update_id: u64,
}

/// WebSocket aggregated trade message
#[derive(Debug, Clone, Deserialize)]
pub struct AggTradeWs {
    #[serde(rename = "e")]
    pub event_type: String, // "aggTrade"
    #[serde(rename = "E")]
    pub event_time: i64,
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "a")]
    pub aggregate_trade_id: i64,
    #[serde(rename = "p")]
    pub price: String,
    #[serde(rename = "q")]
    pub quantity: String,
    #[serde(rename = "f")]
    pub first_trade_id: i64,
    #[serde(rename = "l")]
    pub last_trade_id: i64,
    #[serde(rename = "T")]
    pub trade_time: i64,
    #[serde(rename = "m")]
    pub is_buyer_maker: bool,
}

/// CSV format for trades
#[derive(Debug, Clone, Serialize)]
pub struct TradeCsv {
    pub exchange: String,
    pub market: String,
    pub datatype: String,
    pub timestamp_ms: i64,
    pub symbol: String,
    pub trade_id: i64,
    pub price: String,
    pub quantity: String,
    pub side: String,        // "Buy" or "Sell"
    pub is_buyer_maker: i32, // 1 or 0
    pub first_update_id: i64,
    pub last_update_id: i64,
}

/// WebSocket kline message
#[derive(Debug, Clone, Deserialize)]
pub struct KlineWs {
    #[serde(rename = "e")]
    pub event_type: String, // "kline"
    #[serde(rename = "E")]
    pub event_time: i64,
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "k")]
    pub kline: KlineData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KlineData {
    #[serde(rename = "t")]
    pub open_time: i64,
    #[serde(rename = "T")]
    pub close_time: i64,
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "i")]
    pub interval: String,
    #[serde(rename = "o")]
    pub open: String,
    #[serde(rename = "h")]
    pub high: String,
    #[serde(rename = "l")]
    pub low: String,
    #[serde(rename = "c")]
    pub close: String,
    #[serde(rename = "v")]
    pub volume: String,
    #[serde(rename = "q")]
    pub quote_volume: String,
    #[serde(rename = "n")]
    pub trade_count: i64,
    #[serde(rename = "x")]
    pub is_closed: bool,
    #[serde(rename = "V")]
    pub taker_buy_base_volume: String,
    #[serde(rename = "Q")]
    pub taker_buy_quote_volume: String,
}

/// CSV format for klines
#[derive(Debug, Clone, Serialize)]
pub struct KlineCsv {
    pub exchange: String,
    pub market: String,
    pub datatype: String,
    pub timestamp_ms: i64,
    pub symbol: String,
    pub open: String,
    pub high: String,
    pub low: String,
    pub close: String,
    pub volume: String,
    pub quote_volume: String,
    pub trade_count: i64,
    pub taker_buy_base: String,
    pub taker_buy_quote: String,
}

/// WebSocket liquidation order message
#[derive(Debug, Clone, Deserialize)]
pub struct LiquidationWs {
    #[serde(rename = "e")]
    pub event_type: String, // "forceOrder"
    #[serde(rename = "E")]
    pub event_time: i64,
    #[serde(rename = "o")]
    pub order: LiquidationOrder,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LiquidationOrder {
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "S")]
    pub side: String, // "SELL" or "BUY"
    #[serde(rename = "p")]
    pub price: String,
    #[serde(rename = "q")]
    pub quantity: String,
    #[serde(rename = "T")]
    pub time: i64,
}

/// CSV format for liquidations
#[derive(Debug, Clone, Serialize)]
pub struct LiquidationCsv {
    pub exchange: String,
    pub market: String,
    pub datatype: String,
    pub timestamp_ms: i64,
    pub symbol: String,
    pub side: String, // lowercase "buy" or "sell"
    pub price: String,
    pub quantity: String,
    pub value: String, // price * quantity
}

/// REST API funding rate response (part of premiumIndex endpoint)
#[derive(Debug, Clone, Deserialize)]
pub struct FundingRateRest {
    pub symbol: String,
    #[serde(rename = "markPrice")]
    pub mark_price: String,
    #[serde(rename = "indexPrice")]
    pub index_price: String,
    #[serde(rename = "estimatedSettlePrice")]
    pub estimated_settle_price: String,
    #[serde(rename = "lastFundingRate")]
    pub last_funding_rate: String,
    #[serde(rename = "nextFundingTime")]
    pub next_funding_time: i64,
    #[serde(rename = "interestRate")]
    pub interest_rate: String,
    pub time: i64,
}

/// CSV format for funding rates
#[derive(Debug, Clone, Serialize)]
pub struct FundingRateCsv {
    pub exchange: String,
    pub market: String,
    pub datatype: String,
    pub timestamp_ms: i64,
    pub symbol: String,
    pub funding_rate: String,
    pub next_funding_time: i64,
}

/// REST API open interest response
#[derive(Debug, Clone, Deserialize)]
pub struct OpenInterestRest {
    #[serde(rename = "openInterest")]
    pub open_interest: String,
    pub symbol: String,
    pub time: i64,
}

/// CSV format for open interest
#[derive(Debug, Clone, Serialize)]
pub struct OpenInterestCsv {
    pub exchange: String,
    pub market: String,
    pub datatype: String,
    pub timestamp_ms: i64,
    pub symbol: String,
    pub price: String, // Current mark price
    pub open_interest: String,
    pub oi_value: String, // open_interest * price
}

/// REST API long/short ratio response
#[derive(Debug, Clone, Deserialize)]
pub struct LongShortRatioRest {
    pub symbol: String,
    #[serde(rename = "pair", skip_serializing_if = "Option::is_none")]
    pub pair: Option<String>, // Not always present
    #[serde(rename = "longAccount")]
    pub long_account: Option<String>, // Binance global ratio
    #[serde(rename = "shortAccount")]
    pub short_account: Option<String>, // Binance global ratio
    #[serde(rename = "longShortRatio")]
    pub long_short_ratio: String,
    pub timestamp: i64,
}

/// CSV format for long/short ratio
#[derive(Debug, Clone, Serialize)]
pub struct LongShortRatioCsv {
    pub exchange: String,
    pub market: String,
    pub datatype: String,
    pub timestamp_ms: i64,
    pub symbol: String,
    pub period: String,     // "5m"
    pub ratio_type: String, // "global", "top_trader_account", "top_trader_position"
    pub long_ratio: String,
    pub short_ratio: String,
    pub long_short_ratio: String,
}

/// REST API mark price kline - Binance returns as array
pub type MarkPriceKlineRest = Vec<serde_json::Value>; // [open_time, open, high, low, close, ...]

/// CSV format for mark price klines
#[derive(Debug, Clone, Serialize)]
pub struct MarkPriceKlineCsv {
    pub exchange: String,
    pub market: String,
    pub datatype: String,
    pub timestamp_ms: i64,
    pub symbol: String,
    pub open: String,
    pub high: String,
    pub low: String,
    pub close: String,
}

/// CSV format for index price klines (same structure as mark price)
pub type IndexPriceKlineCsv = MarkPriceKlineCsv;

/// Symbol information from exchange info API
#[derive(Debug, Clone, Deserialize)]
pub struct ExchangeInfoSymbol {
    pub symbol: String,
    pub status: String,
    #[serde(rename = "baseAsset")]
    pub base_asset: String,
    #[serde(rename = "quoteAsset")]
    pub quote_asset: String,
    #[serde(rename = "underlyingType")]
    pub underlying_type: Option<String>,
    #[serde(rename = "contractType")]
    pub contract_type: String,
}

/// Exchange info response
#[derive(Debug, Clone, Deserialize)]
pub struct ExchangeInfo {
    pub symbols: Vec<ExchangeInfoSymbol>,
}

/// Helper functions for formatting
impl DepthUpdateWs {
    pub fn to_csv(&self) -> DepthUpdateCsv {
        // NOTE: depth_updates are DELTAS (price level changes), not full orderbook snapshots
        // - Quantity > 0: Update price level to this quantity
        // - Quantity = 0: DELETE this price level
        // - Order doesn't matter - these are differential updates
        // - "Far from market" prices ($1000 bids) are legitimate limit orders
        // When reconstructing orderbook: apply deltas to snapshot, then sort to get best bid/ask

        DepthUpdateCsv {
            exchange: constants::EXCHANGE.to_string(),
            market: constants::MARKET.to_string(),
            datatype: constants::DATATYPE_ORDERBOOKS.to_string(),
            timestamp_ms: self.event_time,
            symbol: self.symbol.to_lowercase(), // Convert Binance UPPERCASE to lowercase
            first_update_id: self.first_update_id,
            last_update_id: self.final_update_id,
            prev_update_id: self.prev_update_id,
            bids_json: serde_json::to_string(&self.bids).unwrap_or_default(),
            asks_json: serde_json::to_string(&self.asks).unwrap_or_default(),
        }
    }
}

impl AggTradeWs {
    pub fn to_csv(&self) -> TradeCsv {
        TradeCsv {
            exchange: constants::EXCHANGE.to_string(),
            market: constants::MARKET.to_string(),
            datatype: constants::DATATYPE_TRADES.to_string(),
            timestamp_ms: self.trade_time,
            symbol: self.symbol.to_lowercase(), // Convert Binance UPPERCASE to lowercase
            trade_id: self.aggregate_trade_id,
            price: format_decimal_str(&self.price),
            quantity: format_decimal_str(&self.quantity),
            side: if self.is_buyer_maker { "sell" } else { "buy" }.to_string(),
            is_buyer_maker: if self.is_buyer_maker { 1 } else { 0 },
            first_update_id: self.first_trade_id,
            last_update_id: self.last_trade_id,
        }
    }
}

/// REST API taker buy/sell volume ratio response
#[derive(Debug, Clone, Deserialize)]
pub struct TakerBuySellVolumeRest {
    #[serde(rename = "buySellRatio")]
    pub buy_sell_ratio: String,
    #[serde(rename = "buyVol")]
    pub buy_vol: String,
    #[serde(rename = "sellVol")]
    pub sell_vol: String,
    pub timestamp: i64,
}

/// CSV format for taker buy/sell volume ratio
#[derive(Debug, Clone, Serialize)]
pub struct TakerBuySellVolumeCsv {
    pub exchange: String,
    pub market: String,
    pub datatype: String,
    pub timestamp_ms: i64,
    pub symbol: String,
    pub period: String,
    pub buy_volume: String,
    pub sell_volume: String,
    pub buy_sell_ratio: String,
}

/// CSV format for premium index klines (same structure as mark/index klines)
#[derive(Debug, Clone, Serialize)]
pub struct PremiumIndexKlineCsv {
    pub exchange: String,
    pub market: String,
    pub datatype: String,
    pub timestamp_ms: i64,
    pub symbol: String,
    pub open: String,
    pub high: String,
    pub low: String,
    pub close: String,
}

/// Format decimal string to 12 decimal places
pub fn format_decimal_str(value: &str) -> String {
    match value.parse::<f64>() {
        Ok(v) => format!("{:.12}", v),
        Err(_) => value.to_string(),
    }
}

/// Format decimal to 12 decimal places
pub fn format_decimal(value: f64) -> String {
    format!("{:.12}", value)
}
