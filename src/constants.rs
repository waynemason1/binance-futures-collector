// Shared string constants. Static slices avoid repeated allocations.

pub const EXCHANGE: &str = "binance";
pub const MARKET: &str = "futures";

// Data types
pub const DATATYPE_ORDERBOOKS: &str = "orderbooks";
pub const DATATYPE_TRADES: &str = "trades";
pub const DATATYPE_KLINES: &str = "klines";
pub const DATATYPE_LIQUIDATIONS: &str = "liquidations";
pub const DATATYPE_FUNDING_RATES: &str = "funding_rates";
pub const DATATYPE_OPEN_INTEREST: &str = "open_interest";
pub const DATATYPE_LONG_SHORT_RATIO: &str = "long_short_ratio";
pub const DATATYPE_MARK_PRICE_KLINES: &str = "mark_price_klines";
pub const DATATYPE_INDEX_PRICE_KLINES: &str = "index_price_klines";
pub const DATATYPE_TAKER_BUY_SELL_RATIO: &str = "taker_buy_sell_ratio";
pub const DATATYPE_PREMIUM_INDEX_KLINES: &str = "premium_index_klines";

// Sides
pub const SIDE_BID: &str = "bid";
pub const SIDE_ASK: &str = "ask";
pub const SIDE_BUY: &str = "buy";
pub const SIDE_SELL: &str = "sell";

// CSV Headers (pre-allocated to avoid runtime allocation)
pub const HEADER_DEPTH_UPDATES: &str = "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,first_update_id,last_update_id,prev_update_id,bids_json,asks_json";
pub const HEADER_DEPTH_SNAPSHOT: &str = "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,event_type,side,price,quantity,last_update_id";
pub const HEADER_TRADES: &str = "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,trade_id,side,price,quantity,is_buyer_maker,first_update_id,last_update_id";
pub const HEADER_KLINES: &str = "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,open,high,low,close,volume,quote_volume,trade_count,taker_buy_base,taker_buy_quote";
pub const HEADER_LIQUIDATIONS: &str =
    "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,side,price,quantity,value";
pub const HEADER_FUNDING_RATES: &str =
    "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,funding_rate,next_funding_time";
pub const HEADER_OPEN_INTEREST: &str =
    "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,mark_price,open_interest,oi_value";
pub const HEADER_LONG_SHORT_RATIO: &str = "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,period,ratio_type,long_ratio,short_ratio,long_short_ratio";
pub const HEADER_MARK_INDEX_KLINES: &str =
    "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,open,high,low,close";
pub const HEADER_TAKER_BUY_SELL_RATIO: &str = "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,period,buy_volume,sell_volume,buy_sell_ratio";
pub const HEADER_PREMIUM_INDEX_KLINES: &str =
    "exchange,market,datatype,timestamp_ms,datetime_utc,symbol,open,high,low,close";
