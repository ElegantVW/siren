#!/usr/bin/env bash
# Cross-compile Windows GNU and smoke CLI under Wine. No Windows mpv this pass
# (play stays Linux/nspawn). Does not replace Arch rust — uses ~/.rustup if present.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="$ROOT/dist"
mkdir -p "$DIST"

if ! command -v wine >/dev/null 2>&1; then
  echo "need wine" >&2
  exit 2
fi

if [[ -x "$HOME/.cargo/bin/rustup" ]]; then
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env" 2>/dev/null || true
  export PATH="$HOME/.cargo/bin:$PATH"
fi

if ! rustup target list --installed 2>/dev/null | grep -q 'x86_64-pc-windows-gnu'; then
  echo "need: rustup target add x86_64-pc-windows-gnu" >&2
  echo "need: sudo pacman -S mingw-w64-gcc" >&2
  exit 2
fi
if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
  echo "need mingw-w64-gcc" >&2
  exit 2
fi

cd "$ROOT"
cargo build --release --target x86_64-pc-windows-gnu
cp -f "$ROOT/target/x86_64-pc-windows-gnu/release/siren.exe" "$DIST/siren.exe"

export WINEDEBUG=-all
wine "$DIST/siren.exe" --help | head
wine "$DIST/siren.exe" config get palette | grep -q pixie
echo "wine CLI smoke ok"
