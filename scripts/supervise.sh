#!/usr/bin/env bash
#
# Supervise the collector: keep it running, and honour restart requests dropped by
# the dashboard (a sentinel file it writes when the config changes). The dashboard
# never starts or stops the process itself; this script is the only thing that
# does, so the web UI needs no privilege to spawn or kill anything.
#
# Alternatives for production: a systemd unit with Restart=always, or Docker with
# a restart policy. Any of them turns "Restart to apply" in the dashboard into a
# clean, graceful reload.
#
#   ./scripts/supervise.sh
#
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${COLLECTOR_BIN:-$ROOT/target/release/binance-futures-collector}"
# The dashboard writes the restart sentinel under the CONFIGURED data directory,
# so this has to read the same config rather than assume ./data. Hardcoding it
# meant "Restart to apply" silently did nothing whenever the data directory was
# anywhere other than the default.
CONFIG="${CONFIG_PATH:-$ROOT/config.toml}"
data_root=""
if [ -f "$CONFIG" ]; then
  # first base_dir under [output] is "<data>/futures"; its parent is the data root
  base_dir=$(awk -F'"' '/^[[:space:]]*base_dir[[:space:]]*=/{print $2; exit}' "$CONFIG")
  [ -n "$base_dir" ] && data_root=$(dirname "$base_dir")
fi
case "$data_root" in
  "" ) data_root="$ROOT/data" ;;   # no config readable: fall back to the default
  /* ) ;;                          # already absolute
  *  ) data_root="$ROOT/${data_root#./}" ;;
esac
SENTINEL="${RESTART_SENTINEL:-$data_root/control/restart.request}"
# How long to wait for the collector's own drain (stop ingest, join the handler,
# flush every open CSV buffer, save the gap registry) before giving up on it.
STOP_TIMEOUT="${COLLECTOR_STOP_TIMEOUT:-120}"

cd "$ROOT"
mkdir -p "$(dirname "$SENTINEL")"

collector_pid=""
shutting_down=0

# Wait for the collector to exit on its own, up to STOP_TIMEOUT seconds.
await_exit() {
  local waited=0
  while kill -0 "$collector_pid" 2>/dev/null; do
    if [ "$waited" -ge "$STOP_TIMEOUT" ]; then
      echo "supervise: collector still running after ${STOP_TIMEOUT}s; sending SIGKILL"
      kill -KILL "$collector_pid" 2>/dev/null
      return
    fi
    sleep 1
    waited=$((waited + 1))
  done
}

# This script is PID 1 in the container. A shell running as PID 1 ignores signals
# that only carry a default action, so without this trap SIGTERM never reaches the
# collector: Docker would wait out its grace period and SIGKILL the container,
# discarding every buffered row instead of flushing it. Forward the signal and
# wait for the drain to finish.
on_term() {
  shutting_down=1
  if [ -n "$collector_pid" ] && kill -0 "$collector_pid" 2>/dev/null; then
    echo "supervise: signal received, stopping collector ($collector_pid) gracefully"
    kill -TERM "$collector_pid" 2>/dev/null
    await_exit
  fi
  echo "supervise: collector stopped, exiting"
  exit 0
}
trap on_term TERM INT

while true; do
  rm -f "$SENTINEL"
  "$BIN" &
  collector_pid=$!
  echo "supervise: collector started (pid $collector_pid)"

  while kill -0 "$collector_pid" 2>/dev/null; do
    [ "$shutting_down" = 1 ] && break
    if [ -f "$SENTINEL" ]; then
      echo "supervise: restart requested; stopping collector ($collector_pid) gracefully"
      kill -TERM "$collector_pid" 2>/dev/null
      await_exit
      break
    fi
    # Backgrounded sleep + wait, so a trap fires immediately rather than after the
    # current sleep finishes. A foreground sleep is not interruptible here.
    sleep 2 &
    wait $! 2>/dev/null
  done

  wait "$collector_pid" 2>/dev/null
  [ "$shutting_down" = 1 ] && exit 0
  echo "supervise: collector exited; relaunching in 2s"
  sleep 2
done
