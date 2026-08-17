"""Every validator must catch exactly the corruption it claims to catch.

Each test seeds one symbol with data that is clean except for a single,
deliberate defect, then asserts the corresponding check (and only it) fires.
"""
from conftest import (DAY, MIN, T0, H_FUNDING, H_KLINES, H_LIQ, H_LS, H_OI, H_TAKER, H_TRADES,
                      checks_of, clean_klines, kline_row, prefix, trade_row,
                      validate, write_csv)


# ------------------------------------------------------------------- klines
def test_clean_klines_pass_every_check(client):
    clean_klines("klokusdt")
    rep = validate(client, "klokusdt")
    assert rep["verdict"] == "clean"
    assert all(v == 0 for v in checks_of(rep, "klines").values())


def test_kline_gap_trips_continuity(client):
    rows = [kline_row("kgapusdt", T0 + i * MIN) for i in (0, 1, 2, 5, 6)]  # 3 min hole
    write_csv("klines", "kgapusdt", H_KLINES, rows)
    rep = validate(client, "kgapusdt")
    assert checks_of(rep, "klines")["1-minute continuity (missing candles)"] == 2  # minutes 3 and 4 missing
    assert rep["verdict"] == "issues"


def test_kline_high_below_low_trips_ohlc(client):
    rows = [kline_row("kohlcusdt", T0), kline_row("kohlcusdt", T0 + MIN, h=98.0, low=99.5)]
    write_csv("klines", "kohlcusdt", H_KLINES, rows)
    rep = validate(client, "kohlcusdt")
    assert checks_of(rep, "klines")["OHLC bounds (high=max, low=min)"] == 1


def test_duplicate_kline_timestamp_detected(client):
    rows = [kline_row("kdupusdt", T0), kline_row("kdupusdt", T0)]
    write_csv("klines", "kdupusdt", H_KLINES, rows)
    rep = validate(client, "kdupusdt")
    assert checks_of(rep, "klines")["unique timestamps"] == 1


def test_wrong_datetime_utc_detected(client):
    rows = [kline_row("kbadisousdt", T0), kline_row("kbadisousdt", T0 + MIN, bad_iso=True)]
    write_csv("klines", "kbadisousdt", H_KLINES, rows)
    rep = validate(client, "kbadisousdt")
    assert checks_of(rep, "klines")["datetime_utc matches timestamp_ms"] == 1
    assert rep["verdict"] == "issues"


# ------------------------------------------------------------------- trades
def test_clean_trades_pass(client):
    rows = [trade_row("tokusdt", T0 + i * 1000, 100 + i, side="buy" if i % 2 else "sell")
            for i in range(6)]
    write_csv("trades", "tokusdt", H_TRADES, rows)
    rep = validate(client, "tokusdt")
    assert rep["verdict"] == "clean"


def test_duplicate_trade_id_detected(client):
    rows = [trade_row("tdupusdt", T0, 100), trade_row("tdupusdt", T0 + 1000, 100)]
    write_csv("trades", "tdupusdt", H_TRADES, rows)
    rep = validate(client, "tdupusdt")
    assert checks_of(rep, "trades")["unique trade_id"] == 1


def test_out_of_order_trade_id_detected(client):
    rows = [trade_row("tooousdt", T0, 200), trade_row("tooousdt", T0 + 1000, 150)]
    write_csv("trades", "tooousdt", H_TRADES, rows)
    rep = validate(client, "tooousdt")
    assert checks_of(rep, "trades")["monotonic trade_id"] == 1


def test_uppercase_side_rejected(client):
    # The v2 schema lower-cased side; "Buy"/"SELL" must be flagged.
    rows = [trade_row("tsideusdt", T0, 1, side="Buy"), trade_row("tsideusdt", T0 + 1, 2, side="SELL")]
    write_csv("trades", "tsideusdt", H_TRADES, rows)
    rep = validate(client, "tsideusdt")
    assert checks_of(rep, "trades")["side in {buy, sell}"] == 2


def test_nonpositive_price_detected(client):
    rows = [trade_row("tpxusdt", T0, 1, px=0.0)]
    write_csv("trades", "tpxusdt", H_TRADES, rows)
    rep = validate(client, "tpxusdt")
    assert checks_of(rep, "trades")["price & qty > 0"] == 1


# ------------------------------------------------------- liquidations & REST
def test_liquidation_value_mismatch_detected(client):
    ok = prefix("liquidations", "lqusdt", T0) + ["sell", 100.0, 2.0, 200.0]
    bad = prefix("liquidations", "lqusdt", T0 + 1000) + ["buy", 100.0, 2.0, 350.0]
    write_csv("liquidations", "lqusdt", H_LIQ, [ok, bad])
    rep = validate(client, "lqusdt")
    assert checks_of(rep, "liquidations")["value = price x qty"] == 1


def test_open_interest_value_mismatch_detected(client):
    ok = prefix("open_interest", "oiusdt", T0) + [150.0, 1000.0, 150000.0]
    bad = prefix("open_interest", "oiusdt", T0 + MIN) + [150.0, 1000.0, 999999.0]
    write_csv("open_interest", "oiusdt", H_OI, [ok, bad])
    rep = validate(client, "oiusdt")
    assert checks_of(rep, "open_interest")["oi_value = OI x mark_price"] == 1


def test_absurd_funding_rate_detected(client):
    ok = prefix("funding_rates", "fundusdt", T0) + [0.0001, T0 + 8 * 3600 * 1000]
    bad = prefix("funding_rates", "fundusdt", T0 + MIN) + [3.5, T0 + 8 * 3600 * 1000]
    write_csv("funding_rates", "fundusdt", H_FUNDING, [ok, bad])
    rep = validate(client, "fundusdt")
    assert checks_of(rep, "funding_rates")["funding_rate finite & bounded (|r|<=3%)"] == 1


def _ls_row(sym, ts, rt_type, lr, sr):
    return prefix("long_short_ratio", sym, ts) + ["5m", rt_type, lr, sr, round(lr / sr, 4)]


def test_long_short_missing_ratio_type_detected(client):
    rows = [_ls_row("lsmissusdt", T0, "global", 0.66, 0.34),
            _ls_row("lsmissusdt", T0, "top_trader_account", 0.6, 0.4)]
    write_csv("long_short_ratio", "lsmissusdt", H_LS, rows)
    rep = validate(client, "lsmissusdt")
    assert checks_of(rep, "long_short_ratio")["all 3 ratio types present"] == 1


def test_long_short_sum_violation_detected(client):
    rows = [_ls_row("lssumusdt", T0, "global", 0.66, 0.34),
            _ls_row("lssumusdt", T0, "top_trader_account", 0.8, 0.4),  # sums to 1.2
            _ls_row("lssumusdt", T0, "top_trader_position", 1.4, 0.9)]  # sums to 2.3
    write_csv("long_short_ratio", "lssumusdt", H_LS, rows)
    rep = validate(client, "lssumusdt")
    # both top_trader_account (1.2) and top_trader_position (2.3) now violate
    assert checks_of(rep, "long_short_ratio")["long + short ~= 1"] == 2


def test_taker_ratio_mismatch_detected(client):
    ok = prefix("taker_buy_sell_ratio", "takusdt", T0) + ["5m", 60.0, 40.0, 1.5]
    bad = prefix("taker_buy_sell_ratio", "takusdt", T0 + MIN) + ["5m", 60.0, 40.0, 9.9]
    write_csv("taker_buy_sell_ratio", "takusdt", H_TAKER, [ok, bad])
    rep = validate(client, "takusdt")
    assert checks_of(rep, "taker_buy_sell_ratio")["ratio = buy / sell"] == 1


def test_unknown_symbol_is_404(client):
    r = client.get(f"/api/validate/nosuchusdt?date={DAY}")
    assert r.status_code == 404
