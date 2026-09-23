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
| Scope | No trove/Internet-Archive in v1. Python `faeOS/bin/siren` is fallback until parity sign-off, then retired. |

## Iteration rule

Port slices in order: config → library/fuzzy → mpv IPC → queue/playlist →
CLI → two-box TUI → audio menu. Verify each against the Python build
before moving on. TUI layout: top = content (browser/queue/waves/audio),
bottom = focused view's menu (Option A); waves stays on top.
