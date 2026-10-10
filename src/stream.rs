//! Siren stream — Siren_Master monitor → HEOS as PCM.
//!
//! Follow is **music only**. The PipeWire default sink stays the headset
//! (calls, browser, the rest of the box). mpv is pinned to `Siren_Master`;
//! the encoder captures that monitor, serves WAV / L16 on `:8899`, UPnP
//! SetAVTransportURI to Cave. `follow off` unpins mpv and leaves the
//! headset default alone.
//!
//! mp3+64KiB was a radio path (seconds late). L16/WAV is what these
//! boxes advertise in GetProtocolInfo.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 8899;
pub const STREAM_PATH: &str = "/siren.wav";
pub const STREAM_PATH_L16: &str = "/siren.l16";
pub const STREAM_MIME: &str = "audio/wav";
pub const STREAM_MIME_L16: &str = "audio/L16;rate=48000;channels=2";
pub const CHUNK_BYTES: usize = 4096;
pub const SAMPLE_RATE: u32 = 48000;
pub const MASTER: &str = "Siren_Master";
pub const FALLBACK_SINK: &str = "alsa_output.pci-0000_31_00.4.iec958-stereo";

/// Pulse monitor name for a sink (`Foo` → `Foo.monitor`).
pub fn monitor_of(sink: &str) -> String {
    format!("{sink}.monitor")
}

/// Capture source for viz: Master while following (music to Cave),
/// otherwise the default sink (headset).
pub fn capture_from_default(sink: Option<&str>) -> String {
    match sink {
        Some(s) if !s.is_empty() => monitor_of(s),
        _ => monitor_of(MASTER),
    }
}

pub fn capture_source() -> String {
    if following() {
        monitor_of(MASTER)
    } else {
        capture_from_default(default_sink().as_deref())
    }
}

#[allow(dead_code)]
pub const MONITOR: &str = "Siren_Master.monitor";

struct StreamState {
    child: Option<Child>,
    following: bool,
    muted_sink: Option<String>,
    was_muted: bool,
    speaker_ip: Option<String>,
    prev_default: Option<String>,
}

fn state() -> &'static Mutex<StreamState> {
    static STATE: OnceLock<Mutex<StreamState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(StreamState {
            child: None,
            following: false,
            muted_sink: None,
            was_muted: false,
            speaker_ip: None,
            prev_default: None,
        })
    })
}

/// Is our encoder+server child alive? Cheap waitpid — this process only.
pub fn running() -> bool {
    match state().lock().unwrap().child.as_mut() {
        Some(c) => c.try_wait().ok().flatten().is_none(),
        None => false,
    }
}

/// Encoder alive in this process **or** another (systemd / leftover python).
pub fn encoder_alive() -> bool {
    running() || !encoder_pids().is_empty()
}

/// First encoder pid, if any.
pub fn encoder_pid() -> Option<u32> {
    encoder_pids().into_iter().next()
}

pub fn following() -> bool {
    if state().lock().unwrap().following {
        return true;
    }
    follow_file().is_file()
}

/// Stream URL for a given local IP and port (WAV, the default feed).
pub fn url(local_ip: &str, port: u16) -> String {
    format!("http://{local_ip}:{port}{STREAM_PATH}")
}

pub fn url_l16(local_ip: &str, port: u16) -> String {
    format!("http://{local_ip}:{port}{STREAM_PATH_L16}")
}

fn script_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("siren-stream");
            if p.is_file() {
                return p;
            }
        }
    }
    let p = crate::paths::home_dir().join(".local/lib/faeos/siren-stream");
    if p.is_file() {
        return p;
    }
    crate::paths::home_dir().join("siren/scripts/siren-stream")
}

/// Start the encoder+server (monitor → PCM HTTP). Idempotent across processes.
pub fn start(port: u16) -> bool {
    if encoder_alive() {
        return true;
    }
    let cap = monitor_of(MASTER);
    let script = script_path();
    let port_s = port.to_string();
    let r = if script.is_file() {
        Command::new("python3")
            .args([script.as_os_str(), std::ffi::OsStr::new(&port_s), cap.as_ref()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
    } else {
        Command::new("python3")
            .args(["-c", &server_script(port)])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
    };
    match r {
        Ok(c) => {
            state().lock().unwrap().child = Some(c);
            for _ in 0..20 {
                std::thread::sleep(Duration::from_millis(250));
                if port_open(port) {
                    return true;
                }
            }
            false
        }
        Err(_) => false,
    }
}

/// Kill the encoder child and leftover python servers.
/// Do not `pkill -f siren-stream`: that substring lives in unrelated
/// command lines (install scripts, agent shells) and SIGTERMs them.
fn stop_encoder() {
    let child = state().lock().unwrap().child.take();
    if let Some(mut c) = child {
        let pid = c.id();
        let _ = Command::new("kill").arg(pid.to_string()).status();
        let _ = c.wait();
    }
    for pid in encoder_pids() {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
}

/// Python processes whose argv contains a `siren-stream` *path* argument.
fn encoder_pids() -> Vec<u32> {
    let mut pids = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return pids;
    };
    for ent in dir.flatten() {
        let pid: u32 = match ent.file_name().to_str().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        let raw = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let args: Vec<&[u8]> = raw.split(|b| *b == 0).filter(|a| !a.is_empty()).collect();
        if !cmdline_is_encoder(&args) {
            continue;
        }
        pids.push(pid);
    }
    pids
}

fn cmdline_is_encoder(args: &[&[u8]]) -> bool {
    let Some(argv0) = args.first() else {
        return false;
    };
    let a0 = String::from_utf8_lossy(argv0);
    let base = a0.rsplit('/').next().unwrap_or(&a0);
    if !base.starts_with("python") {
        return false;
    }
    args.iter().any(|a| a.ends_with(b"/siren-stream") || *a == b"siren-stream")
}

/// Stop then start so ffmpeg binds the current default-sink monitor.
fn restart(port: u16) -> bool {
    stop_encoder();
    start(port)
}

/// Stop the encoder+server and undo follow routing.
pub fn stop() {
    follow_off(true);
}

fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_millis(300),
    )
    .is_ok()
}

fn server_script(port: u16) -> String {
    let cap = capture_source().replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"
import os, signal, subprocess
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
CAP = "{cap}"
CHUNK = {chunk}
FFC = ["ffmpeg","-hide_banner","-loglevel","error","-fflags","nobuffer","-flags","low_delay","-probesize","32","-analyzeduration","0","-f","pulse","-fragment_size","1920","-i",CAP,"-ac","2","-ar","48000"]
KIDS = []
def cleanup(*_):
    for p in list(KIDS):
        try: p.kill()
        except OSError: pass
    os._exit(0)
signal.signal(signal.SIGTERM, cleanup)
signal.signal(signal.SIGINT, cleanup)
class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def _pick(self):
        p = self.path.split("?",1)[0]
        if p == "/siren.wav": return "audio/wav", FFC+["-f","wav","-"]
        if p == "/siren.l16": return "audio/L16;rate=48000;channels=2", FFC+["-f","s16be","-"]
        return None, None
    def do_HEAD(self):
        mime,_ = self._pick()
        if not mime:
            self.send_response(404); self.end_headers(); return
        self.send_response(200)
        self.send_header("Content-Type", mime)
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
    def do_GET(self):
        mime, ff = self._pick()
        if not mime:
            self.send_response(404); self.end_headers(); return
        self.send_response(200)
        self.send_header("Content-Type", mime)
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        p = None
        try:
            p = subprocess.Popen(ff, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL, bufsize=0)
            KIDS.append(p)
            while True:
                chunk = p.stdout.read(CHUNK)
                if not chunk: break
                try:
                    self.wfile.write(chunk); self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError): break
        except Exception:
            pass
        finally:
            if p is not None:
                try: p.kill()
                except OSError: pass
                try: KIDS.remove(p)
                except ValueError: pass
ThreadingHTTPServer(("0.0.0.0", {port}), H).serve_forever()
"#,
        cap = cap,
        chunk = CHUNK_BYTES,
        port = port,
    )
}

/// POST a SOAP action to a speaker's UPnP AVTransport endpoint.
pub fn soap(ip: &str, action: &str, inner: &str) -> String {
    let body = format!(
        concat!(
            r#"<?xml version="1.0"?>"#,
            r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">"#,
            r#"<s:Body>{inner}</s:Body></s:Envelope>"#,
        ),
        inner = inner
    );
    let req = format!(
        "POST /upnp/control/renderer_dvc/AVTransport HTTP/1.1\r\n\
         Host: {ip}:60006\r\n\
         Content-Type: text/xml; charset=\"utf-8\"\r\n\
         SOAPACTION: \"urn:schemas-upnp-org:service:AVTransport:1#{action}\"\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        ip = ip,
        action = action,
        len = body.len(),
        body = body,
    );
    let mut s = match TcpStream::connect_timeout(
        &format!("{ip}:60006").parse().unwrap(),
        Duration::from_secs(5),
    ) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(8)));
    if s.write_all(req.as_bytes()).is_err() {
        return String::new();
    }
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn didl_metadata(stream_url: &str, mime: &str) -> String {
    format!(
        r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/"><item id="0" parentID="-1" restricted="1"><dc:title>Siren Follow</dc:title><upnp:class>object.item.audioItem</upnp:class><res protocolInfo="http-get:*:{mime}:*">{url}</res></item></DIDL-Lite>"#,
        mime = mime,
        url = xml_escape(stream_url),
    )
}

/// Tell a speaker to play our PCM stream. WAV first, L16 fallback.
pub fn play_stream(ip: &str, stream_url: &str) -> bool {
    if play_uri(ip, stream_url, STREAM_MIME) {
        return true;
    }
    if let Some(l16) = stream_url
        .strip_suffix(STREAM_PATH)
        .map(|base| format!("{base}{STREAM_PATH_L16}"))
    {
        return play_uri(ip, &l16, STREAM_MIME_L16);
    }
    false
}

fn play_uri(ip: &str, stream_url: &str, mime: &str) -> bool {
    let didl = xml_escape(&didl_metadata(stream_url, mime));
    let set = format!(
        r#"<u:SetAVTransportURI xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID><CurrentURI>{u}</CurrentURI><CurrentURIMetaData>{didl}</CurrentURIMetaData></u:SetAVTransportURI>"#,
        u = xml_escape(stream_url),
        didl = didl,
    );
    let r1 = soap(ip, "SetAVTransportURI", &set);
    if !r1.contains("200") {
        return false;
    }
    std::thread::sleep(Duration::from_millis(80));
    let r2 = soap(
        ip,
        "Play",
        r#"<u:Play xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID><Speed>1</Speed></u:Play>"#,
    );
    r2.contains("200")
}

pub fn stop_playback(ip: &str) -> bool {
    soap(
        ip,
        "Stop",
        r#"<u:Stop xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID></u:Stop>"#,
    )
    .contains("200")
}

pub fn pause_playback(ip: &str) -> bool {
    soap(
        ip,
        "Pause",
        r#"<u:Pause xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID></u:Pause>"#,
    )
    .contains("200")
}

pub fn resume_playback(ip: &str) -> bool {
    soap(
        ip,
        "Play",
        r#"<u:Play xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID><Speed>1</Speed></u:Play>"#,
    )
    .contains("200")
}

pub fn seek_rel(ip: &str, target: &str) -> bool {
    let inner = format!(
        r#"<u:Seek xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID><Unit>REL_TIME</Unit><Target>{}</Target></u:Seek>"#,
        xml_escape(target),
    );
    soap(ip, "Seek", &inner).contains("200")
}

/// True when the renderer is on our PCM sound-card URI.
pub fn is_follow_uri(uri: &str) -> bool {
    uri.contains(STREAM_PATH) || uri.contains(STREAM_PATH_L16)
}

/// Map UPnP CurrentTransportState onto siren's published states.
pub fn map_transport_state(upnp: &str) -> Option<&'static str> {
    match upnp.trim().to_ascii_uppercase().as_str() {
        "PLAYING" | "TRANSITIONING" => Some("play"),
        "PAUSED_PLAYBACK" | "PAUSED" => Some("pause"),
        "STOPPED" | "NO_MEDIA_PRESENT" => Some("stop"),
        _ => None,
    }
}

/// Prefer UPnP when it answers. CLI `unknown` is never published then.
pub fn publish_state(cli: Option<&str>, upnp: &str) -> Option<String> {
    if let Some(s) = map_transport_state(upnp) {
        return Some(s.to_string());
    }
    match cli.map(|s| s.trim()) {
        Some(s) if !s.is_empty() && s != "unknown" && s != "?" => Some(s.to_string()),
        _ => None,
    }
}

/// Label suffix for `now`: STOPPED drops leftover titles; follow URI
/// wins while playing; else HEOS song; else state.
pub fn now_suffix(uri: &str, song: Option<&str>, state: Option<&str>) -> String {
    if state.map(|s| s.trim()) == Some("stop") {
        return "stop".into();
    }
    if is_follow_uri(uri) {
        return "Siren Follow".into();
    }
    if let Some(s) = song {
        let t = s.trim();
        if !t.is_empty() && t != "unknown" {
            return t.to_string();
        }
    }
    state.unwrap_or("?").to_string()
}

pub fn duration_is_live(duration: &str) -> bool {
    let t = duration.trim();
    t.is_empty() || t == "00:00:00" || t == "0:00:00" || t.eq_ignore_ascii_case("NOT_IMPLEMENTED")
}

/// Live wav / zero-duration streams wedge this firmware on Seek.
pub fn should_skip_heos_seek(uri: &str, duration: &str) -> bool {
    is_follow_uri(uri) || duration_is_live(duration)
}

/// Reclaim after radio/cast Stop, or when Cave dropped the wav while
/// follow still wants play. Empty URI stays stopped. `want_play=false`
/// is `siren stop` with follow still armed — leave Cave silent.
pub fn reclaim_due(follow_file_present: bool, uri: &str, transport: &str, want_play: bool) -> bool {
    if !follow_file_present || !want_play {
        return false;
    }
    if uri.trim().is_empty() {
        return false;
    }
    matches!(
        transport.trim().to_ascii_uppercase().as_str(),
        "STOPPED" | "NO_MEDIA_PRESENT"
    )
}

pub fn follow_speaker_ip() -> Option<String> {
    load_follow()?
        .get("speaker_ip")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

pub fn follow_port() -> u16 {
    load_follow()
        .and_then(|v| v.get("port").and_then(|p| p.as_u64()))
        .unwrap_or(DEFAULT_PORT as u64) as u16
}

/// If follow.json is present, the renderer left our wav, and transport is
/// STOPPED, feed the group again. Radio/cast PLAYING is left alone.
/// `siren stop` sets want=stop so STOPPED-on-wav stays silent.
pub fn maybe_reclaim() -> bool {
    let Some(doc) = load_follow() else {
        return false;
    };
    let want_play = follow_want_play(&doc);
    let Some(ip) = follow_speaker_ip() else {
        return false;
    };
    let uri = track_uri(&ip);
    let st = transport_state(&ip);
    if !reclaim_due(true, &uri, &st, want_play) {
        return false;
    }
    follow_on(follow_port(), &ip)
}

fn follow_want_play(doc: &serde_json::Value) -> bool {
    match doc.get("want").and_then(|w| w.as_str()) {
        Some("stop") => false,
        _ => true,
    }
}

/// `siren stop` while the wav is up: keep follow.json, do not re-Play.
pub fn mark_follow_want_stop() {
    let Some(mut doc) = load_follow() else {
        return;
    };
    doc["want"] = serde_json::json!("stop");
    if let Ok(text) = serde_json::to_string(&doc) {
        let _ = std::fs::write(follow_file(), text);
    }
}

/// One-line house: armed/off, encoder, :8899, Cave GET, UPnP, want.
pub fn health_line() -> String {
    let doc = load_follow();
    let armed = doc.is_some();
    let pid = encoder_pid();
    let listen = port_open(follow_port());
    let peers = wav_peers(follow_port());
    let want = doc
        .as_ref()
        .map(|d| if follow_want_play(d) { "play" } else { "stop" })
        .unwrap_or("-");
    if !armed && pid.is_none() && !listen {
        return "follow:  off".into();
    }
    let ip = follow_speaker_ip();
    let (upnp, uri) = match ip.as_deref() {
        Some(ip) => (transport_state(ip), track_uri(ip)),
        None => (String::new(), String::new()),
    };
    let uri_short = if is_follow_uri(&uri) {
        "wav"
    } else if uri.is_empty() {
        "-"
    } else {
        uri.split('/').next_back().unwrap_or(&uri)
    };
    format!(
        "follow:  {arm}  encoder={enc}  :{port}={lis}  peers={peers}  upnp={upnp}  uri={uri_short}  want={want}",
        arm = if armed { "armed" } else { "loose" },
        enc = pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
        port = follow_port(),
        lis = if listen { "listen" } else { "down" },
        upnp = if upnp.is_empty() { "-" } else { upnp.as_str() },
    )
}

/// ESTABLISHED peers on the follow wav port (Cave GET).
pub fn wav_peers(port: u16) -> usize {
    let mut n = 0;
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(text) = std::fs::read_to_string(path) {
            n += count_estab_on_port(&text, port);
        }
    }
    n
}

/// Count ESTABLISHED rows whose local port matches (hex `IP:PORT` column).
pub fn count_estab_on_port(text: &str, port: u16) -> usize {
    let want = format!("{port:04X}");
    text.lines()
        .filter(|l| {
            let cols: Vec<&str> = l.split_whitespace().collect();
            if cols.len() < 4 {
                return false;
            }
            let local = cols[1];
            let st = cols[3];
            st.eq_ignore_ascii_case("01")
                && local
                    .rsplit_once(':')
                    .map(|(_, p)| p.eq_ignore_ascii_case(&want))
                    .unwrap_or(false)
        })
        .count()
}

/// Block until the encoder child / leftover python is gone (follow run).
pub fn wait_while_encoder() {
    loop {
        if !encoder_alive() {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// (TrackURI, RelTime, TrackDuration) from one GetPositionInfo.
pub fn position_info(ip: &str) -> (String, String, String) {
    let hay = soap(
        ip,
        "GetPositionInfo",
        r#"<u:GetPositionInfo xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID></u:GetPositionInfo>"#,
    );
    (
        xml_tag(&hay, "TrackURI"),
        xml_tag(&hay, "RelTime"),
        xml_tag(&hay, "TrackDuration"),
    )
}

/// HEOS file Seek. Skips live wav. `arg` is `+5` / `-5` / `30%` / `mm:ss`.
pub fn apply_heos_seek(ip: &str, arg: &str) -> Result<String, String> {
    let (uri, rel, dur) = position_info(ip);
    if should_skip_heos_seek(&uri, &dur) {
        return Err("seek skipped (live stream)".into());
    }
    let pos = crate::player::parse_time_token(&rel).unwrap_or(0.0);
    let d = crate::player::parse_time_token(&dur).unwrap_or(0.0);
    let next = crate::player::parse_seek(arg, pos, d).ok_or_else(|| "bad seek".to_string())?;
    let target = crate::player::fmt_rel_time(next);
    if seek_rel(ip, &target) {
        Ok(format!("seek {target}"))
    } else {
        Err("seek failed".into())
    }
}

/// Pause if PLAYING, else Play. UPnP — HEOS CLI is `unknown` on PCM follow.
pub fn toggle_follow_pause(ip: &str) -> &'static str {
    let st = transport_state(ip);
    if map_transport_state(&st) == Some("play") {
        let _ = pause_playback(ip);
        "pause"
    } else {
        let _ = resume_playback(ip);
        "play"
    }
}

fn follow_file() -> PathBuf {
    crate::paths::follow_json()
}

/// Keep a hardware prev_default across repeated `follow on`. Never store Master.
pub fn remember_prev_default(current: Option<&str>, existing: Option<&str>) -> String {
    if let Some(e) = existing {
        if !e.is_empty() && e != MASTER {
            return e.to_string();
        }
    }
    match current {
        Some(s) if !s.is_empty() && s != MASTER => s.to_string(),
        _ => FALLBACK_SINK.to_string(),
    }
}

fn follow_doc(port: u16, speaker_ip: &str, prev_default: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "port": port,
        "speaker_ip": speaker_ip,
        "prev_default": prev_default,
        "want": "play",
    })
}

fn persist_follow(port: u16, speaker_ip: &str, prev_default: Option<&str>) {
    let v = follow_doc(port, speaker_ip, prev_default);
    if let Some(parent) = follow_file().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(follow_file(), v.to_string());
}

fn load_follow() -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(follow_file()).ok()?;
    serde_json::from_str(&raw).ok()
}

fn clear_follow_file() {
    let _ = std::fs::remove_file(follow_file());
}

/// Start PCM encoder, pin mpv to Siren_Master, feed Cave. Headset default stays.
pub fn follow_on(port: u16, speaker_ip: &str) -> bool {
    let disk = load_follow();
    let existing_prev = disk
        .as_ref()
        .and_then(|v| v.get("prev_default"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .or_else(|| state().lock().unwrap().prev_default.clone());
    let current = default_sink();
    let prev = remember_prev_default(current.as_deref(), existing_prev.as_deref());
    {
        let mut st = state().lock().unwrap();
        st.prev_default = Some(prev.clone());
        st.speaker_ip = Some(speaker_ip.to_string());
    }

    crate::player::apply_follow_route(true);

    if !restart(port) {
        eprintln!("follow failed: encoder :{port}");
        crate::player::apply_follow_route(false);
        return false;
    }
    let Some(local) = crate::heos::local_ip() else {
        eprintln!("follow failed: no speaker-LAN IP");
        stop_encoder();
        crate::player::apply_follow_route(false);
        return false;
    };
    let u = url(&local, port);
    // Same URI as a leftover Stop can no-op SetAVTransportURI; Stop first
    // so Play opens a fresh HTTP GET against the new encoder.
    let _ = stop_playback(speaker_ip);
    std::thread::sleep(Duration::from_millis(120));
    if !play_stream(speaker_ip, &u) {
        eprintln!("follow failed: UPnP Play {u}");
        stop_encoder();
        crate::player::apply_follow_route(false);
        return false;
    }
    if !wait_playing(speaker_ip, 8) {
        let _ = play_stream(speaker_ip, &u);
        if !wait_playing(speaker_ip, 8) {
            eprintln!(
                "follow failed: transport {} (want PLAYING)",
                transport_state(speaker_ip)
            );
            stop_encoder();
            crate::player::apply_follow_route(false);
            return false;
        }
    }
    state().lock().unwrap().following = true;
    persist_follow(port, speaker_ip, Some(&prev));
    true
}

/// Stop feeding HEOS and restore the previous default sink.
/// Encoder stays unless `kill_encoder`.
pub fn follow_off(kill_encoder: bool) {
    let disk = load_follow();
    let (ip, prev, muted) = {
        let mut st = state().lock().unwrap();
        st.following = false;
        let ip = st.speaker_ip.take().or_else(|| {
            disk.as_ref()
                .and_then(|v| v.get("speaker_ip"))
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
        });
        let prev = st.prev_default.take().or_else(|| {
            disk.as_ref()
                .and_then(|v| v.get("prev_default"))
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
        });
        if st.muted_sink.is_none() {
            if let Some(s) = disk
                .as_ref()
                .and_then(|v| v.get("muted_sink"))
                .and_then(|x| x.as_str())
            {
                st.muted_sink = Some(s.to_string());
                st.was_muted = disk
                    .as_ref()
                    .and_then(|v| v.get("was_muted"))
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
            }
        }
        let muted = st.muted_sink.clone();
        (ip, prev, muted)
    };
    if let Some(ip) = ip {
        let _ = stop_playback(&ip);
    }
    crate::player::apply_follow_route(false);
    // Legacy: old follow stole the default sink onto Master. Headset
    // follow leaves the default alone.
    if default_sink().as_deref() == Some(MASTER) {
        let dest = follow_off_dest(prev.as_deref()).to_string();
        restore_default(&dest);
    }
    // Old follow.json muted S/PDIF; lift that if we still have the name.
    if muted.is_some() {
        unmute_follow();
    }
    if kill_encoder {
        stop_encoder();
    }
    clear_follow_file();
}

fn xml_tag(hay: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let Some(start) = hay.find(&open) else {
        return String::new();
    };
    let rest = &hay[start + open.len()..];
    let Some(end) = rest.find(&close) else {
        return String::new();
    };
    rest[..end].trim().to_string()
}

/// What the speaker reports as its current TrackURI (empty on failure).
pub fn track_uri(ip: &str) -> String {
    xml_tag(
        &soap(
            ip,
            "GetPositionInfo",
            r#"<u:GetPositionInfo xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID></u:GetPositionInfo>"#,
        ),
        "TrackURI",
    )
}

pub fn transport_state(ip: &str) -> String {
    xml_tag(
        &soap(
            ip,
            "GetTransportInfo",
            r#"<u:GetTransportInfo xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID></u:GetTransportInfo>"#,
        ),
        "CurrentTransportState",
    )
}

fn is_playing(state: &str) -> bool {
    // Honesty already maps TRANSITIONING → play. Cave sits there for
    // seconds while the wav GET starts; treating it as failure killed
    // the encoder before the first byte.
    map_transport_state(state) == Some("play")
}

fn wait_playing(ip: &str, attempts: u32) -> bool {
    for _ in 0..attempts {
        if is_playing(&transport_state(ip)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

fn follow_off_dest(prev: Option<&str>) -> &str {
    match prev {
        Some(s) if !s.is_empty() => s,
        _ => FALLBACK_SINK,
    }
}

pub fn default_sink() -> Option<String> {
    let out = Command::new("pactl").arg("get-default-sink").output().ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn set_sink_mute(name: &str, mute: bool) -> bool {
    let flag = if mute { "1" } else { "0" };
    let a = Command::new("pactl")
        .args(["set-sink-mute", name, flag])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let b = Command::new("wpctl")
        .args(["set-mute", "@DEFAULT_AUDIO_SINK@", flag])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    a || b
}

fn restore_default(name: &str) {
    let _ = set_default_sink(name);
    move_sink_inputs_to(name);
}

fn set_default_sink(name: &str) -> bool {
    // pactl names, never pactl indexes fed to wpctl (IEC958 is 57 vs 56).
    Command::new("pactl")
        .args(["set-default-sink", name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn move_sink_inputs_to(sink: &str) {
    let out = Command::new("pactl")
        .args(["list", "short", "sink-inputs"])
        .output();
    let Ok(out) = out else { return };
    let text = String::from_utf8_lossy(&out.stdout);
    for id in sink_input_ids(&text) {
        let _ = Command::new("pactl")
            .args(["move-sink-input", id, sink])
            .status();
    }
}

fn sink_input_ids(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|l| {
            let id = l.split_whitespace().next()?;
            if id.is_empty() {
                None
            } else {
                Some(id)
            }
        })
        .collect()
}

fn parse_sink_id(text: &str, name: &str) -> Option<u32> {
    for l in text.lines() {
        let mut cols = l.split_whitespace();
        let id = cols.next()?.parse::<u32>().ok()?;
        let n = cols.next()?;
        if n == name {
            return Some(id);
        }
    }
    None
}

fn sink_id(name: &str) -> Option<u32> {
    let out = Command::new("pactl")
        .args(["list", "short", "sinks"])
        .output()
        .ok()?;
    parse_sink_id(&String::from_utf8_lossy(&out.stdout), name)
}

fn count_clients_on(text: &str, sink: usize) -> usize {
    text.lines()
        .filter(|l| {
            l.split_whitespace()
                .nth(1)
                .and_then(|s| s.parse::<usize>().ok())
                == Some(sink)
        })
        .count()
}

fn unmute_follow() {
    let (sink, was) = {
        let mut st = state().lock().unwrap();
        (st.muted_sink.take(), st.was_muted)
    };
    if let Some(sink) = sink {
        if !was {
            let _ = set_sink_mute(&sink, false);
        }
    }
}

/// Is Siren_Master the current default sink? (reads pactl)
pub fn master_is_default() -> bool {
    default_sink().as_deref() == Some(MASTER)
}

/// Count of live clients feeding Siren_Master (reads pactl).
pub fn master_clients() -> usize {
    let out = Command::new("pactl")
        .args(["list", "short", "sink-inputs"])
        .output();
    let Ok(out) = out else { return 0 };
    let text = String::from_utf8_lossy(&out.stdout);
    let sink = master_sink_id().unwrap_or(usize::MAX);
    count_clients_on(&text, sink)
}

fn master_sink_id() -> Option<usize> {
    let out = Command::new("pactl")
        .args(["list", "short", "sinks"])
        .output()
        .ok()?;
    parse_sink_id(&String::from_utf8_lossy(&out.stdout), MASTER).map(|id| id as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_shapes_correctly() {
        assert_eq!(
            url("192.168.8.186", 8899),
            "http://192.168.8.186:8899/siren.wav"
        );
        assert_eq!(
            url_l16("192.168.8.186", 8899),
            "http://192.168.8.186:8899/siren.l16"
        );
    }

    #[test]
    fn chunk_is_tens_of_ms_not_seconds() {
        // 48 kHz s16le stereo = 192000 bytes/s. 4096 bytes ≈ 21 ms.
        let bytes_per_sec = SAMPLE_RATE as usize * 2 * 2;
        let ms = CHUNK_BYTES * 1000 / bytes_per_sec;
        assert!(ms < 50, "chunk {CHUNK_BYTES} = {ms}ms, want <50ms");
        assert!(CHUNK_BYTES < 16_384);
    }

    #[test]
    fn didl_names_wav_protocol() {
        let d = didl_metadata("http://192.168.8.186:8899/siren.wav", STREAM_MIME);
        assert!(d.contains("audio/wav"));
        assert!(d.contains("Siren Follow"));
        assert!(d.contains("/siren.wav"));
    }

    #[test]
    fn monitor_of_includes_master() {
        assert_eq!(monitor_of(MASTER), "Siren_Master.monitor");
        assert_eq!(
            monitor_of(FALLBACK_SINK),
            "alsa_output.pci-0000_31_00.4.iec958-stereo.monitor"
        );
    }

    #[test]
    fn remember_prev_keeps_hardware() {
        assert_eq!(
            remember_prev_default(Some(FALLBACK_SINK), None),
            FALLBACK_SINK
        );
        assert_eq!(
            remember_prev_default(Some(MASTER), Some(FALLBACK_SINK)),
            FALLBACK_SINK
        );
        assert_eq!(remember_prev_default(Some(MASTER), None), FALLBACK_SINK);
        assert_eq!(
            remember_prev_default(Some(MASTER), Some(MASTER)),
            FALLBACK_SINK
        );
    }

    #[test]
    fn follow_doc_keeps_prev_default() {
        let v = follow_doc(8899, "192.168.8.187", Some(FALLBACK_SINK));
        assert_eq!(v["prev_default"], FALLBACK_SINK);
        assert_eq!(v["speaker_ip"], "192.168.8.187");
        assert_eq!(v["port"], 8899);
        assert_eq!(v["want"], "play");
    }

    #[test]
    fn xml_tag_reads_transport_state() {
        let hay = r#"<s:Body><CurrentTransportState>PLAYING</CurrentTransportState></s:Body>"#;
        assert_eq!(xml_tag(hay, "CurrentTransportState"), "PLAYING");
        assert_eq!(xml_tag(hay, "TrackURI"), "");
    }

    #[test]
    fn encoder_pid_scan_ignores_shell_minus_c() {
        let zsh: Vec<&[u8]> = b"zsh\0-c\0pgrep -af siren-stream\0"
            .split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .collect();
        assert!(!cmdline_is_encoder(&zsh));
        let py: Vec<&[u8]> =
            b"/usr/bin/python3\0/home/evenweaker/.local/lib/faeos/siren-stream\08899\0Siren_Master.monitor\0"
                .split(|b| *b == 0)
                .filter(|a| !a.is_empty())
                .collect();
        assert!(cmdline_is_encoder(&py));
        let embedded: Vec<&[u8]> = b"python3\0-c\0from http.server import ThreadingHTTPServer\0"
            .split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .collect();
        assert!(!cmdline_is_encoder(&embedded));
        let empty: Vec<&[u8]> = vec![];
        assert!(!cmdline_is_encoder(&empty));
        let ffmpeg: Vec<&[u8]> = b"ffmpeg\0-f\0pulse\0-i\0Siren_Master.monitor\0"
            .split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .collect();
        assert!(!cmdline_is_encoder(&ffmpeg));
    }

    #[test]
    fn capture_from_default_includes_master() {
        assert_eq!(
            capture_from_default(Some(MASTER)),
            "Siren_Master.monitor"
        );
        assert_eq!(
            capture_from_default(Some(FALLBACK_SINK)),
            "alsa_output.pci-0000_31_00.4.iec958-stereo.monitor"
        );
        assert_eq!(capture_from_default(None), "Siren_Master.monitor");
        assert_eq!(capture_from_default(Some("")), "Siren_Master.monitor");
    }

    #[test]
    fn remember_prev_empty_falls_back() {
        assert_eq!(remember_prev_default(None, None), FALLBACK_SINK);
        assert_eq!(remember_prev_default(Some(""), None), FALLBACK_SINK);
        assert_eq!(
            remember_prev_default(Some(FALLBACK_SINK), Some("")),
            FALLBACK_SINK
        );
    }

    #[test]
    fn follow_off_dest_falls_back() {
        assert_eq!(follow_off_dest(Some(FALLBACK_SINK)), FALLBACK_SINK);
        assert_eq!(follow_off_dest(None), FALLBACK_SINK);
        assert_eq!(follow_off_dest(Some("")), FALLBACK_SINK);
        assert_eq!(follow_off_dest(Some("other-sink")), "other-sink");
    }

    #[test]
    fn is_playing_is_case_insensitive() {
        assert!(is_playing("PLAYING"));
        assert!(is_playing("playing"));
        assert!(is_playing("TRANSITIONING"));
        assert!(!is_playing("STOPPED"));
        assert!(!is_playing(""));
        assert!(!is_playing("PAUSED_PLAYBACK"));
    }

    #[test]
    fn set_default_sink_uses_pactl_name_not_wpctl_index() {
        let src = std::fs::read_to_string("src/stream.rs").expect("stream.rs");
        let start = src.find("fn set_default_sink").expect("set_default_sink");
        let end = src[start..]
            .find("\nfn move_sink_inputs_to")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(body.contains("set-default-sink"), "pactl by name");
        assert!(
            !body.contains("set-default\""),
            "pactl indexes (IEC958 57) must not be fed to wpctl set-default"
        );
    }

    #[test]
    fn encoder_start_captures_master() {
        let src = std::fs::read_to_string("src/stream.rs").expect("stream.rs");
        let start = src.find("pub fn start(port: u16)").expect("start");
        let end = src[start..]
            .find("\n/// Kill the encoder")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains("monitor_of(MASTER)"),
            "encoder captures Siren_Master, not the headset default"
        );
    }

    #[test]
    fn parse_pactl_short_sinks() {
        let text = "\
31\tSiren_Master\tPipeWire\tfloat32le 2ch 48000Hz\tSUSPENDED
57\talsa_output.pci-0000_31_00.4.iec958-stereo\tPipeWire\ts32le 2ch 48000Hz\tSUSPENDED
";
        assert_eq!(parse_sink_id(text, MASTER), Some(31));
        assert_eq!(parse_sink_id(text, FALLBACK_SINK), Some(57));
        assert_eq!(parse_sink_id(text, "nope"), None);
    }

    #[test]
    fn parse_pactl_short_sink_inputs() {
        let text = "\
248\t31\t47\tPipeWire\ts16le 2ch 48000Hz
299\t57\t47\tPipeWire\ts16le 2ch 48000Hz
";
        assert_eq!(sink_input_ids(text), vec!["248", "299"]);
        assert_eq!(count_clients_on(text, 31), 1);
        assert_eq!(count_clients_on(text, 57), 1);
        assert_eq!(count_clients_on(text, 99), 0);
        assert!(sink_input_ids("").is_empty());
    }

    #[test]
    fn xml_tag_reads_track_uri() {
        let hay = concat!(
            r#"<TrackURI>http://192.168.8.186:8899/siren.wav</TrackURI>"#,
            r#"<RelTime>00:00:09</RelTime>"#,
        );
        assert_eq!(
            xml_tag(hay, "TrackURI"),
            "http://192.168.8.186:8899/siren.wav"
        );
        assert_eq!(xml_tag(hay, "RelTime"), "00:00:09");
        assert_eq!(xml_tag("no tags", "TrackURI"), "");
    }

    #[test]
    fn follow_doc_null_prev_is_json_null() {
        let v = follow_doc(8899, "192.168.8.187", None);
        assert!(v["prev_default"].is_null());
    }

    #[test]
    fn capture_source_does_not_skip_master() {
        let src = std::fs::read_to_string("src/stream.rs").expect("stream.rs");
        assert!(
            !src.contains("if sink != \"Siren_Master\""),
            "capture_source must not skip Siren_Master"
        );
        assert!(
            src.contains("capture_from_default"),
            "capture_source must go through capture_from_default"
        );
    }

    #[test]
    fn encoder_stop_does_not_pkill_substring() {
        let src = std::fs::read_to_string("src/stream.rs").expect("stream.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .unwrap_or(&src);
        for (i, line) in prod.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") || t.starts_with("///") {
                continue;
            }
            assert!(
                !(t.contains("pkill") && t.contains("siren-stream")),
                "pkill -f would SIGTERM agent shells at stream.rs:{}",
                i + 1
            );
        }
    }

    #[test]
    fn follow_on_confirms_playing() {
        let src = std::fs::read_to_string("src/stream.rs").expect("stream.rs");
        let start = src.find("pub fn follow_on").expect("follow_on");
        let end = src[start..]
            .find("\npub fn follow_off")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(body.contains("stop_playback"), "Stop before Play so leftover URI re-GETs");
        assert!(body.contains("wait_playing"), "must wait for PLAYING");
        assert!(
            body.contains("apply_follow_route(true)"),
            "pin mpv to Siren_Master"
        );
        assert!(
            !body.contains("set_default_sink(MASTER)"),
            "follow must not steal the headset default"
        );
        assert!(body.contains("persist_follow"), "persist prev_default");
        assert!(
            body.contains("follow failed:"),
            "fail path names the step"
        );
    }

    #[test]
    fn follow_unit_tracks_encoder() {
        let unit = std::fs::read_to_string("systemd/siren-follow.service").expect("unit");
        assert!(unit.contains("Type=simple"));
        assert!(!unit.contains("RemainAfterExit"));
        assert!(!unit.contains("KillMode=none"));
        assert!(unit.contains("ExecStart=%h/bin/siren audio follow run"));
        assert!(unit.contains("ExecStop=%h/bin/siren audio follow off"));
        assert!(unit.contains("After=pipewire.service"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn build_sh_restarts_follow_unit() {
        let sh = std::fs::read_to_string("build.sh").expect("build.sh");
        assert!(
            sh.contains("install --house") || sh.contains("--house"),
            "follow unit only on house install"
        );
        assert!(
            sh.contains("systemctl --user restart siren-follow.service"),
            "RemainAfterExit start is a no-op; house install must restart"
        );
        assert!(sh.contains("systemctl --user enable siren-follow.service"));
        assert!(
            !sh.contains("enable --now siren-follow"),
            "enable --now skips ExecStart when already active"
        );
    }

    #[test]
    fn is_follow_uri_matches_wav_and_l16() {
        assert!(is_follow_uri("http://192.168.8.186:8899/siren.wav"));
        assert!(is_follow_uri("http://192.168.8.186:8899/siren.l16"));
        assert!(!is_follow_uri("http://proic1.evspt.com/oxigenio_aac"));
        assert!(!is_follow_uri(""));
    }

    #[test]
    fn publish_state_prefers_upnp_over_cli_unknown() {
        assert_eq!(publish_state(Some("unknown"), "PLAYING").as_deref(), Some("play"));
        assert_eq!(publish_state(Some("unknown"), "TRANSITIONING").as_deref(), Some("play"));
        assert_eq!(publish_state(Some("play"), "PAUSED_PLAYBACK").as_deref(), Some("pause"));
        assert_eq!(publish_state(Some("unknown"), "STOPPED").as_deref(), Some("stop"));
        assert_eq!(publish_state(Some("unknown"), ""), None);
        assert_eq!(publish_state(Some("play"), "").as_deref(), Some("play"));
    }

    #[test]
    fn now_suffix_follow_uri_wins() {
        assert_eq!(
            now_suffix("http://192.168.8.186:8899/siren.wav", Some("other"), Some("play")),
            "Siren Follow"
        );
        assert_eq!(now_suffix("http://x/radio", Some("OXIGÉNIO"), Some("play")), "OXIGÉNIO");
        assert_eq!(now_suffix("", None, Some("play")), "play");
        assert_eq!(now_suffix("", None, None), "?");
        assert_eq!(
            now_suffix("http://x/radio", Some("102nd Old Time Music Hour"), Some("stop")),
            "stop",
            "STOPPED drops leftover TuneIn title"
        );
        assert_eq!(
            now_suffix("http://192.168.8.186:8899/siren.wav", Some("other"), Some("stop")),
            "stop"
        );
    }

    #[test]
    fn skip_heos_seek_on_live_wav_or_zero_duration() {
        assert!(should_skip_heos_seek("http://192.168.8.186:8899/siren.wav", "00:18:20"));
        assert!(should_skip_heos_seek("http://dlna/track.mp3", "00:00:00"));
        assert!(should_skip_heos_seek("http://dlna/track.mp3", ""));
        assert!(!should_skip_heos_seek("http://dlna/track.mp3", "00:03:12"));
    }

    #[test]
    fn reclaim_due_only_when_stopped_on_foreign_uri() {
        assert!(!reclaim_due(false, "http://x/radio", "STOPPED", true));
        assert!(!reclaim_due(true, "", "STOPPED", true));
        assert!(!reclaim_due(true, "http://x/radio", "PLAYING", true));
        assert!(reclaim_due(true, "http://x/radio", "STOPPED", true));
        assert!(
            reclaim_due(true, "http://192.168.8.186:8899/siren.wav", "STOPPED", true),
            "Cave dropped the wav while want=play"
        );
        assert!(
            !reclaim_due(true, "http://192.168.8.186:8899/siren.wav", "STOPPED", false),
            "siren stop sets want=stop"
        );
    }

    #[test]
    fn count_estab_on_port_reads_proc_net_tcp() {
        let sample = "\
  sl  local_address rem_address   st tx_queue\n\
   0: 0100007F:22C3 00000000:0000 0A 00000000:00000000\n\
   1: 0100A8C0:22C3 3300A8C0:BEEF 01 00000000:00000000\n\
   2: 0100A8C0:1F90 3300A8C0:BEEF 01 00000000:00000000\n";
        assert_eq!(count_estab_on_port(sample, 8899), 1);
        assert_eq!(count_estab_on_port(sample, 8080), 1);
        assert_eq!(count_estab_on_port(sample, 1255), 0);
    }

    #[test]
    fn now_snapshot_uses_upnp_when_following() {
        let src = std::fs::read_to_string("src/main.rs").expect("main.rs");
        let start = src.find("fn now_snapshot").expect("now_snapshot");
        let end = src[start..]
            .find("\nfn with_deadline")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(body.contains("publish_state"), "now must publish UPnP over CLI unknown");
        assert!(body.contains("now_suffix"), "follow URI labels Siren Follow");
        assert!(body.contains("transport_state"), "UPnP transport is the honesty path");
        assert!(
            body.contains("following() || cfg.audio_output == \"heos\""),
            "local + follow still uses Cave UPnP"
        );
        let cached_end = src
            .find("fn now_snapshot(")
            .unwrap_or(src.len());
        let cached = &src[src.find("fn now_snapshot_cached").unwrap_or(0)..cached_end];
        assert!(
            cached.contains("following()"),
            "cached now must not recurse into live now when follow is on"
        );
        assert!(
            !cached.contains("now_snapshot(cfg)")
                || cached.find("following()").unwrap() < cached.rfind("now_snapshot(cfg)").unwrap(),
            "follow branch in cached now returns before calling now_snapshot"
        );
    }

    #[test]
    fn agents_names_minidlna_sandbox() {
        let src = std::fs::read_to_string("AGENTS.md").expect("AGENTS");
        assert!(src.contains("ProtectHome=yes"), "minidlna hides ~/Music");
        assert!(src.contains("minidlna.service.d"), "house drop-in path");
        assert!(src.contains("/run/minidlna/music"), "bind dest is outside /home");
        assert!(
            src.contains("8899/tcp"),
            "UFW follow wav from speaker LAN"
        );
    }

    #[test]
    fn living_docs_follow_is_music_only() {
        for path in ["README.md", "AGENTS.md", "docs/AUDIO.md"] {
            let src = std::fs::read_to_string(path).unwrap_or_else(|_| panic!("{path}"));
            assert!(
                !src.contains("restores S/PDIF"),
                "{path}: follow off leaves the headset default"
            );
            assert!(
                !src.contains("speakers as PC output"),
                "{path}: old steal-the-default copy"
            );
            assert!(
                !src.contains("Siren_Master default → HEOS"),
                "{path}: follow does not steal the default sink"
            );
        }
        let audio = std::fs::read_to_string("docs/AUDIO.md").expect("AUDIO.md");
        assert!(audio.contains("heos.json"), "Ether file cache");
        assert!(audio.contains(":8899"), "follow wav port");
        assert!(audio.contains("headset"), "headset layer named");
    }

    #[test]
    fn dhcpd_offers_dns_option() {
        let src = std::fs::read_to_string("scripts/siren-heos-dhcpd.py").expect("dhcpd");
        assert!(
            src.contains("bytes([6, 8])"),
            "DHCP option 6 so Cave UPnP can resolve"
        );
    }

    #[test]
    fn forward_unit_inserts_before_ufw_drop() {
        let sh = std::fs::read_to_string("scripts/siren-heos-forward").expect("forward");
        let unit = std::fs::read_to_string("systemd/siren-heos-forward.service").expect("unit");
        assert!(sh.contains("MASQUERADE"), "NAT out the WAN iface");
        assert!(sh.contains("-I FORWARD 1"), "ACCEPT before UFW DROP");
        assert!(sh.contains("192.168.11.0/24"), "speaker LAN");
        assert!(unit.contains("Type=oneshot"));
        assert!(unit.contains("RemainAfterExit=yes"));
        assert!(unit.contains("siren-heos-forward start"));
    }

    #[test]
    fn pause_and_seek_skip_live_stream() {
        let src = std::fs::read_to_string("src/main.rs").expect("main.rs");
        assert!(src.contains("fn cmd_pause"));
        assert!(src.contains("toggle_follow_pause"));
        assert!(src.contains("fn cmd_seek"));
        assert!(src.contains("seek skipped (live stream)"));
        assert!(src.contains("maybe_reclaim"));
        let vol = src.find("Some(AudioCmd::Vol").expect("Vol");
        let vol_end = src[vol..]
            .find("Some(AudioCmd::Queue")
            .map(|i| vol + i)
            .unwrap_or(src.len());
        let body = &src[vol..vol_end];
        assert!(body.contains("parse_vol_arg"), "vol accepts +N/-N");
        assert!(body.contains("apply_volume"), "following uses group volume");
        assert!(body.contains("following()"), "vol route checks follow");
    }
}
