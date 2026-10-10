![Siren hero](assets/hero/siren.svg)

# Siren — the Aether's music vessel 🎵

faeOS first-party media player. mpv-backed local playback plus HEOS
network-speaker control, in one remote.

**Status:** default engine (v0.1.0, cut over 2026-09-23). Ported: config,
library/fuzzy, mpv IPC, queue/playlist, transport CLI, two-box TUI,
audio menu + `cast`, trove (CLI + TUI), tags (`ffprobe`), directory
browser — all live-verified. `~/bin/siren` is the thin launcher to the
Rust binary. The kit Python player was removed 2026-09-27. The remaining
Python is the `:8899` stream encoder (`stream.rs` / `scripts/siren-stream`),
not a second player.

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

House audio diagram: [`docs/AUDIO.md`](docs/AUDIO.md).

**Global keys** (every view; `handle_key` eats these first):

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
| audio | `o` local/heos · `S` speaker · `W` follow · `r` repeat · `v` refresh · `t` test · mixer `j`/`k`/`m` · `g`/`u`/`G` group · `,`/`.` group vol |
| trove | `s` search · Enter version · `D` download · `M` more · `A` all · `F` format |
| radio | Enter/space play · `C` country · `s` search · `f` fav · `A` URL · `M` more |
| viz | `t`/space flip · `V` zoom |

**Group control** (audio view): `g` groups all speakers under current
speaker as leader, `u` ungroups, `G` toggles group mute, `,`/`.` adjust
group volume. HEOS natively syncs grouped speakers — verified 1–2ms
drift.

**Mixer** (audio view): every live source listed with app, volume,
muted state. `j/k` select, `m` mute/unmute one source. `+/-` is global
transport volume (HEOS while following) — it never reaches mixer source
volume. Group volume is `,`/`.`. The loopback is filtered out
(infrastructure, not a source).

**Radio honesty**: favorites play their own saved URL first (direct
`play_stream`), TuneIn only as fallback. Playback is verified by
polling speaker state — dead streams report failure instead of fake
success. HTTP URLs are pre-checked with HEAD, then GET when HEAD is
400/405 (Icecast/OXIGÉNIO); 404 stays dead. HTTPS passes through
(no TLS in std).

**Loading cue** (`tui.rs` `start_play`/`poll_play`): play is an async
job thread, single-flight, harvested on the UI tick. Spinner `|/-\`
in the bottom status line while running — **never the waves strip**
(reserved for the visualizer). **Never poll 1255 in a loop**: this
firmware wedges its CLI under rapid requests; verify via UPnP 60006.

**Network calls need a deadline.** `now --refresh` runs a parallel
`/24` :1255 sweep (64 workers × 350ms, budget < 4s) then falls back
to the roster cache. `heos::local_ip()` picks the **speaker LAN**
(`enp39s0` / RFC1918 wired /24), never the USB-tether default route
when both are up. Override: `SIREN_LAN_IP` / `SIREN_LAN_IFACE`.
`sweep_known` uses ether cache + last roster; shop `192.168.8.x` is
not a hardcoded fallback. Identity is not a cached IP.
Direct Cave on `enp39s0` is `192.168.11.1/24`; Cave lease is
`192.168.11.51` (MAC `00:05:cd:6d:52:e8`). systemd-networkd
DHCPServer bound `:67` and logged STARTED but never OFFERed Cave's
DISCOVER; `siren-heos-dhcpd.service` (AF_PACKET helper) hands the
lease and DHCP option 6 (`1.1.1.1`, `9.9.9.9`). Cave's UPnP
`:60006` RSTs until the speaker has DNS and a WAN path — isolated
`ip_forward=0` leaves the renderer dead. Speaker-LAN NAT is `siren-heos-forward.service`:
MASQUERADE out the USB tether plus FORWARD ACCEPT **before** UFW
DROP. UFW shop allow is `192.168.8.0/24`; speaker LAN needs
`8899/tcp` from `192.168.11.0/24` or Cave cannot GET the wav.
**Office** on the spare repeater is Setup B — not until we need it.
Bluetooth is not a Siren path. Default speaker while Office is
off-LAN: **Cave**.
`siren audio probe` is the doctor: `ok` / `ip-moved` (adopts new IPs)
/ `not-on-lan` (wrong VLAN, unplugged cable, or HEOS WiFi sleep). A
status command must always answer.

**Ether** (`~/bin/ether`) is the network creature. Siren does not exec
it. The only coupling is `~/.config/ether/heos.json`. Ether writes the
HEOS fleet when it sweeps; `heos::sweep_known` reads it, then the Siren
roster. `write_roster_cache` writes that file back from the live
roster so shop Wi-Fi IPs die. Identity is name/pid, not a cached IP.
`ether net` is the archive.org probe (trove prints a tip).
`whisper ether` / `listen ether` are Bluetooth default-sink weaves;
follow pins mpv **off** the default.

**No blocking I/O on the render thread.** `pactl` costs 233–477ms and
HEOS `get_groups` is a network round-trip — both live in `spawn_bg`'s
worker thread, harvested via channel. `poll_stream` is a pure state
update. The draw path spawns nothing. Regression-tested by
`no_blocking_tests` (source-scan asserts every slow call is inside a
`std::thread::spawn`). `stream::running()` is `try_wait` — cheap, and
fine to call anywhere.

**Visualizer** (`viz.rs`, `V` view): live FFT from the PipeWire
monitor (`stream.rs::capture_source`), so it follows real audio —
not file-decoded. **Latency**: `HOP`=256 slides the window (11.6ms
updates) while `WINDOW`=2048 keeps frequency resolution; the event
loop renders at 60fps only while `animating()` (Viz view, live
spectrum, or a play job) and falls back to a 500ms tick otherwise —
CPU stays proportional to visible motion. Network polls never run
at frame rate. Braille packer (2×4 dots/cell) is pure + tested.
Orientations: `render_horizontal` (bars up) / `render_vertical`
(bars right). `t`/`space` flips, `V` zooms 1×/2×/3×. Smoothing is
attack-fast/release-slow; peak caps tick down. Waves strip carries
a compact horizontal braille spectrum.

**Follow** (audio view `W`, or `siren audio follow on|off`): Cave is
Siren's speaker, **not** the PC sound card. Headset (front combo
jack, analog duplex) stays the PipeWire default for calls and
everything except Siren music. Follow pins mpv to `Siren_Master`;
the encoder captures `Siren_Master.monitor`, serves WAV/L16 on
`:8899/siren.wav`, UPnP to Cave. `follow off` unpins mpv and leaves
the headset default. Login unit `siren-follow.service` is Type=simple
`follow run` (on, then wait on the encoder); `not-on-lan` leaves the
headset so the box is never silent. Unit inactive means `:8899` is dead.
These boxes advertise L16/WAV in GetProtocolInfo; mp3 was the
seconds-late radio path. Grouped HEOS still adds Denon's sync buffer
(documented for AUX). First WAV byte ~98 ms; speaker prebuffer typically
100–300 ms on top.

**Honesty while following:** HEOS CLI `get_play_state` returns
`unknown` for UPnP PCM. `now` / `status` / `audio` publish UPnP
transport (`PLAYING` / `TRANSITIONING` → play) and label `Siren Follow`,
even when `audio_output=local` (mpv rides Master).
`follow on` waits for that mapped play so TRANSITIONING does not kill
the encoder before Cave's wav GET. CLI `unknown` is never the
published state when UPnP answers.

**Seek:** `siren seek +5|-5|30%|mm:ss`; TUI ←/→ ±5s (Shift ±30s).
mpv `time-pos` locally; HEOS SOAP Seek REL_TIME for file casts.
**Never Seek a URI containing `/siren.wav` or duration `00:00:00`** —
this firmware wedges `TRANSITIONING` (live-verified).

**Pause while following:** UPnP Pause/Play. HEOS CLI toggle treats
`unknown` as play and will not pause the wav.

**Volume while following:** `siren audio vol +N|-N` and TUI `+/-`
move **HEOS group volume**. i3 XF86 raise/lower call those commands.
Mute stays pactl. Do not raise Office/Cave in tests.

**Follow reclaim:** `now --refresh` calls `maybe_reclaim` when
`follow.json` is present, `want=play`, and transport is STOPPED
(foreign URL **or** the wav — Cave dropped the GET). PLAYING
radio/cast is left alone. `siren stop` on the wav sets `want=stop`
so Cave stays silent until the next `follow on` / play. Empty URI
stays stopped.

**minidlna:** stock unit is `DynamicUser=yes` + `ProtectHome=yes`.
ProtectHome mounts `/home` inaccessible, so a same-path bind of
`~/Music` never attaches. House drop-in binds Music read-only onto
`/run/minidlna/music` and rewrites `media_dir` in a runtime conf
(`/etc/systemd/system/minidlna.service.d/siren.conf`).

## Audio

Two layers share Cave. Mixing them is how radio vanishes.

- **Headset default** — front combo analog duplex (calls, Zen, WhatsApp).
- **Follow** — mpv pinned to `Siren_Master` → `:8899/siren.wav` → Cave UPnP.
- **`audio_output=local`** — queue/radio through mpv (rides follow).
- **`audio_output=heos`** — Cave fetches itself (TuneIn / DLNA). Needs WAN.
  Audio-view `o` flips this; local + follow is how Siren radio reaches Cave.

Siren owns its audio routing (`~/.config/siren/config.json`):

**Group mode:** `g` in audio view groups all HEOS speakers.
HEOS handles sync natively. Verified: 1–2ms drift between speakers.

```
siren audio                  # output, speaker, live, follow health (encoder/ESTAB/UPnP)
siren audio probe            # LAN doctor: cached IPs vs /24 :1255
siren audio follow on        # mpv → Siren_Master → Cave; headset stays default
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
IP. `pick_named("")` prefers Cave; `pick_named("Office")` still finds
Office when it joins. Protocol: HEOS CLI on TCP/1255 (newline
`heos://` URIs) + `VANGUARDA-DLNA` (minidlna on `:8200`); queue via
`add_to_queue aid=1` with raw `$` in cid/mid.

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

**Tests:** every new behavior lands with a unit or source-scan test in
the same pass. Pure helpers (name shape, JSON, pactl parsers, cmdline
match) over live LAN. `#[ignore]` only for boxes that must be on the
wire. `cargo test` is the gate.
