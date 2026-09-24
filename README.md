# Siren — the Aether's music vessel

faeOS first-party media player. mpv-backed local playback plus HEOS
network-speaker control, in one remote.

**Status:** default engine (v0.1.0, cut over 2026-09-23). Ported: config,
library/fuzzy, mpv IPC, queue/playlist, transport CLI, two-box TUI,
audio menu + `cast`, trove (CLI + TUI), tags (`ffprobe`), directory
browser — all live-verified. `~/bin/siren` is the thin launcher.
Python `faeOS/bin/siren` is archived (tests/rollback only).

## Layout

- `src/` — rust engine (`cargo build --release`)
- `scripts/siren` — thin launcher (never a prebuilt ELF in git)
- `build.sh install` — engine → `~/.local/lib/faeos/siren`, launcher → `~/bin/siren`

## TUI (target)

Two boxes + a persistent waves strip. Top = content view (Tab cycles
browser → queue → audio → trove). Bottom = context menu of the focused
view (Option A):

| Focus   | Bottom menu                          |
|---------|--------------------------------------|
| browser | open/play · add · cast · backspace up · / filter |
| queue   | play-from · remove · clear            |
| audio   | output local\|heos · speaker · vol · test-cast |
| trove   | search · pick version · download      |

## Audio (target)

Siren owns its audio routing (`~/.config/siren/config.json`):

```
siren audio                  # output, speaker, volumes, DLNA status
siren audio output heos      # route play/pause/next/now to the speaker
siren audio output local     # back to mpv (default)
siren cast <query>           # top library match → Vanguarda Office (DLNA)
siren audio queue              # speaker queue: qid · song — artist
siren audio queue play 2       # play / rm / clear / move 1 3
siren cast QUERY --next        # queue play-next (aid=2)
siren cast QUERY --append      # add to end (aid=3)
siren queue add QUERY          # local queue persists (~/.config/siren/queue.json)
```

Default speaker: **Vanguarda Office** (HEOS 1, `192.168.8.184`), `--speaker`
for Cave. Protocol: HEOS CLI on TCP/1255 (newline `heos://` URIs) +
`VANGUARDA-DLNA` (minidlna on `:8200`); queue via `add_to_queue aid=1`
with raw `$` in cid/mid.

## Trove

Free & legal music (Internet Archive), zero new Rust deps (system `curl`):

```
siren trove 10 music lofi   # interactive search + pick
siren trove get <identifier>
siren trove about
```

TUI: 5th Tab stop (`trove`), `s` search · `enter` pick version · `1-9` dl ·
`a` all · `f` session format · mouse wheel + click select, double-click
acts. Search, metadata and downloads run in background threads.
Guards: `.part` resume, skip-if-exists, 40-file cap, 32MB confirm,
`TROVE_MAX_TOTAL`. Lands in `~/Music/trove/<identifier>`.

## Build

```bash
./build.sh          # debug-check: cargo build --release
./build.sh install  # engine + launchers
```
