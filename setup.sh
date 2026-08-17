#!/usr/bin/env bash
#
# Capture Desk: interactive setup for the Binance USDT-M futures collector + dashboard.
# Checks prerequisites, sizes storage, writes config.toml, builds, and launches.
# Prefer containers? See the Docker quickstart in README.md (`docker compose up`).
#
set -uo pipefail
cd "$(dirname "$0")"

BOLD=$'\033[1m'; DIM=$'\033[2m'; GRN=$'\033[32m'; YEL=$'\033[33m'; RED=$'\033[31m'; CYN=$'\033[36m'; RST=$'\033[0m'
ok(){   printf "  ${GRN}\xE2\x9C\x93${RST} %s\n" "$1"; }
warn(){ printf "  ${YEL}!${RST} %s\n" "$1"; }
err(){  printf "  ${RED}\xE2\x9C\x97${RST} %s\n" "$1"; }
hdr() { printf "\n${BOLD}%s${RST}\n" "$1"; }
# ask "prompt" "default"  -> prints prompt to the terminal, echoes the answer to stdout
ask(){
  local p="$1" d="${2:-}" a
  if [ -n "$d" ]; then printf "  %s [%s]: " "$p" "$d" >/dev/tty; else printf "  %s: " "$p" >/dev/tty; fi
  read -r a </dev/tty || true
  echo "${a:-$d}"
}
# ask_valid "prompt" "default" validator_fn "error message"
# Re-prompts until validator_fn accepts the answer. Without this a typo (a port
# of "abc", a key pasted over the wrong prompt) is written to config.toml or .env
# unchallenged and only surfaces later as an unrelated-looking startup failure.
ask_valid(){
  local p="$1" d="${2:-}" fn="$3" msg="$4" a
  while :; do
    a=$(ask "$p" "$d")
    if "$fn" "$a"; then echo "$a"; return 0; fi
    err "$msg" >/dev/tty
  done
}
is_choice_aw(){ case "$1" in a|A|w|W) return 0 ;; *) return 1 ;; esac; }
is_choice_dn(){ case "$1" in d|D|n|N) return 0 ;; *) return 1 ;; esac; }
is_speed(){    case "$1" in 100|250|500) return 0 ;; *) return 1 ;; esac; }
is_port(){     case "$1" in ''|*[!0-9]*) return 1 ;; *) [ "$1" -ge 1 ] && [ "$1" -le 65535 ] ;; esac; }
# Binance keys are long alphanumeric strings. Blank is valid (the key is optional);
# anything else must look like a key, which catches an answer typed at the wrong prompt.
is_api_key(){  [ -z "${1// /}" ] && return 0; case "$1" in *[!A-Za-z0-9]*) return 1 ;; esac; [ "${#1}" -ge 32 ]; }
# Symbols: comma-separated alphanumeric tickers, e.g. btcusdt,ethusdt
is_symbols(){  [ -z "${1// /}" ] && return 1; case "${1// /}" in *[!A-Za-z0-9,]*) return 1 ;; esac; return 0; }
# true when nothing is LISTENing on the given TCP port (assumes free if lsof absent)
port_free(){ ! lsof -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1; }

# Confirm the exchange is actually reachable. Binance is geo-restricted in some
# jurisdictions (notably the US), and a blocked user would otherwise install
# everything, watch both containers start, and then wonder why no data ever
# arrives. Fail here with an explanation instead.
check_exchange(){
  local url="https://fapi.binance.com/fapi/v1/ping" out code ms
  command -v curl >/dev/null 2>&1 || { warn "curl not found, skipping the reachability check"; return 0; }
  out=$(curl -s -o /dev/null -w '%{http_code} %{time_total}' --max-time 15 "$url" 2>/dev/null || echo "000 0")
  code=${out%% *}
  ms=$(awk -v t="${out##* }" 'BEGIN{printf "%d", t*1000}')
  case "$code" in
    200) ok "fapi.binance.com reachable (${ms} ms)"; return 0 ;;
    451) err "Binance returned HTTP 451: the API is geo-restricted from this network."
         printf "  ${DIM}Binance blocks several jurisdictions, including the US. A VPN placed outside\n"
         printf "  the restriction, or a host in a permitted region, is required.\n"
         printf "  Note that binance.us is a separate exchange with a different API and is NOT\n"
         printf "  supported by this collector.${RST}\n"; return 1 ;;
    000) err "Could not reach fapi.binance.com at all."
         printf "  ${DIM}No internet route, DNS failure, or an outbound firewall. The collector needs\n"
         printf "  HTTPS and WebSocket access to fapi.binance.com.${RST}\n"; return 1 ;;
    *)   warn "fapi.binance.com answered HTTP $code (expected 200); continuing, but capture may fail"
         return 0 ;;
  esac
}

PY=""            # native build only: the Python 3.10+ interpreter picked in step 1
CHECK_ONLY=no    # --check runs every preflight and exits without installing

case "${1:-}" in
  --check) CHECK_ONLY=yes ;;
  -h|--help)
    printf "\nUsage: ./setup.sh [--check]\n\n"
    printf "  (no args)  interactive install: prerequisites, storage, pairs, cadence, build, start\n"
    printf "  --check    run every preflight check and exit; installs nothing\n\n"
    exit 0 ;;
  "") ;;
  *) printf "\nUnknown option: %s (try --help)\n\n" "$1"; exit 1 ;;
esac

printf "\n${BOLD}${CYN}Capture Desk / collector setup${RST}\n"
printf "${DIM}Binance USDT-M futures capture + live health/quality dashboard${RST}\n"
[ "$CHECK_ONLY" = yes ] && printf "${DIM}Preflight only, nothing will be installed.${RST}\n"

# ---- 0. install path --------------------------------------------------------
# Docker needs no Rust/Node/Python on the host, so offer it first when it is
# usable. Both paths ask the same questions and write the same config.toml; they
# differ only in how the collector and dashboard are built and started.
hdr "0. Install path"
MODE=native
# `docker` and `docker compose` both answer perfectly well with no daemon behind
# them, so probe the daemon itself. Without this the script offers the container
# path, asks every question, and only fails at `compose up` several minutes later.
docker_state=missing
if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
  if docker info >/dev/null 2>&1; then docker_state=ready; else docker_state=daemon_down; fi
fi

case "$docker_state" in
  ready)
    if [ "$CHECK_ONLY" = yes ]; then
      MODE=docker; ok "Docker: $(docker --version 2>&1 | head -1) (daemon running)"
    else
      printf "  ${DIM}[d] Docker  : one command, no Rust/Node/Python toolchain needed (recommended)\n"
      printf "  [n] Native  : build from source with cargo, npm and python${RST}\n"
      case "$(ask_valid "Install path [d/n]?" "d" is_choice_dn "answer 'd' for Docker or 'n' for a native build")" in
        d|D) MODE=docker; ok "Docker: $(docker --version 2>&1 | head -1)" ;;
        *)   MODE=native; ok "Native build" ;;
      esac
    fi
    ;;
  daemon_down)
    warn "Docker is installed, but its daemon is not running"
    printf "  ${DIM}Start it and re-run this script to use the container path:\n"
    printf "    macOS/Windows  open Docker Desktop (or your runtime, e.g. 'colima start')\n"
    printf "    Linux          sudo systemctl start docker${RST}\n"
    if [ "$CHECK_ONLY" != yes ] && [ "$(ask "Continue with the native build instead? [Y/n]" "Y")" = "n" ]; then
      echo; err "Start Docker, then re-run ./setup.sh"; exit 1
    fi
    ok "Native build will be used instead"
    ;;
  *)
    warn "Docker not found, using the native build"
    printf "  ${DIM}(install Docker Desktop for the one-command path: https://docs.docker.com/get-docker/)${RST}\n"
    ;;
esac

# ---- 1. prerequisites -------------------------------------------------------
hdr "1. Prerequisites"
miss=0
need(){  # need cmd "label" "install hint"
  if command -v "$1" >/dev/null 2>&1; then ok "$2: $("$1" --version 2>&1 | head -1)"
  else err "$2 missing. Install: $3"; miss=1; fi
}
if [ "$MODE" = docker ]; then
  ok "Docker builds the collector and dashboard in containers, nothing else to install"
else
  need cargo "Rust / cargo" "install from https://rustup.rs"
  # The dashboard backend needs Python 3.10+ (PEP-604 unions). macOS ships 3.9 as
  # the default python3, so pick the newest 3.10+ interpreter available.
  for c in python3 python3.13 python3.12 python3.11 python3.10; do
    command -v "$c" >/dev/null 2>&1 || continue
    "$c" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)' 2>/dev/null \
      && { PY="$c"; break; }
  done
  if [ -n "$PY" ]; then ok "Python: $("$PY" --version 2>&1) [$PY]"
  else err "Python 3.10+ required for the dashboard (found $(python3 --version 2>&1 || echo none)). Try: brew install python@3.12"; miss=1; fi
  need node "Node.js" "install Node 24 LTS or newer from https://nodejs.org"
  need npm  "npm"      "ships with Node.js"
  # npm 10 (node 20/22) false-positives on this lockfile's optional peer deps
  # and refuses `npm ci`, so require npm 11+ up front instead of failing mid-build.
  if command -v npm >/dev/null 2>&1; then
    npm_major=$(npm --version 2>/dev/null | cut -d. -f1)
    if [ -n "$npm_major" ] && [ "$npm_major" -lt 11 ]; then
      err "npm $(npm --version) found, but npm 11+ is required (Node 24 LTS ships it)"; miss=1
    fi
  fi
fi
[ "$miss" = 1 ] && { echo; err "Install the tools above, then re-run ./setup.sh"; exit 1; }

# ---- 2. exchange reachability ----------------------------------------------
hdr "2. Exchange reachability"
if ! check_exchange; then
  echo; err "Binance is not reachable from this machine, so capture cannot work here."
  exit 1
fi

# ---- 3. pairs ---------------------------------------------------------------
hdr "3. Pairs"
RAW_SYMS=""
if [ "$CHECK_ONLY" = yes ]; then
  ok "checking against the default profile: all pairs"
else
  printf "  ${DIM}[a] all tradeable USDT-M perps (~500; new listings captured after a restart)   [w] a whitelist${RST}\n"
  case "$(ask_valid "Collect [a]ll or [w]hitelist?" "a" is_choice_aw "answer 'a' for all pairs or 'w' for a whitelist")" in
    w|W) RAW_SYMS=$(ask_valid "Symbols (comma-separated, e.g. btcusdt,ethusdt,solusdt)" \
           "btcusdt,ethusdt,solusdt" is_symbols "comma-separated tickers only, e.g. btcusdt,ethusdt") ;;
  esac
fi

# ---- 4. order-book update speed --------------------------------------------
hdr "4. Order-book update speed"
if [ "$CHECK_ONLY" = yes ]; then
  SPEED=500; ok "checking against the default cadence: ${SPEED}ms"
else
  printf "  ${DIM}100ms = max detail/storage   250ms   500ms = recommended (full microstructure, ~80%% less storage)${RST}\n"
  SPEED=$(ask_valid "Update speed ms (100/250/500)" "500" is_speed "choose 100, 250 or 500")
fi

# ---- 5. data storage + disk preflight ---------------------------------------
# Asked AFTER pairs and cadence, because GB/day depends on both. A fixed estimate
# is worse than none: it tells someone capturing every pair at 100ms that they
# have six days of headroom when they have barely one.
hdr "5. Data storage"

# ~40 GB/day is the measured figure for the full universe at 500ms. Depth
# dominates the volume, so halving the interval roughly doubles it, and 100ms is
# about five times 500ms.
case "$SPEED" in
  100) speed_factor=5 ;;
  250) speed_factor=2 ;;
  *)   speed_factor=1 ;;
esac
if [ -n "${RAW_SYMS// /}" ]; then
  n_syms=$(printf '%s' "$RAW_SYMS" | tr ',' '\n' | grep -c '[a-zA-Z]')
  est_gb_day=$(( 40 * speed_factor * n_syms / 520 ))
  [ "$est_gb_day" -lt 1 ] && est_gb_day=1
  scope="$n_syms pair(s) at ${SPEED}ms"
else
  est_gb_day=$(( 40 * speed_factor ))
  scope="all ~520 pairs at ${SPEED}ms"
fi
printf "  ${DIM}At %s this writes roughly %s GB/day.${RST}\n" "$scope" "$est_gb_day"

if [ "$CHECK_ONLY" = yes ]; then
  DATA_DIR="$(pwd)"
else
  DATA_DIR=$(ask "Data directory" "$(pwd)/data")
  mkdir -p "$DATA_DIR" 2>/dev/null || { err "cannot create $DATA_DIR"; exit 1; }
  DATA_DIR=$(cd "$DATA_DIR" && pwd)
fi
free_gb=$(df -Pk "$DATA_DIR" | awk 'NR==2{print int($4/1048576)}')
days=$(( free_gb / est_gb_day ))
if [ "$days" -lt 2 ]; then
  err "${free_gb} GB free is under 2 days at this rate; use a bigger disk, a whitelist, or a slower cadence"
elif [ "$days" -lt 7 ]; then
  warn "${free_gb} GB free is about ${days} days at this rate"
else
  ok "${free_gb} GB free (about ${days} days at this rate)"
fi

# --check stops here. Everything past this point writes files, builds and starts
# services, which --check must never do.
if [ "$CHECK_ONLY" = yes ]; then
  printf "\n${GRN}${BOLD}All checks passed.${RST}  Run ${CYN}./setup.sh${RST} to install.\n\n"
  exit 0
fi

# ---- 5. optional read-only API key -----------------------------------------
hdr "6. Binance API key (optional)"
printf "  ${DIM}NOT required: the collector reads only PUBLIC market data. Provide one only to\n"
printf "  associate requests with your account, and if you do it MUST be READ-ONLY:\n"
printf "  enable Reading only, DISABLE trading & withdrawals, and IP-restrict it.${RST}\n"
API_KEY=$(ask_valid "Read-only API key (blank to skip)" "" is_api_key "that does not look like a Binance API key (32+ alphanumeric chars); leave blank to skip")

# ---- 6. write config.toml ---------------------------------------------------
# Pure shell on purpose: the Docker path installs no Python, so templating the
# config must not depend on one.
hdr "7. Writing config.toml"
[ -f config.toml ] && { cp config.toml "config.toml.bak.$(date +%s)"; warn "existing config.toml backed up"; }
cp config.example.toml config.toml

# Under Docker the host directory is bind-mounted at /app/data (DATA_PATH in
# .env), so base_dir must stay container-relative. Rewriting it to the host path
# would point the collector outside the container and capture nothing.
if [ "$MODE" = native ]; then
  sed -i.tmp -e "s|^base_dir = \"./data/futures\"|base_dir = \"$DATA_DIR/futures\"|" \
             -e "s|^base_dir = \"./data/gaps\"|base_dir = \"$DATA_DIR/gaps\"|" config.toml
fi
sed -i.tmp -e "s|^update_speed_ms = 500|update_speed_ms = $SPEED|" config.toml
if [ -n "${RAW_SYMS// /}" ]; then
  arr=$(printf '%s' "$RAW_SYMS" | tr 'A-Z' 'a-z' | tr ',' '\n' \
        | sed -e 's/^ *//' -e 's/ *$//' -e '/^$/d' -e 's/^/"/' -e 's/$/"/' \
        | paste -sd, - | sed -e 's/,/, /g')
  sed -i.tmp -e "s|^symbols = \[\]|symbols = [$arr]|" config.toml
fi
if [ -n "${API_KEY// /}" ]; then
  sed -i.tmp -e "s|^# api_key = \"\"|api_key = \"$API_KEY\"|" config.toml
fi
rm -f config.toml.tmp
[ -s config.toml ] && ok "config.toml written (data -> $DATA_DIR)" || { err "failed to write config.toml"; exit 1; }

# ---- 7/8. Docker path -------------------------------------------------------
if [ "$MODE" = docker ]; then
  hdr "8. Build and start (Docker)"
  DASH_PORT=$(ask_valid "Dashboard port" "3010" is_port "enter a port number between 1 and 65535")
  if ! port_free "$DASH_PORT"; then
    warn "port $DASH_PORT is already in use, finding a free one"
    for p in 3011 3020 8010 8080 8500; do port_free "$p" && { DASH_PORT="$p"; break; }; done
    ok "using port $DASH_PORT instead"
  fi
  # compose reads DATA_PATH (host dir bind-mounted at /app/data) and
  # DASHBOARD_PORT from .env; see docker-compose.yml.
  { echo "DATA_PATH=$DATA_DIR"; echo "DASHBOARD_PORT=$DASH_PORT"; } > .env
  ok ".env written (DATA_PATH=$DATA_DIR, DASHBOARD_PORT=$DASH_PORT)"
  if [ "$(ask "Build images and start now (~3-5 min first time)? [Y/n]" "Y")" != "n" ]; then
    printf "  ${DIM}docker compose up -d --build ...${RST}\n"
    if docker compose up -d --build; then
      ok "collector + dashboard running"
      printf "\n${GRN}${BOLD}Ready.${RST}  Dashboard -> ${CYN}http://localhost:%s${RST}\n" "$DASH_PORT"
      printf "  ${DIM}logs:${RST} docker compose logs -f collector    ${DIM}stop:${RST} docker compose down\n"
    else
      err "docker compose failed; see the output above"
      exit 1
    fi
  else
    printf "\n${BOLD}To start later:${RST}\n  ${DIM}docker compose up -d --build${RST}\n"
  fi
  printf "\n${DIM}Tune pairs & cadence live from the dashboard's Config tab. Data -> %s${RST}\n\n" "$DATA_DIR"
  exit 0
fi

# ---- 7. build (native) ------------------------------------------------------
hdr "8. Build"
if [ "$(ask "Build now: collector + dashboard (~2-3 min)? [Y/n]" "Y")" != "n" ]; then
  printf "  ${DIM}building collector (cargo build --release)...${RST}\n"
  if cargo build --release; then ok "collector built"; else err "cargo build failed"; exit 1; fi
  printf "  ${DIM}installing dashboard backend (venv, %s)...${RST}\n" "$PY"
  "$PY" -m venv dashboard/.venv \
    && dashboard/.venv/bin/pip install -q --upgrade pip \
    && dashboard/.venv/bin/pip install -q -r dashboard/backend/requirements.txt \
    && ok "dashboard backend ready" || { err "backend install failed"; exit 1; }
  printf "  ${DIM}building dashboard UI (npm)...${RST}\n"
  ( cd dashboard/frontend && npm ci --silent && npm run build >/dev/null 2>&1 ) \
    && ok "dashboard UI built" || { err "UI build failed"; exit 1; }
  BUILT=1
else BUILT=0; fi

# ---- 8. launch --------------------------------------------------------------
hdr "9. Launch"
DASH_PORT=$(ask_valid "Dashboard port" "8000" is_port "enter a port number between 1 and 65535")
if ! port_free "$DASH_PORT"; then
  warn "port $DASH_PORT is already in use, finding a free one"
  for p in 8010 8080 8500 3010 8888; do port_free "$p" && { DASH_PORT="$p"; break; }; done
  ok "using port $DASH_PORT instead"
fi
START_CMD_C="bash scripts/supervise.sh"
# DATA_DIR and LOG_DIR must be handed to the dashboard explicitly. The collector
# learns the data directory from config.toml, but the dashboard only reads the
# environment, and its fallback is <repo>/data. Launch it without these and any
# non-default data directory leaves the dashboard reading an empty tree: it shows
# a live heartbeat and zero symbols, with nothing to explain the contradiction.
START_CMD_D="cd dashboard/backend && DATA_DIR='$DATA_DIR' LOG_DIR='$(pwd)/logs' SERVE_STATIC=1 ../.venv/bin/python -m uvicorn server:app --host 127.0.0.1 --port $DASH_PORT"
if [ "$BUILT" = 1 ] && [ "$(ask "Start collector + dashboard now? [Y/n]" "Y")" != "n" ]; then
  mkdir -p logs
  LOG_DIR_ABS="$(pwd)/logs"
  nohup bash scripts/supervise.sh >logs/supervise.log 2>&1 </dev/null &
  ok "collector started under supervisor (logs/supervise.log)"
  ( cd dashboard/backend && DATA_DIR="$DATA_DIR" LOG_DIR="$LOG_DIR_ABS" SERVE_STATIC=1 \
      nohup ../.venv/bin/python -m uvicorn server:app \
      --host 127.0.0.1 --port "$DASH_PORT" >../../logs/dashboard.log 2>&1 </dev/null & )
  sleep 3
  if port_free "$DASH_PORT"; then
    err "dashboard failed to bind :$DASH_PORT; see logs/dashboard.log"
    printf "  ${DIM}start it by hand on another port:${RST}  %s\n" "$START_CMD_D"
  else
    ok "dashboard started (logs/dashboard.log)"
    printf "\n${GRN}${BOLD}Ready.${RST}  Dashboard -> ${CYN}http://localhost:%s${RST}\n" "$DASH_PORT"
  fi
else
  printf "\n${BOLD}To start later:${RST}\n"
  printf "  ${DIM}collector:${RST}  %s\n" "$START_CMD_C"
  printf "  ${DIM}dashboard:${RST}  %s\n" "$START_CMD_D"
fi
printf "\n${DIM}Tune pairs & cadence live from the dashboard's Config tab. Data -> %s${RST}\n\n" "$DATA_DIR"
