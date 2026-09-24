# AGENTS.md — Siren

Canonical repo: `ElegantVW/siren` → `~/siren`.
faeOS keeps only a thin launcher after cutover; do **not** vendor this tree back into `faeos/`.

## North star (locked)

Personal music vessel that **does not lie about what's playing** and
**never blasts the speaker**: one remote (`siren`) for local mpv and the
HEOS fleet. Small steps, evidence each iteration (live speaker tests at
low volume).

## Product laws

| Law | Rule |
|-----|------|
| Voice | faeOS cli-voice: all-ages, "music vessel" tone. No superlatives in output. |
| Engine | Rust. Playback stays mpv-over-IPC (`/tmp/siren-mpv.sock`) — never reimplement decode. |
| Speaker | HEOS CLI on 1255 + DLNA only. Speakers are not PipeWire sinks. |
| Safety | Volume changes print the new level. Test casts at ≤20%. `add_to_queue` cids keep raw `$` (never %-encode). |
| Secrets | None. LAN-only, no tokens. |
| State | `~/.config/siren/config.json` (library, output, speaker). DLNA server stays external (minidlna). |
| Scope | Trove, tags, dir-browser, radio landed. One output channel (`output.rs`): sources stage queue items, output routes local mpv / speaker DLNA / speaker stream. Python `faeOS/bin/siren` is archived (tests/rollback only). |

## Iteration rule

Engine is Rust. TUI layout: top = content (browser/queue/audio/trove),
bottom = focused view's menu (Option A); waves strip stays visible.
