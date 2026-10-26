# Siren changelog

## Unreleased — radio is not the file queue

- `player::alive` is a live IPC connect. A leftover sock after logout is
  unlinked and mpv respawns. Local play no longer prints Queue empty for
  a dead socket (`mpv not ready`).
- `siren radio play` / TUI radio call `output::play_station` — one URL
  through mpv (or TuneIn on heos). They do not write `queue.json`.
  The file queue stays for playlists and library tracks.

## 2026-10-11 — siren heos JSON

- `siren heos` for Wisp: list, vol, mute, play/pause/stop, source aux|bt|net,
  probe, group/ungroup. `play_input` for AUX/BT. No Denon sign-in.

## 2026-10-11 — mixer tab

- New Tab stop: vertical faders for the current speaker, analog
  phones (and other Pulse sinks), and live apps. Mouse drag / house-wave
  mute glyph / wheel. On this tab `+/-` is the selected fader, and
  unshifted arrows pick a channel. `Siren_Master` stays off the board.

## 2026-10-10 — sea-glass (v0.1.1)

- GitHub Release: linux x86_64 + windows x86_64 `.exe`. Grove card has
  the buttons. macOS is still source (`docs/PORT.md`). mpv is required.
  Waves/mixer/follow skip on Windows (no Pulse).

## 2026-10-10 — tab push, no viz flip/zoom

- Viz is horizontal only. `t` / space / `V` no longer flip or zoom.
- Tab / Shift-Tab push the top pane ~180ms (ease-out cubic). Waves
  strip and menu snap to the destination. 60Hz while the push is live.

## 2026-10-10 — waves at 60Hz

- Event loop schedules 60Hz from the target tick (period
  `1e9/60` ns). Draw time is not added as extra sleep. Late frames
  resync. Smoothing is snappier (attack 0.80 / release 0.45 toward
  the new hop) so 60 frames read as 60Hz. `HOP` 128, `WINDOW` 2048.

## 2026-10-10 — waves low-delay capture

- Viz ffmpeg matches the follow encoder: `nobuffer` / `low_delay` /
  Pulse `probesize 32` / `fragment_size 1920`. HTTP probe is 256KiB /
  1s so Icecast AAC still detects (`probesize 32` on `pipe:0` was
  `Invalid data` and empty waves). curl `-N`. `HOP` 128 (~5.8ms).
  Window stays 2048. Draw stays 60fps. Killed ffmpeg/curl are wait()ed.

## 2026-10-10 — waves hunt a live source

- If the current capture is silent, viz steps to the next candidate.
  Order: analog monitor, `Siren_Master.monitor`, HEOS HTTP last
  (follow puts Master first). One ffmpeg (and optional curl) at a
  time. Quiet Pulse ~280ms, HTTP ~1s, then hop. After a full silent
  round, 1.5s pause so curl does not loop. No extra readers, no
  follow restart, Cave play left alone.

## 2026-10-10 — waves tap HEOS radio URL

- With `audio_output=heos`, viz taps the station `mid` when it is
  `http://` / `https://`. curl DoH (`1.1.1.1`) feeds ffmpeg — this
  box's stub DNS SERVFAILs some radio hosts. Follow still uses
  `Siren_Master.monitor`. Headset analog stays the local path.
- Cave play is left alone (no follow restart, no SOAP Stop).

## 2026-10-10 — Rust follow encoder, TUI does not bind :8899

- `siren stream serve` is the WAV/L16 HTTP encoder (same ELF). Python
  `scripts/siren-stream` is gone. Opening the TUI starts viz ffmpeg
  only; follow (`W` / `audio follow on`) is what binds `:8899`.
- Opening the TUI starts viz ffmpeg only. Follow binds `:8899`.

## 2026-10-10 — compat tests on this box

- `tests/compat_play.rs` plays a generated wav through the siren binary
  with isolated HOME and `SIREN_AO=null` (no analog, no Cave).
- `scripts/compat-debian.sh` (nspawn) and `scripts/compat-wine.sh` (CLI).

## 2026-10-10 — portable local playback

- Paths use `$HOME` / `%USERPROFILE%` (no `/root`, no host home).
- mpv IPC: unix socket or Windows named pipe. `pactl` / ffmpeg / follow
  skip when missing. `./build.sh install` → `~/.local/bin`; `--house`
  is the faeOS follow unit.
- Looking for testers: README + [`docs/PORT.md`](docs/PORT.md).

## 2026-10-10 — five TUI palettes

- Global `T` cycles pixie / blush / silver / siren / cave (house pinks +
  Siren teal). RGB is compiled in. `siren config set palette siren`.

## 2026-10-10 — viz tab is the waves

- The small waves strip hides on the viz tab; that height goes to the
  full visualizer. Other views keep the strip.

## 2026-10-10 — waves follow Siren_Master

- TUI waves retarget onto `Siren_Master.monitor` when follow comes up
  (they used to stay on silent headset analog). `viz::stop` on TUI exit
  leaves the follow encoder.

## 2026-10-10 — music-only follow, dual-home Cave

- Follow is Siren music only: mpv pinned to `Siren_Master`; headset analog
  duplex stays the PipeWire default (calls, WhatsApp). `follow off` unpins
  mpv. Cave pulls `:8899/siren.wav` over the speaker LAN.
- Dual-home: USB tether keeps the default route; speaker LAN is the box
  ethernet (`192.168.11.1/24`). `heos::local_ip()` prefers that iface.
  dhcpd helper + option 6 DNS. Cave UPnP `:60006` needs DNS + WAN
  (session NAT, FORWARD before UFW DROP). UFW `8899/tcp` from the
  speaker LAN.
- Honesty: `TRANSITIONING` maps to play so the encoder survives Cave's
  first wav GET. Seek still skipped on `/siren.wav`.
- TUI help and view menus match the handlers: global `n`/`b`/`+/-`/`W`,
  Esc quits (closes a picker first), mixer `+/-` is global volume.
- Docs: [`docs/AUDIO.md`](docs/AUDIO.md) (Ether file cache, two audio
  layers, ports). Default speaker while Office is off this LAN: Cave.
- Follow health: `siren audio` prints encoder / `:8899` / Cave GET /
  UPnP. Login unit is Type=simple `follow run` (inactive when the
  encoder dies). Reclaim STOPPED-on-wav when `want=play`; `siren stop`
  sets `want=stop`.
- `now` drops leftover titles when transport is STOPPED. Follow on +
  `audio_output=local` still publishes Cave UPnP (`Cave — Siren Follow`);
  cached now does not recurse into the live path.
- Default pick is Cave; `pick_named("Office")` still finds Office.
- Live roster writes `~/.config/ether/heos.json`.
- `siren-heos-forward.service` persists speaker-LAN NAT. House minidlna
  drop-in binds `~/Music` onto `/run/minidlna/music`.

## 2026-10-02 — listen journeys

- Heal: Office was `TRANSITIONING` after a Seek on the live wav;
  follow restart restored PLAYING. Seek on `/siren.wav` / duration 0
  is skipped (firmware wedges).
- Honesty: `now`/`status`/`audio` publish UPnP transport + **Siren Follow**
  while the wav is up. HEOS CLI `unknown` is not the published state.
- Pause while following is UPnP Pause/Play. `siren seek +5|-5|30%|mm:ss`;
  TUI ←/→ ±5s (Shift ±30s). Queue view `s` shuffles.
- Follow reclaim: `now --refresh` and `stop` restore the wav when the
  renderer is STOPPED on a non-wav URI.
- Volume: `siren audio vol +N|-N`; while following, HEOS group volume.
  i3 XF86 raise/lower → those commands.
- minidlna: stock `ProtectHome=yes` hid `~/Music` (0 audio files).
  Drop-in binds the library read-only.

## 2026-10-02 — heos sound card

- Follow makes the HEOS pair the machine default: PipeWire default →
  `Siren_Master`, encoder captures `Siren_Master.monitor` (WAV/L16,
  `:8899/siren.wav`), Office plays that URL, Cave `zone://zone`.
- `capture_source` no longer skips Master. `follow off` restores S/PDIF.
- User unit `siren-follow.service` (`build.sh install` enables it).
  `not-on-lan` leaves the hardware sink.
- TUI exit leaves follow up when the sound card is on.
- `follow on` waits until the leader reports PLAYING (retry Play).
- Encoder stop kills python by /proc argv, not `pkill -f`.
- Tests for capture/Master, prev_default, pactl parsers, PLAYING tag,
  encoder cmdline, follow unit, `build.sh restart`, TUI exit, not-on-lan
  fail-closed.

## 2026-10-02 — pcm follow

- `siren audio follow on|off` / TUI `W`: default-sink monitor → WAV/L16
  HTTP (`:8899/siren.wav`, 4 KiB chunks) → HEOS UPnP. Local S/PDIF muted
  while following. mp3+64KiB was the seconds-late radio path.
- HEOS Sink (live): `audio/L16` 44.1/48k + `audio/wav`. Group sync still
  taxes two-room delay (Denon AUX law).
- Live: first WAV byte **98 ms**. Office PLAYING `http://192.168.8.186:8899/siren.wav`;
  Cave `zone://zone` same RelTime (grouped). Allow-updates **Off** on both.
  Firmware blob is not downloadable over HTTP (`/update/heos.update` 501);
  dump waits on UART. Audio delay not mic-timed this pass.

## 2026-10-02 — heos wired

- Both boxes on this ethernet LAN. Last-known: Office HEOS 1
  `192.168.8.187`, Cave HEOS 5 `192.168.8.192` (old `.184`/`.185` dead).
- `sweep_known` fallbacks follow that pair. `siren audio probe` → `ok`.
- Stock CLI has no login; firmware rewrite skipped.

## 2026-10-02 — heos probe

- Speakers can be powered and still absent from this ethernet LAN (no
  `:1255` on the `/24`). Cached IPs are last-known, not live.
- Parallel `/24` sweep (64 workers × 350ms, budget < 4s) so
  `now --refresh` finishes inside its 5s deadline when IPs move.
- `siren audio probe` / `--json`: `ok` | `ip-moved` (adopts roster) |
  `not-on-lan`. Tests lock the verdict table, sweep budget, mock CLI,
  and `parallel_open`. Ignored live test: `live_lan_has_heos_cli`.

## 2026-10-02 — audit

- Identity: public command is the Rust engine (`~/bin/siren` launcher →
  `~/.local/lib/faeos/siren`). Kit Python player is gone; `build.sh`
  no longer mentions it. Stream encoder on `:8899` is `siren stream serve`.
- `url_alive` retries GET when HEAD is 400/405. OXIGÉNIO was HEAD 400 /
  GET 200 and was being skipped as dead while a leftover mpv played it.
- `siren now` / `status` / `audio` use the 5s deadline; expired roster
  still names Cave/Office with `stale` instead of a blank unreachable.
- Discovery `/24` sweep is only on `now --refresh`, not on every resolve.
- Audio view `S` cycles speakers (the shuffle arm made that unreachable).
- Gapless spawn flag follows config.
- Tests: 60 pass, 2 ignored (url_alive mock, queue URL validate, stale snap, pick).

## v0.1.0 (2026-09-23)

- Cut over to Rust engine: config, library/fuzzy, mpv IPC, queue/playlist,
  transport CLI, two-box TUI, audio menu + `cast`, trove, tags, dir browser.
- Speaker: HEOS CLI :1255 + DLNA; test casts at low volume.
- Evidence: live-verified; `cargo test` 2 pass.
- Holes: speaker radio rides TuneIn; raw-URL `play_stream` won't hold on this unit.
