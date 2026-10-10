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
  # DEPS: mpv (local playback over IPC). HEOS/minidlna only for casting.
  if ! command -v mpv >/dev/null 2>&1; then
    echo "siren: WARNING — mpv not found; local playback will fail." >&2
    echo "siren:   next:  sudo pacman -S mpv" >&2
  fi
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

  UNIT_SRC="$ROOT/systemd/siren-follow.service"
  UNIT_DST="$HOME/.config/systemd/user/siren-follow.service"
  if [[ -f "$UNIT_SRC" ]]; then
    mkdir -p "$HOME/.config/systemd/user"
    cp -f "$UNIT_SRC" "$UNIT_DST"
    systemctl --user daemon-reload
    systemctl --user enable siren-follow.service
    # restart re-runs ExecStart; start is a no-op if RemainAfterExit is already active
    if systemctl --user restart siren-follow.service; then
      echo "user unit       → $UNIT_DST (enabled, started)"
    else
      echo "user unit       → $UNIT_DST (enabled; start failed — speakers off?)"
    fi
  fi

  echo "installed engine → $LIB/siren"
  echo "launcher        → $HOME/bin/siren"
fi
