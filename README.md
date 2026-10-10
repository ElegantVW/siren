![Siren hero](assets/hero/siren.svg)

# Siren — the Aether's music vessel 🎵

faeOS first-party media player. mpv-backed local playback plus HEOS
network-speaker control, in one remote.

**Status:** default engine (v0.1.0, cut over 2026-09-23). Ported: config,
library/fuzzy, mpv IPC, queue/playlist, transport CLI, two-box TUI,
audio menu + `cast`, trove (CLI + TUI), tags (`ffprobe`), directory
browser — all live-verified. `~/bin/siren` is the thin launcher to the
Rust binary. The kit Python player was removed 2026-09-27. The remaining Python is
the `:8899` stream encoder (`scripts/siren-stream`), not a second player.

House audio: [`docs/AUDIO.md`](docs/AUDIO.md).

For: local files + HEOS speakers from one remote. Not for: Spotify/streaming apps, Bluetooth, multi-OS.

## Look

![Siren browser](assets/screenshots/siren-tui.png)
![Siren queue](assets/screenshots/siren-queue.png)

```
~~~~~~~
 ~~~~~ 
~~~~~~~
 ~~~~~ 
~~~~~~~
```

## Layout

- `src/` — rust engine (`cargo build --release`)
- `scripts/siren` — thin launcher (never a prebuilt ELF in git)
- `build.sh install` — engine → `~/.local/lib/faeos/siren`, launcher → `~/bin/siren`

## TUI

Two boxes + a persistent waves strip. Top = content view (Tab cycles
browser → queue → audio → trove → radio → viz). Bottom = context menu
of the focused view. Waves strip shows speaker state + 3-row spectrum
(local files) or `· live stream ·` (radio). Press `h` for help overlay
(matches the handlers; any key closes it).

**Global keys** (every view; these fire first):

| Key | Action |
|-----|--------|
| Tab / Shift-Tab | cycle views |
| `n` / `b` | next / previous |
| `p` | pause / resume |
| ← / → | seek ±5s (Shift ±30s) |
| `+` / `-` | volume (HEOS while following) |
| `h` | help overlay |
| `q` | quit |
| Esc | quit (closes a picker first) |

**View keys** (uppercase = variant):

| View | Keys |
|------|------|
| browser | Enter/space play · `a` add · `c` cast · ⌫ up · `/` filter · `S` save · `L` load · `R` rm |
| queue | Enter play-from · `d` remove · `C` clear · `s` shuffle |
| audio | `o` local/heos · `S` speaker · `W` follow · mixer `j`/`k`/`m` · `g`/`u`/`G` group · `,`/`.` group vol |
| trove | `s` search · Enter version · `D` download · `M` more · `A` all · `F` format |
| radio | Enter/space play · `C` country · `s` search · `f` fav · `A` URL · `M` more |
| viz | `t`/space flip · `V` zoom |

**Group control** (audio view): `g` groups all speakers under current
speaker as leader, `u` ungroups, `G` toggles group mute, `,`/`.` adjust
group volume. HEOS natively syncs grouped speakers — verified 1–2ms
drift.

**Mixer** (audio view): every live source listed with app, volume,
muted state. `j/k` select, `m` mute/unmute one source. `+/-` is global
transport volume (HEOS while following). Group volume is `,`/`.`. The
loopback is filtered out (infrastructure, not a source).

**Radio honesty**: favorites play their own saved URL first (direct
`play_stream`), TuneIn only as fallback. Playback is verified by
polling speaker state — dead streams report failure instead of fake
success. HTTP URLs are pre-checked with HEAD, then GET when HEAD is
400/405 (Icecast); 404 stays dead. HTTPS passes through (no TLS in std).

**Loading cue**: play runs in a job thread so the TUI never freezes.
While it runs, the bottom status line shows `| connecting… <name>`
with an animated `|/-\` spinner. The waves strip is reserved for the
visualizer and is never used for status.

**Visualizer** (`V` view, last Tab stop): live spectrum from the
PipeWire monitor. 60fps while animating. Braille rendering (2×4 dots
per cell). `t`/`space` flips horizontal ↔ vertical; `V` cycles zoom
1×/2×/3×. Peak-hold caps fall slowly. The waves strip carries a compact
horizontal braille spectrum.

**Follow** (audio view `W`, or `siren audio follow on|off`): Cave plays
Siren music. Headset (front combo jack) stays the PipeWire default for
calls and everything except that music. Follow pins mpv to
`Siren_Master`; the encoder captures `Siren_Master.monitor`, serves
WAV/L16 on `:8899/siren.wav`, UPnP to Cave. `follow off` unpins mpv and
leaves the headset default. Login unit `siren-follow.service` runs
`follow on`. `now` labels follow as **Siren Follow** (UPnP, not CLI
`unknown`). Seek is skipped on the live wav. Volume keys move HEOS
group volume. Queue `s` shuffles. `p` / ← → are global pause and seek.

`output local` is how radio rides follow. `output heos` is Cave fetching
TuneIn or DLNA itself (needs WAN).

## Audio

Siren owns its audio routing (`~/.config/siren/config.json`):

**Group mode:** `g` in audio view groups all HEOS speakers.
HEOS handles sync natively. Verified: 1–2ms drift between speakers.

```
siren audio                  # output, speaker, volumes, DLNA status
siren audio probe            # LAN doctor: cached IPs vs /24 :1255
siren audio follow on        # mpv → Siren_Master → Cave; headset stays analog
siren audio vol +10          # HEOS group volume while following
siren seek +5                # skip live wav; files only
siren audio output heos      # Cave fetches the URL (needs WAN)
siren audio output local     # mpv; radio rides follow
siren cast <query>           # top library match → current speaker (DLNA)
siren audio queue              # speaker queue: qid · song — artist
siren audio queue play 2       # play / rm / clear / move 1 3
siren cast QUERY --next        # queue play-next (aid=2)
siren cast QUERY --append      # add to end (aid=3)
siren queue add QUERY          # local queue persists (~/.config/siren/queue.json)
```

Default speaker while Office is off this LAN: **Cave**. Roster lives in
`~/.cache/siren/heos-roster.json` — identity is name/pid, not a cached
IP. `siren audio probe` is the doctor when a box goes missing.
Protocol: HEOS CLI on TCP/1255 (newline `heos://` URIs) +
`VANGUARDA-DLNA` (minidlna on `:8200`); queue via `add_to_queue aid=1`
with raw `$` in cid/mid.

## Trove

Free & legal music (Internet Archive + ccMixter), zero new Rust deps (system `curl`):

```
siren trove music lofi      # 20, then [m] more (infinite scroll)
siren trove live grateful   # Live Music Archive (etree)
siren trove ccmixter lofi   # ccMixter remixes (needs Referer, handled)
siren trove get <identifier>
siren trove about
```

TUI: 4th Tab stop (`trove`), `s` search · Enter pick version · `D`
download cursor · `j/k` + auto-prefetch near the end · `M` more · `A`
all (double-press) · `F` session format · mouse wheel + click select,
double-click acts. Search, metadata and downloads run in background
threads.

## Radio

Community stations (radio-browser.info, keyless), same output channel
as music — local mpv or speaker `play_stream`:

```
siren radio countries         list countries (a-z)
siren radio stations Portugal stations for a country
siren radio search lofi       search by name
siren radio play <words>      play top match
siren radio fav               list favorites
siren radio fav <words>       toggle favorite
```

TUI: 5th Tab stop (`radio`). `Enter` with no country opens the country
picker (a-z, cached a week); `Enter`/space plays the cursor row, `C`
re-picks country, `s` searches, `f` toggles favorite (★ pinned on top),
`A` favorites any stream URL, `M` more. Favorites + last country persist
in `~/.config/siren/radio.json`. Streams ride the queue as URL items
(station name as label). Speaker radio rides TuneIn (no login): raw-URL
`play_stream` won't hold on this unit (verified dead 5 ways), but
`browse/play_stream` with a TuneIn `mid` sustains — output maps the
station name to TuneIn live. Local radio is plain mpv.
Guards: `.part` resume, skip-if-exists, 40-file cap, 32MB confirm,
`TROVE_MAX_TOTAL`. Lands in `~/Music/trove/<identifier>`.

## Build

```bash
./build.sh          # debug-check: cargo build --release
./build.sh install  # engine + launchers + siren-follow.service
```
