# Siren audio house

How this box talks to Ether, how music reaches Cave, and which ports
must be open. Public surface stays in [`README.md`](../README.md).
Agent contract: [`AGENTS.md`](../AGENTS.md).

## Two layers share Cave

Mixing them is how radio vanishes.

```
Headset default (calls, Zen, WhatsApp)
  PipeWire analog-stereo  ← front combo jack
  wireplumber 51-headset-duplex.conf

Siren music (follow on)
  mpv --audio-device=pulse/Siren_Master
    → Siren_Master.monitor
    → siren stream serve  :8899/siren.wav
    → Cave UPnP SetAVTransportURI  (TCP 60006)
    → Cave HTTP GET 192.168.11.1:8899
```

Follow does **not** steal the PipeWire default. Headset stays analog.
Browser tabs, WhatsApp, and `whisper ether` / `listen ether` (Bluetooth
weaves) ride the default sink. Siren mpv is pinned **off** it.

Player (`output::play_from`) is a second switch, independent of follow:

| `audio_output` | What happens |
|----------------|--------------|
| `local` | queue + radio through mpv. With follow on, Cave hears Siren. |
| `heos` | Cave fetches itself (TuneIn browse/play_stream, or DLNA `:8200`). Needs WAN. |

`siren now`, `siren status`, and `siren audio` publish Cave UPnP while
follow is on, even when `audio_output=local` (mpv rides Master; the
speaker is still the transport). STOPPED drops leftover TuneIn titles.

Audio-view `o` flips that switch. `heos` is how radio leaves follow and
Cave goes STOPPED on a leftover TuneIn title.

Stop recipe: `siren stop` then `siren audio follow off`.

## Local analog (AUX into a HEOS box)

Office (or any amp) on a rear analog jack is the sound card, not HEOS
CLI. `audio_output=heos` tries Cave UPnP (`192.168.11.51`) and fails
when that box is only on AUX.

```
siren audio output local     # TUI `o` until the hint says local
siren audio follow off       # mpv must not sit on Siren_Master
```

PipeWire sink `alsa_output.pci-0000_31_00.4.analog-stereo`, port
`analog-output-lineout`. That is Front L/R on this ALC1220 (codec pin
0x14, rear, colour red in the pin default). Surround/center jacks stay
silent on the stereo profile. On the HEOS 3, source must be **AUX**.

TUI mixer: first fader is Cave on the network; **phones** is this analog
sink. Drag phones for AUX loudness. Global `+/-` on the mixer tab is
the selected fader.

## Ether is a file cache

Ether (`~/bin/ether`) is the faeOS **network creature**. Siren does not
exec it, does not open a socket to it, and does not share a bus.

Ether writes the HEOS fleet when it sweeps:

`~/.config/ether/heos.json` → `{ ts, players[{name,pid,ip,network,…}] }`

Siren `heos::sweep_known()` reads that file first, then
`~/.cache/siren/heos-roster.json`, and keeps IPs that still answer
`:1255` (400 ms). Identity is **name/pid**, not a cached IP. A live
roster write refreshes Ether's file so shop Wi-Fi addresses die.

Other shared edges (still not IPC):

- `ether net` — IPv4 HTTPS probe (archive.org). Trove prints a tip when
  the archive is dark. Trove uses system `curl` / `ia.py`.
- `whisper ether` / `listen ether` — Bluetooth default-sink weaves
  (ANC / JBL). Opposite of follow.

Speaker LAN addressing is Siren's (`systemd/10-siren-heos.network`,
`scripts/siren-heos-dhcpd.py`), not Ether's.

## Ports

| Port | Who | Why |
|------|-----|-----|
| TCP 1255 | HEOS CLI | `heos://` newline JSON. IPv4 only. |
| TCP 60006 | HEOS UPnP | SetAVTransportURI / GetTransportInfo / Pause. RSTs until the speaker has DNS + WAN. |
| TCP 8899 | `siren stream serve` | Cave GETs `/siren.wav` (WAV/L16). UFW must allow speaker LAN. |
| TCP 8200 | minidlna `VANGUARDA-DLNA` | File cast. Stock unit hides `~/Music` under `ProtectHome=yes`. |

Never poll `:1255` in a tight loop — this firmware wedges its CLI.
Honesty while the wav is up comes from UPnP `:60006`.

## Speaker LAN (this house)

USB tether (`enp49s0f3u1`) keeps the default route. Box ethernet
(`enp39s0`) is the speaker LAN:

- host `192.168.11.1/24` (+ `169.254.200.1/16` so AutoIP Cave is reachable)
- Cave lease `192.168.11.51` (MAC `00:05:cd:6d:52:e8`)
- `siren-heos-dhcpd.service` (AF_PACKET) hands the lease + DHCP option 6
  (`1.1.1.1`, `9.9.9.9`)
- `heos::local_ip()` prefers `enp39s0` / RFC1918 wired /24, never the
  tether default route when both are up. Override: `SIREN_LAN_IP` /
  `SIREN_LAN_IFACE`.

Cave's UPnP `:60006` RSTs until DNS and a WAN path exist. Isolated
`ip_forward=0` leaves the renderer dead. Session NAT:

- `ip_forward=1`
- MASQUERADE out the USB tether
- FORWARD ACCEPT **before** UFW DROP

UFW shop allow is `192.168.8.0/24`. Speaker LAN needs
`8899/tcp` from `192.168.11.0/24` or Cave cannot GET the wav.

NAT lives in `siren-heos-forward.service` (MASQUERADE + FORWARD
ACCEPT at position 1). A reboot of this box restores it with the unit.

Office on the spare repeater is Setup B — not until we need it.
Bluetooth is not a Siren path. Default speaker while Office is
off-LAN: **Cave**.

## Follow

`siren audio follow on` (TUI `W`, login unit `siren-follow.service`):

1. Pin mpv to `pulse/Siren_Master`. Headset default stays analog.
2. Encoder (`siren stream serve`) captures
   `Siren_Master.monitor`, listens `:8899`.
3. UPnP SetAVTransportURI to Cave with `http://<lan-ip>:8899/siren.wav`.
4. Wait until transport is PLAYING **or TRANSITIONING** — the first
   wav GET is TRANSITIONING; treating that as failure kills the encoder.
5. Write `~/.cache/siren/follow.json`.

`follow off` unpins mpv and leaves the headset default. `not-on-lan`
leaves the headset so the box is never silent.

The login unit is Type=simple `siren audio follow run`: it stays
active while the encoder is alive and goes inactive when `:8899` dies.

TUI waves (and the viz view) hunt one source at a time: headset analog,
then `Siren_Master.monitor`, then the HEOS station HTTP URL (follow
puts Master first). A silent capture is skipped quickly; HTTP is last
so curl only runs if Pulse is quiet. Opening the TUI before follow,
then `W` / the login unit, retargets the hunt list. The small strip
hides on the viz tab — that tab *is* the waves.

Volume keys move **HEOS** volume, not the null-sink percent. Seek on
`/siren.wav` (or duration `00:00:00`) is skipped — firmware wedges
TRANSITIONING. Pause is UPnP Pause/Play. CLI `get_play_state` is
`unknown` for this PCM; `now` / `status` publish UPnP and label
**Siren Follow**.

## Reclaim

`now --refresh` calls `maybe_reclaim` when:

- `follow.json` is present and `want=play`
- transport is STOPPED (foreign URL **or** the wav)

PLAYING radio/cast is left alone. `siren stop` on the wav sets
`want=stop` so Cave stays silent. Empty URI stays stopped.

## minidlna

Stock unit: `DynamicUser=yes` + `ProtectHome=yes`. A same-path bind of
`~/Music` never attaches. House drop-in
(`/etc/systemd/system/minidlna.service.d/siren.conf`, tree copy
`systemd/minidlna-siren.conf`) binds Music read-only onto
`/run/minidlna/music` and rewrites `media_dir`. Until that drop-in is
the one installed, VANGUARDA-DLNA lists 0 files and library `cast` is
dead.

## Listener keys

Help (`h`) matches the handlers. Global `+/-` is transport volume
(HEOS while following). Mixer `j`/`k`/`m` select and mute sources;
source volume on `+/-` never fires. Esc quits (closes a picker first).
Full tables: README and AGENTS.
