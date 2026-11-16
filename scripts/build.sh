#!/usr/bin/env bash
# Builds everything for this machine: the web UI, the `pulse` binary with the UI embedded, and the
# desktop app (Linux: AppImage; macOS: dmg), from the repository root:
#
#   ./scripts/build.sh            all three
#   ./scripts/build.sh --no-app   the web UI and the binary only (no Electron download)
#
# Needs: a Rust toolchain (rustup, stable) with a C compiler, Node >= 22 and pnpm (the version in
# package.json's packageManager; `corepack enable` provides it). The headless binary alone needs
# only `cargo build --release -p pulse-cli`, without Node; it then serves a placeholder page.
#
# Outputs: target/release/pulse and apps/desktop/release/.
set -euo pipefail

cd "$(dirname "$0")/.."
app=1
for arg in "$@"; do
  case "$arg" in
    --no-app) app=0 ;;
    -h | --help)
      sed -n '2,13p' "$0"
      exit 0
      ;;
    *)
      echo "unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

step() { printf '\n==> %s\n' "$*"; }

step "JavaScript dependencies"
pnpm install --frozen-lockfile

step "Web UI (apps/web)"
pnpm --dir apps/web build

step "pulse binary (release, UI embedded)"
# rust-embed reads apps/web/dist when pulse-daemon compiles, but cargo does not notice files
# appearing there, so a stale binary could keep an older UI (or the placeholder). Recompile the
# crate that embeds it; the rest of the build stays cached.
cargo clean --release -p pulse-daemon
cargo build --release --locked -p pulse-cli

if [ "$app" = 1 ]; then
  step "Desktop app (apps/desktop)"
  pnpm --dir apps/desktop run package
fi

step "Done"
ls -lh target/release/pulse
if [ "$app" = 1 ]; then
  find apps/desktop/release -maxdepth 1 -type f \( -name '*.AppImage' -o -name '*.dmg' \) -exec ls -lh {} +
fi
