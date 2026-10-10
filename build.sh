#!/usr/bin/env bash
# build.sh — release binary. install → ~/.local/bin. install --house → faeOS paths + follow unit.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"
cargo build --release
BIN="$ROOT/target/release/siren"
echo "built: $BIN"
ls -la "$BIN"

if [[ "${1:-}" != "install" ]]; then
  exit 0
fi
shift || true
HOUSE=0
if [[ "${1:-}" == "--house" ]]; then
  HOUSE=1
fi

if ! command -v mpv >/dev/null 2>&1; then
  echo "siren: WARNING — mpv not found; local playback will fail." >&2
  echo "siren:   linux:  sudo pacman -S mpv   |  sudo apt install mpv" >&2
  echo "siren:   mac:    brew install mpv" >&2
  echo "siren:   windows: https://mpv.io" >&2
fi

if [[ "$HOUSE" -eq 1 ]]; then
  LIB="$HOME/.local/lib/faeos"
  WRAP_SRC="$ROOT/scripts/siren"
  mkdir -p "$LIB" "$HOME/bin"
  cp -f "$BIN" "$LIB/siren"
  chmod +x "$LIB/siren"
  if [[ -f "$ROOT/scripts/siren-stream" ]]; then
    cp -f "$ROOT/scripts/siren-stream" "$LIB/siren-stream"
    chmod +x "$LIB/siren-stream"
  fi

  install_launcher() {
    local dest="$1"
    mkdir -p "$(dirname "$dest")"
    cp -f "$WRAP_SRC" "$dest"
    chmod +x "$dest"
  }

  install_launcher "$HOME/bin/siren"

  UNIT_SRC="$ROOT/systemd/siren-follow.service"
  UNIT_DST="$HOME/.config/systemd/user/siren-follow.service"
  if [[ -f "$UNIT_SRC" ]]; then
    mkdir -p "$HOME/.config/systemd/user"
    cp -f "$UNIT_SRC" "$UNIT_DST"
    systemctl --user daemon-reload
    systemctl --user enable siren-follow.service
    if systemctl --user restart siren-follow.service; then
      echo "user unit       → $UNIT_DST (enabled, started)"
    else
      echo "user unit       → $UNIT_DST (enabled; start failed — speakers off?)"
    fi
  fi

  echo "installed engine → $LIB/siren"
  echo "launcher        → $HOME/bin/siren"
else
  DEST="$HOME/.local/bin/siren"
  mkdir -p "$(dirname "$DEST")"
  cp -f "$BIN" "$DEST"
  chmod +x "$DEST"
  echo "installed → $DEST"
  echo "put ~/.local/bin on PATH. mpv is the only hard dependency."
  echo "house follow unit: ./build.sh install --house"
fi
