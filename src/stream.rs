//! Siren stream — one live output for the whole machine.
//!
//! Pipeline: every client → Siren_Master (null sink) → monitor capture →
//! ffmpeg mp3 → HTTP on `stream_port` → HEOS group leader pulls once,
//! members follow via group sync. No TuneIn, no Denon servers.
//!
//! UPnP uses raw TcpStream (std only, no new deps) — same pattern as
//! the HEOS connection pool. The encoder+server is a single child
//! process owned by siren.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 8899;
pub const STREAM_PATH: &str = "/siren.mp3";

/// Capture source for the stream. We capture the REAL sink's monitor
/// (not Siren_Master's) so local listening stays low-latency while the
/// HEOS path runs on its own clock. Resolved at runtime because sink
/// names can shift; falls back to Siren_Master if the real sink is gone.
pub fn capture_source() -> String {
    let out = std::process::Command::new("pactl")
        .args(["list", "short", "sinks"])
        .output();
    if let Ok(out) = out {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let mut cols = line.split_whitespace();
            let _id = cols.next();
            let name = cols.next().unwrap_or("");
            if name != "Siren_Master" && name.contains("alsa_output") {
                return format!("{name}.monitor");
            }
        }
    }
    // Fallback: Siren_Master's monitor
    "Siren_Master.monitor".to_string()
}

#[allow(dead_code)]
pub const MONITOR: &str = "Siren_Master.monitor";

static CHILD: OnceLock<Mutex<Option<std::process::Child>>> = OnceLock::new();

fn child() -> &'static Mutex<Option<std::process::Child>> {
    CHILD.get_or_init(|| Mutex::new(None))
}

/// Is our encoder+server child alive?
pub fn running() -> bool {
    match child().lock().unwrap().as_mut() {
        Some(c) => c.try_wait().ok().flatten().is_none(),
        None => false,
    }
}

/// Stream URL for a given local IP and port.
pub fn url(local_ip: &str, port: u16) -> String {
    format!("http://{local_ip}:{port}{STREAM_PATH}")
}

/// Start the encoder+server: ffmpeg captures the Siren_Master monitor,
/// encodes mp3, and serves it on `port`. Returns false if already running
/// or if spawn fails.
pub fn start(port: u16) -> bool {
    if running() {
        return true;
    }
    // single shell pipeline: ffmpeg mp3 to stdout → python HTTP server
    // reads stdin? No — python server spawns its own ffmpeg per client.
    // Here we just launch the server script; it spawns ffmpeg per GET.
    let script = server_script(port);
    let r = std::process::Command::new("python3")
        .args(["-c", &script])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn();
    match r {
        Ok(c) => {
            *child().lock().unwrap() = Some(c);
            // wait for the port to answer
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

/// Stop the encoder+server.
pub fn stop() {
    if let Some(mut c) = child().lock().unwrap().take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_millis(300),
    )
    .is_ok()
}

/// The HTTP server, embedded as a python one-liner host.
/// Spawns one ffmpeg per GET (monitor → mp3 → socket).
fn server_script(port: u16) -> String {
    let cap = capture_source();
    format!(
        r#"
import subprocess
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
CAP = "{cap}"
FF = ["ffmpeg","-hide_banner","-loglevel","error","-f","pulse","-i",CAP,"-ac","2","-ar","44100","-codec:a","libmp3lame","-b:a","128k","-f","mp3","-"]
class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def do_GET(self):
        if self.path != "{STREAM_PATH}":
            self.send_response(404); self.end_headers(); return
        self.send_response(200)
        self.send_header("Content-Type", "audio/mpeg")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        try:
            p = subprocess.Popen(FF, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            while True:
                chunk = p.stdout.read(65536)
                if not chunk: break
                try: self.wfile.write(chunk)
                except (BrokenPipeError, ConnectionResetError): break
            p.terminate()
        except Exception: pass
    def do_HEAD(self):
        if self.path != "{STREAM_PATH}":
            self.send_response(404); self.end_headers(); return
        self.send_response(200)
        self.send_header("Content-Type", "audio/mpeg")
        self.end_headers()
ThreadingHTTPServer(("0.0.0.0", {port}), H).serve_forever()
"#
    )
}

/// POST a SOAP action to a speaker's UPnP AVTransport endpoint.
/// Returns the raw HTTP response body (empty on failure).
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

/// Tell a speaker (usually the group leader) to play our stream.
/// Returns true when both SetAVTransportURI and Play return HTTP 200.
pub fn play_stream(ip: &str, stream_url: &str) -> bool {
    let set = format!(
        r#"<u:SetAVTransportURI xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID><CurrentURI>{u}</CurrentURI><CurrentURIMetaData></CurrentURIMetaData></u:SetAVTransportURI>"#,
        u = stream_url,
    );
    let r1 = soap(ip, "SetAVTransportURI", &set);
    if !r1.contains("200") {
        return false;
    }
    std::thread::sleep(Duration::from_secs(1));
    let r2 = soap(
        ip,
        "Play",
        r#"<u:Play xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID><Speed>1</Speed></u:Play>"#,
    );
    r2.contains("200")
}

/// What the speaker reports as its current TrackURI (empty on failure).
pub fn track_uri(ip: &str) -> String {
    let r = soap(
        ip,
        "GetPositionInfo",
        r#"<u:GetPositionInfo xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID></u:GetPositionInfo>"#,
    );
    let Some(start) = r.find("<TrackURI>") else {
        return String::new();
    };
    let rest = &r[start + 11..];
    let Some(end) = rest.find("</TrackURI>") else {
        return String::new();
    };
    // TrackURI may itself contain tags; take the raw text
    rest[..end].trim().to_string()
}

/// Is Siren_Master the current default sink? (reads pactl)
pub fn master_is_default() -> bool {
    std::process::Command::new("pactl")
        .arg("get-default-sink")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "Siren_Master")
        .unwrap_or(false)
}

/// Count of live clients feeding Siren_Master (reads pactl).
pub fn master_clients() -> usize {
    let out = std::process::Command::new("pactl")
        .args(["list", "short", "sink-inputs"])
        .output();
    let Ok(out) = out else { return 0 };
    let text = String::from_utf8_lossy(&out.stdout);
    // sink-input lines: "<id> <sink-id> <client> ..."
    // Siren_Master is sink 157 — but ids shift; match by resolving the name
    let sink_id = master_sink_id().unwrap_or(usize::MAX);
    text.lines()
        .filter(|l| {
            l.split_whitespace()
                .nth(1)
                .and_then(|s| s.parse::<usize>().ok())
                == Some(sink_id)
        })
        .count()
}

fn master_sink_id() -> Option<usize> {
    let out = std::process::Command::new("pactl")
        .args(["list", "short", "sinks"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for l in text.lines() {
        let mut cols = l.split_whitespace();
        let id = cols.next()?.parse::<usize>().ok()?;
        let name = cols.next()?;
        if name == "Siren_Master" {
            return Some(id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_shapes_correctly() {
        assert_eq!(
            url("192.168.8.186", 8899),
            "http://192.168.8.186:8899/siren.mp3"
        );
    }
}
