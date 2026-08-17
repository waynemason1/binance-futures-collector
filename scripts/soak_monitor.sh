#!/usr/bin/env bash
# Soak sampler. 60 s samples of the collector's health -> CSV, and at each UTC
# midnight a day-file handoff integrity check (order-book update-id chain +
# trade-tape gap across the boundary). This is the exact instrument behind
# docs/SOAK_TEST.md.
#
# Usage:  soak_monitor.sh [hours] [out_csv] [out_log]
#   hours    how long to run (default 48)
#   out_csv  sample output (default ./soak_monitor.csv)
#   out_log  integrity/event log (default ./soak_monitor.log)
#
# Durable by design: append-only outputs, safe to relaunch after a crash, and
# reads only stats.json + /proc + the data tree; it never touches the collector.
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SJ=$ROOT/data/stats/stats.json
HOURS=${1:-48}
OUT=${2:-$PWD/soak_monitor.csv}
LOG=${3:-$PWD/soak_monitor.log}
END=$(( $(date -u +%s) + HOURS * 3600 ))
SYMBOLS=${SOAK_SYMBOLS:-btcusdt ethusdt solusdt}

[ -f "$OUT" ] || echo "ts_utc,epoch,uptime_s,cpu_pct,cpu_cores,rss_mb,vmhwm_mb,fd,conns,reconnects,msgs_total,msg_per_s,errors,ob_live" > "$OUT"
log(){ echo "[$(date -u '+%F %T')] $*" >> "$LOG"; }
log "monitor START (pid $$); sampling 60s for ${HOURS}h"

sample(){
  local col vmhwm errs obl
  col=$(pgrep -f 'target/release/binance-futures-collector'|head -1)
  vmhwm=""; [ -n "$col" ] && vmhwm=$(awk '/VmHWM/{printf "%.1f",$2/1024}' /proc/$col/status 2>/dev/null)
  errs=$(grep -oE 'Errors: [0-9]+' "$ROOT/logs/collector_rCURRENT.log" 2>/dev/null | tail -1 | grep -oE '[0-9]+$')
  obl=$(grep -oE 'Orderbooks Live: [0-9]+/[0-9]+' "$ROOT/logs/collector_rCURRENT.log" 2>/dev/null | tail -1 | awk '{print $3}')
  python3 - "$SJ" "${vmhwm:-}" "${errs:-}" "${obl:-}" >> "$OUT" 2>/dev/null <<'PY'
import json,sys,datetime
sj,vmhwm,errs,obl=sys.argv[1:5]
try: d=json.load(open(sj))
except Exception: sys.exit(0)
now=datetime.datetime.now(datetime.timezone.utc)
cpu=d.get('cpu_pct')
row=[now.strftime('%F %T'),int(now.timestamp()),d.get('uptime_seconds',''),
     round(cpu,1) if cpu is not None else '', round(cpu/100,3) if cpu is not None else '',
     round(d.get('rss_mb',0),1), vmhwm, d.get('fd_count',''),
     d.get('active_connections',''), d.get('total_reconnects',''),
     d.get('messages_total',''), round(d.get('messages_per_sec') or 0,1), errs, obl]
print(','.join(str(x) for x in row))
PY
}

integrity(){  # $1 prev-day  $2 new-day
  local prev=$1 new=$2
  log "===== MIDNIGHT INTEGRITY  $prev 23:59 -> $new 00:00 ====="
  SOAK_SYMBOLS="$SYMBOLS" python3 - "$ROOT/data/futures" "$prev" "$new" >> "$LOG" 2>&1 <<'PY'
import csv,glob,os,sys
base,prev,new=sys.argv[1:4]
def last_row(f):
    r=None
    for r in csv.DictReader(open(f)): pass
    return r
def first_row(f):
    for r in csv.DictReader(open(f)): return r
for sym in os.environ.get("SOAK_SYMBOLS","btcusdt ethusdt solusdt").split():
    pd=sorted(glob.glob(f"{base}/orderbooks/{sym}/{prev}/*depth_updates*.csv"))
    nd=sorted(glob.glob(f"{base}/orderbooks/{sym}/{new}/*depth_updates*.csv"))
    if not pd or not nd:
        print(f"  {sym}: MISSING files (prev={len(pd)} new={len(nd)}); check pending"); continue
    lu=last_row(pd[-1]); fu=first_row(nd[0])
    try:
        u=int(lu["last_update_id"]); pu=int(fu["prev_update_id"])
        ok = (pu==u)
        print(f"  {sym} orderbook: last u(prev)={u}  first pu(new)={pu}  CHAINED={'YES' if ok else 'NO (gap='+str(pu-u)+')'}")
    except Exception as e:
        print(f"  {sym} orderbook: parse error {e}")
    # trade tape gap across the boundary
    pt=sorted(glob.glob(f"{base}/trades/{sym}/{prev}/*.csv"))
    nt=sorted(glob.glob(f"{base}/trades/{sym}/{new}/*.csv"))
    if pt and nt:
        try:
            lt=int(last_row(pt[-1])["timestamp_ms"]); ft=int(first_row(nt[0])["timestamp_ms"])
            print(f"  {sym} trades: last(prev)={lt}  first(new)={ft}  gap={ft-lt} ms")
        except Exception as e:
            print(f"  {sym} trades: parse error {e}")
print("  (verdict per symbol above: CHAINED=YES means zero loss across that rotation)")
PY
  log "===== end integrity ====="
}

last_day=$(date -u +%F)
while [ "$(date -u +%s)" -lt "$END" ]; do
  sample
  today=$(date -u +%F)
  if [ "$today" != "$last_day" ]; then
    # Dense-sample the crater window instead of pausing here: the midnight
    # rotation frees memory within ~30 s, so a single sleep-then-integrity
    # leaves a ~2 min blind spot that can swallow the RSS trough. Sample every
    # 10 s across it so the floor is always captured regardless of loop phase;
    # the new day's first files have also landed by the time integrity runs.
    for _ in $(seq 1 12); do sleep 10; sample; done
    integrity "$last_day" "$today"
    last_day=$today
  fi
  sleep 60
done
log "monitor COMPLETE"
