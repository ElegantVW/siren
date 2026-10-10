# Porting Siren — other machines

**Looking for testers.** Linux (Debian, Q4OS, others), Windows 10/11,
macOS. Open a [GitHub issue](https://github.com/ElegantVW/siren/issues)
or email evenweaker@disroot.org with subject `[siren test]`.

## Three tiers

1. **Vessel (this is the product)** — TUI, local files, queue, palettes.
   Radio/trove if `curl` exists. Needs **mpv** on PATH.
2. **HEOS remote (optional)** — TCP `:1255` on the **speaker LAN**.
   A laptop on a different network will not see the house speakers.
3. **House follow** — PipeWire `Siren_Master`, python encoder, systemd.
   Skip on other machines (empty mixer, quiet waves, follow prints a skip).

Hard dependency: **mpv**. Everything else fails closed.

## Install

Preferred: **build on the machine you test** (proves the port):

```bash
sudo apt install mpv rustc cargo   # Debian / Q4OS
brew install mpv rust              # macOS
git clone https://github.com/ElegantVW/siren.git
cd siren && ./build.sh install     # ~/.local/bin/siren
```

Unix with a prebuilt ELF:

```bash
chmod +x siren && mv siren ~/.local/bin/
apt install mpv
```

Windows: install [mpv](https://mpv.io), copy `siren.exe` next to it or
onto PATH. Use Windows Terminal. WSL fallback: run the Linux binary
inside WSL with Linux mpv.

Library default: `~/Music` (Windows: `%USERPROFILE%\Music`).
Config: `~/.config/siren/config.json`.

## Smoke list

Fill this in when you report:

- OS and version
- Terminal (Windows Terminal, Konsole, kitty, …)
- `mpv --version`
- File type you played (flac / mp3 / ogg / …)
- What you heard
- What broke

Steps:

1. Put one music file in Music.
2. Run `siren` (TUI) or `siren play <words>`.
3. Enter on the file. Hear it.
4. `T` cycles palettes. `q` quits.
5. `h` is help.

Q4OS: `apt install mpv`, copy the musl `siren` to `~/.local/bin`.

Windows native: `siren.exe` + mpv. If the named pipe IPC is ugly, WSL
+ the Linux binary is the fallback.

macOS: unix binary + `brew install mpv`. Same keys as Linux.

## How we test on the development box

No Windows QEMU here (`/dev/kvm` is missing). Three layers:

1. `cargo test --offline --test compat_play` — isolated `HOME`, `SIREN_AO=null`
   (mpv decodes, no analog, no Cave). Needs host `mpv`.
2. `scripts/compat-debian.sh` — Debian bookworm nspawn (Q4OS family),
   rustup + mpv, same tests. Builds **inside** the guest.
3. `scripts/compat-wine.sh` — `siren.exe` `--help` / `config get palette`
   under Wine. Play-under-Wine waits for a Windows mpv.

## What we do not need in a first report

House follow, Cave, NAT, minidlna. Those live on the development box.
Do not probe anyone else's host. No passwords, keys, or client names.
