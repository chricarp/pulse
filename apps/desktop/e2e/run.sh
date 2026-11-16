#!/usr/bin/env bash
# Runs the end-to-end scenarios of e2e/harness.ts under Xvfb, each against a fresh temporary
# Pulse home, and checks the daemon from outside the app afterwards.
#
#   e2e/run.sh OUT_DIR [scenario...]     (default: all)
#
# Environment: PULSE_BIN (default ../../target/release/pulse), E2E_LD_LIBRARY_PATH (extra
# libraries Electron needs on a machine without GTK), E2E_ELECTRON_FLAGS (for example
# --no-sandbox where the Chromium sandbox cannot start). Never touches ~/.pulse.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
out="$(mkdir -p "$1" && cd "$1" && pwd)"
shift
scenarios=("$@")
[ ${#scenarios[@]} -eq 0 ] && scenarios=(main adopt stale older gnome)
bin="${PULSE_BIN:-$here/../../target/release/pulse}"
electron="$here/node_modules/electron/dist/electron"
node "$here/scripts/build.mjs" --e2e

config() {
  cat <<'EOF'
[daemon]
port = 0

[limits]
circuit_breaker = { max_fires = 5, window = "1h", open_for = "1h" }

[[agent]]
name = "ok"
adapter = "command"
command = ["true"]
allow = ["ext.*"]
max_queued = 100

[[agent]]
name = "broken"
adapter = "command"
command = ["sh", "-c", "exit 3"]
allow = ["ext.*"]

[[rule]]
name = "hot"
on = "ext.test.hot"
run = { agent = "ok" }

[[rule]]
name = "fails"
on = "ext.test.fail"
run = { agent = "broken" }

[[rule]]
name = "needs-approval"
on = "ext.test.approve"
supervised = true
run = { agent = "ok" }
EOF
}

# Stops a daemon this script (or the app it ran) started, by asking it, and checks its pid is gone.
stop_daemon() {
  local home=$1 pid
  pid=$(sed -n 's/.*"pid": *\([0-9]*\).*/\1/p' "$home/daemon.json" 2>/dev/null || true)
  "$bin" --home "$home" stop >/dev/null 2>&1 || true
  # `pulse stop` returns when the lock is released, a moment before the process is gone.
  if [ -n "$pid" ]; then timeout 10 tail --pid="$pid" -f /dev/null || true; fi
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then echo "daemon $pid still running" >&2; return 1; fi
  echo "${pid:-none}"
}

run_app() {
  local name=$1 home=$2 desktop=$3
  local log="$out/$name.log"
  env PULSE_E2E="$name" PULSE_E2E_OUT="$out" PULSE_E2E_ANSWER="${ANSWER:-1}" PULSE_HOME="$home" PULSE_BIN="$bin" \
    XDG_CONFIG_HOME="$out/xdg-$name" XDG_CURRENT_DESKTOP="$desktop" \
    LD_LIBRARY_PATH="${E2E_LD_LIBRARY_PATH:-}" ELECTRON_ENABLE_LOGGING=0 \
    xvfb-run -a -s "-screen 0 1440x900x24" "$electron" ${E2E_ELECTRON_FLAGS:-} "$here/dist-e2e/harness.cjs" \
    >"$log" 2>&1 &
  local pid=$!
  echo "$name: xvfb-run pid $pid" >&2
  local code=0
  wait "$pid" || code=$?
  if kill -0 "$pid" 2>/dev/null; then echo "$name: xvfb-run $pid still alive" >&2; fi
  echo "$code"
}

summary="$out/summary.txt"
: >"$summary"
for s in "${scenarios[@]}"; do
  home=$(mktemp -d "$out/home-$s-XXXX")
  config >"$home/pulse.toml"
  case "$s" in
    main)
      code=$(run_app main "$home" KDE)
      st=$("$bin" --home "$home" --json status >/dev/null 2>&1 && echo running || echo stopped)
      echo "main: app exit $code; daemon after plain quit: $st" | tee -a "$summary"
      # The daemon left running is adopted by the next scenario.
      code=$(run_app adopt "$home" KDE)
      st=$("$bin" --home "$home" --json status >/dev/null 2>&1 && echo running || echo stopped)
      echo "adopt: app exit $code; daemon after quit-and-stop: $st" | tee -a "$summary"
      echo "main/adopt: stopped daemon $(stop_daemon "$home")" | tee -a "$summary"
      ;;
    adopt) ;; # runs after main
    stale)
      # A daemon.json left by a daemon that died: nothing listens on its port.
      printf '{"pid": 999999, "port": 9, "version": "0.1.0", "started_at": "2026-01-01T00:00:00Z"}' >"$home/daemon.json"
      code=$(run_app stale "$home" KDE)
      st=$("$bin" --home "$home" --json status >/dev/null 2>&1 && echo running || echo stopped)
      echo "stale: app exit $code; daemon after quit-and-stop: $st" | tee -a "$summary"
      echo "stale: stopped daemon $(stop_daemon "$home")" | tee -a "$summary"
      ;;
    older)
      "$bin" --home "$home" --json start --detach >"$out/older-start.json"
      # Pretend the running daemon is from an older release.
      sed -i 's/"version": *"[^"]*"/"version": "0.0.1"/' "$home/daemon.json"
      code=$(ANSWER=0 run_app older "$home" KDE)
      st=$("$bin" --home "$home" --json status >/dev/null 2>&1 && echo running || echo stopped)
      echo "older: app exit $code; first daemon $(cat "$out/older-start.json"); daemon after quit-and-stop: $st" | tee -a "$summary"
      echo "older: stopped daemon $(stop_daemon "$home")" | tee -a "$summary"
      ;;
    gnome)
      "$bin" --home "$home" --json start --detach >"$out/gnome-start.json"
      code=$(run_app gnome "$home" GNOME)
      st=$("$bin" --home "$home" --json status >/dev/null 2>&1 && echo running || echo stopped)
      echo "gnome: app exit $code after closing its window; daemon: $st" | tee -a "$summary"
      echo "gnome: stopped daemon $(stop_daemon "$home")" | tee -a "$summary"
      ;;
  esac
done
