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
| Speaker | HEOS CLI on 1255 + DLNA + UPnP AVTransport (60006) only. Speakers are not PipeWire sinks. **Group control**: `group/set_group?pid=leader,member` (comma-separated, leader first), `group/set_volume?gid=`, `group/set_mute?gid=`. HEOS syncs natively (1–2ms drift verified). **Live stream** (`stream.rs`): `Siren_Master` monitor → ffmpeg mp3 → `:8899/siren.mp3` → UPnP `SetAVTransportURI` on group leader. No third party in the loop. |
| Safety | Volume changes print the new level. Test casts at ≤20%. `add_to_queue` cids keep raw `$` (never %-encode). |
| Secrets | None. LAN-only, no tokens. |
| State | `~/.config/siren/config.json` (library, output, speaker). DLNA server stays external (minidlna). |
| Scope | Trove, tags, dir-browser, radio landed. **Group control** (`heos.rs`): `get_groups`, `set_group`, `ungroup`, `group_set_volume`, `group_set_mute`. **Live stream** (`stream.rs` + `W` key): encoder+server child, UPnP feed, audio-view status. **Help overlay** (`h` key). **Consistent keys**: `c`=cast `d`=delete `s`=search `m`=mute `a`=add `f`=favorite. One output channel (`output.rs`). Python `faeOS/bin/siren` is archived (tests/rollback only). |

## Iteration rule

Engine is Rust. TUI layout: top = content (browser/queue/audio/trove),
bottom = focused view's menu (Option A); waves strip stays visible.
**Help overlay** on `h`. **Key consistency**: same key = same meaning
across views; uppercase for view-specific variants.
