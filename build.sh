#!/usr/bin/env bash
# build.sh — produce release siren; install into faeOS engine paths
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"
cargo build --release
BIN="$ROOT/target/release/siren"
echo "built: $BIN"
ls -la "$BIN"

if [[ "${1:-}" == "install" ]]; then
  LIB="$HOME/.local/lib/faeos"
  WRAP_SRC="$ROOT/scripts/siren"
  mkdir -p "$LIB" "$HOME/bin"
  cp -f "$BIN" "$LIB/siren"
  chmod +x "$LIB/siren"

  install_launcher() {
    local dest="$1"
    mkdir -p "$(dirname "$dest")"
    cp -f "$WRAP_SRC" "$dest"
    chmod +x "$dest"
  }

  install_launcher "$HOME/bin/siren"
  if [[ -d "$HOME/faeOS/bin" ]]; then
    echo "note: ~/faeOS/bin/siren is still the live python player — not overwritten."
    echo "  cutover happens at parity sign-off (launcher swap then)."
  fi

  if [[ -w /usr/local/bin ]] || sudo -n true 2>/dev/null; then
    if [[ -w /usr/local/bin ]]; then
      install_launcher /usr/local/bin/siren
      echo "launcher        → /usr/local/bin/siren (sudo-visible)"
    else
      sudo install -m 755 "$WRAP_SRC" /usr/local/bin/siren
      echo "launcher        → /usr/local/bin/siren (sudo-visible)"
    fi
  else
    echo "note: once for sudo PATH: sudo install -m 755 $WRAP_SRC /usr/local/bin/siren"
  fi

  echo "installed engine → $LIB/siren"
  echo "launcher        → $HOME/bin/siren"
fi
