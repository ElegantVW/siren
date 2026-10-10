//! mpv IPC client — mirrors Python `Player` (`faeOS/bin/siren`).
//!
//! Protocol: one JSON `{"command": [...]}` line per connection.
//! Unix: socket. Windows: named pipe. `get` never spawns mpv.

use crate::config::SirenConfig;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static SPAWN_ATTEMPTED: AtomicBool = AtomicBool::new(false);

pub fn sock_path() -> String {
    crate::paths::mpv_ipc()
}

#[cfg(unix)]
fn connect() -> Option<std::os::unix::net::UnixStream> {
    let s = std::os::unix::net::UnixStream::connect(sock_path()).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = s.set_write_timeout(Some(Duration::from_millis(500)));
    Some(s)
}

#[cfg(windows)]
fn connect() -> Option<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(sock_path())
        .ok()
}

pub fn alive() -> bool {
    #[cfg(unix)]
    {
        std::path::Path::new(&sock_path()).exists() || connect().is_some()
    }
    #[cfg(windows)]
    {
        connect().is_some()
    }
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
    let mut cmd = std::process::Command::new("mpv");
    cmd.args([
        "--idle=yes",
        "--no-video",
        "--audio-display=no",
        &format!("--gapless-audio={gapless}"),
        "--volume-max=150",
        &format!("--input-ipc-server={sock}"),
    ])
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let r = cmd.spawn();
    if r.is_err() {
        return false;
    }
    // detach: Command without wait drops the handle; child keeps running
    std::mem::forget(r);
    for _ in 0..40 {
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
        apply_follow_route(crate::paths::follow_json().is_file());
    }
}

/// Pin mpv to Siren_Master (follow music → Cave) or `auto` (headset default).
pub fn apply_follow_route(on: bool) {
    let dev = if on {
        "pulse/Siren_Master"
    } else {
        "auto"
    };
    let _ = send(&json!(["set_property", "audio-device", dev]));
}

/// Send one command. True on any reply (mirrors `Player.send`).
pub fn send(argv: &Value) -> bool {
    let cfg = SirenConfig::load();
    if !spawn(&cfg) {
        return false;
    }
    let mut stream = match connect() {
        Some(s) => s,
        None => return false,
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
    let mut stream = connect()?;
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

/// Seconds from `90`, `1:30`, or `00:01:30`.
pub fn parse_time_token(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        match parts.as_slice() {
            [m, sec] => {
                let m: f64 = m.parse().ok()?;
                let sec: f64 = sec.parse().ok()?;
                Some(m * 60.0 + sec)
            }
            [h, m, sec] => {
                let h: f64 = h.parse().ok()?;
                let m: f64 = m.parse().ok()?;
                let sec: f64 = sec.parse().ok()?;
                Some(h * 3600.0 + m * 60.0 + sec)
            }
            _ => None,
        }
    } else {
        s.parse().ok()
    }
}

pub fn fmt_rel_time(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

/// `+5` / `-5` / `30%` / `1:30` / `90` → absolute seconds.
pub fn parse_seek(arg: &str, pos: f64, dur: f64) -> Option<f64> {
    let a = arg.trim();
    if a.is_empty() {
        return None;
    }
    let clamp = |x: f64| {
        if dur > 0.0 {
            x.clamp(0.0, dur)
        } else {
            x.max(0.0)
        }
    };
    if let Some(rest) = a.strip_suffix('%') {
        let pct: f64 = rest.trim().parse().ok()?;
        if dur > 0.0 {
            return Some((dur * pct / 100.0).clamp(0.0, dur));
        }
        return Some(0.0);
    }
    if let Some(rest) = a.strip_prefix('+') {
        return Some(clamp(pos + parse_time_token(rest)?));
    }
    if let Some(rest) = a.strip_prefix('-') {
        return Some(clamp(pos - parse_time_token(rest)?));
    }
    Some(clamp(parse_time_token(a)?))
}

pub fn apply_local_seek(arg: &str) -> Result<String, String> {
    let pos = get_f64("time-pos");
    let dur = get_f64("duration");
    let next = parse_seek(arg, pos, dur).ok_or_else(|| "bad seek".to_string())?;
    if send(&serde_json::json!(["set_property", "time-pos", next])) {
        Ok(format!("seek {}", fmt_rel_time(next)))
    } else {
        Err("seek failed".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_endpoint_is_portable() {
        let s = sock_path();
        #[cfg(windows)]
        assert!(
            crate::paths::is_windows_pipe(&s) || std::env::var("SIREN_SOCK").is_ok(),
            "{s}"
        );
        #[cfg(not(windows))]
        assert!(
            s.ends_with("siren-mpv.sock") || std::env::var("SIREN_SOCK").is_ok(),
            "{s}"
        );
        let src = std::fs::read_to_string("src/player.rs").expect("player.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap();
        assert!(prod.contains("#[cfg(windows)]"), "named pipe client");
        assert!(prod.contains("#[cfg(unix)]"), "unix socket client");
        assert!(prod.contains("connect()"), "alive is connect, not exists-only");
    }

    #[test]
    fn parse_time_token_seconds_and_clock() {
        assert_eq!(parse_time_token("90"), Some(90.0));
        assert_eq!(parse_time_token("1:30"), Some(90.0));
        assert_eq!(parse_time_token("00:01:30"), Some(90.0));
        assert_eq!(parse_time_token(""), None);
        assert_eq!(parse_time_token("nope"), None);
    }

    #[test]
    fn parse_seek_relative_percent_absolute() {
        assert_eq!(parse_seek("+5", 10.0, 100.0), Some(15.0));
        assert_eq!(parse_seek("-5", 10.0, 100.0), Some(5.0));
        assert_eq!(parse_seek("-20", 10.0, 100.0), Some(0.0));
        assert_eq!(parse_seek("+20", 90.0, 100.0), Some(100.0));
        assert_eq!(parse_seek("30%", 0.0, 100.0), Some(30.0));
        assert_eq!(parse_seek("1:30", 0.0, 200.0), Some(90.0));
        assert_eq!(parse_seek("90", 0.0, 200.0), Some(90.0));
        assert_eq!(parse_seek("+5", 10.0, 0.0), Some(15.0));
        assert_eq!(parse_seek("", 0.0, 10.0), None);
    }

    #[test]
    fn fmt_rel_time_pads() {
        assert_eq!(fmt_rel_time(0.0), "00:00:00");
        assert_eq!(fmt_rel_time(90.0), "00:01:30");
        assert_eq!(fmt_rel_time(3661.0), "01:01:01");
    }
}
