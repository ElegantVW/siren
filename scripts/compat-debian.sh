#!/usr/bin/env bash
# Q4OS stand-in: Debian bookworm in nspawn, rustup + mpv, cargo test + CLI smoke.
# Does NOT copy the Arch glibc binary in. Needs sudo (password on stdin if you pipe it).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ROOTFS="${SIREN_COMPAT_ROOT:-$HOME/siren-compat/debian}"
SUITE="${SIREN_COMPAT_SUITE:-bookworm}"

if ! command -v debootstrap >/dev/null 2>&1; then
  echo "need debootstrap: sudo pacman -S debootstrap" >&2
  exit 2
fi
if ! command -v systemd-nspawn >/dev/null 2>&1; then
  echo "need systemd-nspawn" >&2
  exit 2
fi

if [[ ! -f "$ROOTFS/etc/os-release" ]]; then
  echo "bootstrapping $SUITE → $ROOTFS"
  sudo debootstrap --variant=minbase "$SUITE" "$ROOTFS" http://deb.debian.org/debian
fi

sudo systemd-nspawn -D "$ROOTFS" --bind="$ROOT:/siren:norbind" /bin/bash -s <<'INSIDE'
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq mpv curl ca-certificates gcc pkg-config
if ! command -v rustup >/dev/null 2>&1 && [[ ! -x /root/.cargo/bin/cargo ]]; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
fi
# shellcheck disable=SC1091
source /root/.cargo/env
cd /siren
cargo test --offline --test compat_play -- --nocapture
INSIDE
echo "debian nspawn smoke ok"
