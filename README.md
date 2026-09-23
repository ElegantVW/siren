# Siren — the Aether's music vessel

faeOS first-party media player. mpv-backed local playback plus HEOS
network-speaker control, in one remote.

**Status:** rust scaffold (v0.1.0). The live player is still the Python
implementation at `faeOS/bin/siren` (2477 lines); this repo ports it slice
by slice. **No trove/Internet-Archive support in v1.**

## Layout

- `src/` — rust engine (`cargo build --release`)
- `scripts/siren` — thin launcher (never a prebuilt ELF in git)
- `build.sh install` — engine → `~/.local/lib/faeos/siren`, launcher → `~/bin/siren`

## TUI (target)

Two boxes. Top = content view (Tab cycles browser → queue → waves → audio;
waves stays on top). Bottom = context menu of the focused view (Option A):

| Focus   | Bottom menu                          |
|---------|--------------------------------------|
| browser | play · add to queue · cast to speaker |
| queue   | play · remove · move · clear          |
| waves   | pause · next · bands · +/-            |
| audio   | output local\|heos · speaker · vol · test-cast |

## Audio (target)

Siren owns its audio routing (`~/.config/siren/config.json`):

```
siren audio                  # output, speaker, volumes, DLNA status
siren audio output heos      # route play/pause/next/now to the speaker
siren audio output local     # back to mpv (default)
siren cast <query>           # top library match → Vanguarda Office (DLNA)
```

Default speaker: **Vanguarda Office** (HEOS 1, `192.168.8.184`), `--speaker`
for Cave. Protocol: HEOS CLI on TCP/1255 (newline `heos://` URIs) +
`VANGUARDA-DLNA` (minidlna on `:8200`); queue via `add_to_queue aid=1`
with raw `$` in cid/mid.

## Build

```bash
./build.sh          # debug-check: cargo build --release
./build.sh install  # engine + launchers
```
