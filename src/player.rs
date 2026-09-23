//! mpv IPC client — mirrors Python `Player` (`faeOS/bin/siren`).
//!
//! Protocol: one JSON `{"command": [...]}` line per Unix-socket
//! connection, short reply read. `get` never spawns mpv.

use crate::config::SirenConfig;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static SPAWN_ATTEMPTED: AtomicBool = AtomicBool::new(false);

pub fn sock_path() -> String {
    std::env::var("SIREN_SOCK").unwrap_or_else(|_| "/tmp/siren-mpv.sock".into())
}

pub fn alive() -> bool {
    std::path::Path::new(&sock_path()).exists()
}

/// Start idle mpv once per process (mirrors `Player.spawn` + prefs).
pub fn spawn(cfg: &SirenConfig) -> bool {
    if alive() {
        return true;
    }
    if SPAWN_ATTEMPTED.swap(true, Ordering::SeqCst) {
        return false;
    }
    let sock = sock_path();
    let gapless = if cfg.gapless { "yes" } else { "no" };
    let r = std::process::Command::new("mpv")
        .args([
            "--idle=yes",
            "--no-video",
            "--audio-display=no",
            "--gapless-audio=yes",
            "--volume-max=150",
            &format!("--input-ipc-server={sock}"),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn();
    if r.is_err() {
        return false;
    }
    // detach: Command without wait drops the handle; child keeps running
    std::mem::forget(r);
    for _ in 0..20 {
        if alive() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !alive() {
        return false;
    }
    std::thread::sleep(Duration::from_millis(150));
    apply_runtime_prefs(cfg);
    let _ = gapless;
    true
}

pub fn apply_runtime_prefs(cfg: &SirenConfig) {
    send(&json!(["set_property", "gapless-audio", if cfg.gapless { "yes" } else { "no" }]));
    send(&json!([
        "set_property",
        "replaygain-mode",
        if cfg.normalize { "track" } else { "off" }
    ]));
    if alive() {
        send(&json!(["set_property", "volume", cfg.default_volume]));
    }
}

/// Send one command. True on any reply (mirrors `Player.send`).
pub fn send(argv: &Value) -> bool {
    let cfg = SirenConfig::load();
    if !spawn(&cfg) {
        return false;
    }
    let mut stream = match UnixStream::connect(sock_path()) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let payload = format!("{}\n", json!({ "command": argv }));
    if stream.write_all(payload.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; 1024];
    matches!(stream.read(&mut buf), Ok(n) if n > 0)
}

/// Read one property. Never spawns mpv (mirrors `Player.get`).
pub fn get(prop: &str) -> Option<Value> {
    if !alive() {
        return None;
    }
    let mut stream = UnixStream::connect(sock_path()).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok()?;
    let payload = format!("{}\n", json!({ "command": ["get_property", prop] }));
    stream.write_all(payload.as_bytes()).ok()?;
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                data.extend_from_slice(&buf[..n]);
                if data.contains(&b'\n') {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&data);
    for line in text.lines() {
        let res: Value = serde_json::from_str(line).unwrap_or(Value::Null);
        if res.get("error").and_then(|e| e.as_str()) == Some("success") {
            return res.get("data").cloned();
        }
    }
    None
}

pub fn get_bool(prop: &str, default: bool) -> bool {
    get(prop).and_then(|v| v.as_bool()).unwrap_or(default)
}

pub fn get_f64(prop: &str) -> f64 {
    get(prop).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

pub fn get_string(prop: &str) -> String {
    get(prop)
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// Current file path, `file://` prefix stripped (mirrors `now_path`).
pub fn now_path() -> String {
    let p = get_string("path");
    if let Some(stripped) = p.strip_prefix("file://") {
        return stripped.to_string();
    }
    p
}
