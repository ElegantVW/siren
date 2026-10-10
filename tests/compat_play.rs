//! Live local-playback smoke. Spawns the siren **binary** in an isolated
//! HOME so the house follow/Cave/headset stay out of it.
//!
//! Needs `mpv` on PATH. Decode uses `SIREN_AO=null` (no analog, no Cave).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn siren_bin() -> &'static str {
    env!("CARGO_BIN_EXE_siren")
}

fn write_beep_wav(path: &Path, secs: f32) {
    let sr = 44100u32;
    let n = ((sr as f32) * secs).round() as u32;
    let data_bytes = n * 2;
    let mut buf = Vec::with_capacity(44 + data_bytes as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36u32 + data_bytes).to_le_bytes());
    buf.extend_from_slice(b"WAVEfmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&sr.to_le_bytes());
    buf.extend_from_slice(&(sr * 2).to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes());
    buf.extend_from_slice(&16u16.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_bytes.to_le_bytes());
    for i in 0..n {
        let t = i as f32 / sr as f32;
        let s = (2.0 * std::f32::consts::PI * 440.0 * t).sin();
        let v = (s * 8000.0) as i16;
        buf.extend_from_slice(&v.to_le_bytes());
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(path, buf).unwrap();
}

struct House {
    home: PathBuf,
    sock: PathBuf,
}

impl House {
    fn new(tag: &str) -> Self {
        let home = std::env::temp_dir().join(format!(
            "siren-compat-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("Music")).unwrap();
        std::fs::create_dir_all(home.join(".config/siren")).unwrap();
        let sock = home.join("mpv.sock");
        Self { home, sock }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(siren_bin());
        c.env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("SIREN_CONFIG_DIR", self.home.join(".config/siren"))
            .env("SIREN_SOCK", &self.sock)
            .env("SIREN_AO", "null")
            .env_remove("SIREN_LAN_IP")
            .env_remove("SIREN_LAN_IFACE");
        c
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.cmd()
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("siren {}: {e}", args.join(" ")))
    }
}

impl Drop for House {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            if let Ok(mut s) = std::os::unix::net::UnixStream::connect(&self.sock) {
                let _ = s.write_all(b"{\"command\":[\"quit\"]}\n");
            }
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn mpv_on_path() -> bool {
    Command::new("mpv")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn palette_default_without_mpv() {
    let h = House::new("palette");
    let out = h.run(&["config", "get", "palette"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stderr {}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("pixie"), "got {stdout}");
}

#[test]
fn play_beep_then_now_json() {
    if !mpv_on_path() {
        eprintln!("skip play_beep_then_now_json: no mpv");
        return;
    }
    let h = House::new("play");
    write_beep_wav(&h.home.join("Music/beep.wav"), 3.0);
    let set = h.run(&["config", "set", "audio_output", "local"]);
    assert!(set.status.success(), "config set: {}", String::from_utf8_lossy(&set.stderr));

    let play = h.run(&["play", "beep"]);
    assert!(
        play.status.success(),
        "play failed stdout={} stderr={}",
        String::from_utf8_lossy(&play.stdout),
        String::from_utf8_lossy(&play.stderr)
    );
    std::thread::sleep(Duration::from_millis(400));

    let now = h.run(&["now", "--json"]);
    assert!(now.status.success(), "now: {}", String::from_utf8_lossy(&now.stderr));
    let body = String::from_utf8_lossy(&now.stdout);
    assert!(body.contains("beep"), "now json should name beep: {body}");
    assert!(
        body.contains("\"state\":\"play\"") || body.contains("\"state\": \"play\""),
        "now json should be play: {body}"
    );

    let stop = h.run(&["stop"]);
    assert!(stop.status.success(), "stop: {}", String::from_utf8_lossy(&stop.stderr));
}

#[test]
fn missing_pactl_still_configs() {
    let h = House::new("nopactl");
    let bin_dir = h.home.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    // empty PATH except our bin — config get must not need pactl
    let out = h
        .cmd()
        .env("PATH", &bin_dir)
        .args(["config", "get", "palette"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stderr {}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("pixie"), "got {stdout}");
}

#[test]
fn missing_mpv_play_fails_closed() {
    let h = House::new("nompv");
    write_beep_wav(&h.home.join("Music/beep.wav"), 1.0);
    let bin_dir = h.home.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let play = h
        .cmd()
        .env("PATH", &bin_dir)
        .args(["play", "beep"])
        .output()
        .unwrap();
    assert!(
        !play.status.success(),
        "play without mpv must fail closed, stdout={}",
        String::from_utf8_lossy(&play.stdout)
    );
}
