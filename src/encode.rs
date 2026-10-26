//! PCM HTTP encoder for HEOS follow (`siren stream serve`).
//!
//! Pulse monitor → ffmpeg → WAV / L16 on `:8899`. Same contract as the
//! old Python `siren-stream`. ffmpeg is the capture tool; this process
//! is the HTTP server.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::stream::{CHUNK_BYTES, STREAM_MIME, STREAM_MIME_L16, STREAM_PATH, STREAM_PATH_L16};

/// MIME + ffmpeg `-f` for a request path (`/siren.wav`, `/siren.l16`).
pub fn stream_route(path: &str) -> Option<(&'static str, &'static str)> {
    let p = path.split('?').next().unwrap_or(path);
    match p {
        STREAM_PATH => Some((STREAM_MIME, "wav")),
        STREAM_PATH_L16 => Some((STREAM_MIME_L16, "s16be")),
        _ => None,
    }
}

/// ffmpeg argv after the binary name. `fmt` is `wav` or `s16be`.
pub fn ffmpeg_args(monitor: &str, fmt: &str) -> Vec<String> {
    vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-fflags".into(),
        "nobuffer".into(),
        "-flags".into(),
        "low_delay".into(),
        "-probesize".into(),
        "32".into(),
        "-analyzeduration".into(),
        "0".into(),
        "-f".into(),
        "pulse".into(),
        "-fragment_size".into(),
        "1920".into(),
        "-i".into(),
        monitor.into(),
        "-ac".into(),
        "2".into(),
        "-ar".into(),
        "48000".into(),
        "-f".into(),
        fmt.into(),
        "-".into(),
    ]
}

/// Bind `0.0.0.0:port` and serve forever. Returns 1 on bind failure.
pub fn serve(port: u16, monitor: &str) -> i32 {
    become_group_leader();
    install_term_handler();
    let listener = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("stream serve: bind :{port}: {e}");
            return 1;
        }
    };
    let _ = listener.set_nonblocking(false);
    let mon = monitor.to_string();
    for incoming in listener.incoming() {
        let Ok(stream) = incoming else {
            continue;
        };
        let mon = mon.clone();
        std::thread::spawn(move || handle_client(stream, &mon));
    }
    0
}

fn handle_client(mut sock: TcpStream, monitor: &str) {
    let Some((method, path)) = read_request(&mut sock) else {
        return;
    };
    let Some((mime, fmt)) = stream_route(&path) else {
        let _ = sock
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    };
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n"
    );
    if sock.write_all(headers.as_bytes()).is_err() {
        return;
    }
    if method.eq_ignore_ascii_case("HEAD") {
        return;
    }
    if !method.eq_ignore_ascii_case("GET") {
        return;
    }
    pump_ffmpeg(&mut sock, monitor, fmt);
}

fn read_request(sock: &mut TcpStream) -> Option<(String, String)> {
    let _ = sock.set_read_timeout(Some(Duration::from_secs(8)));
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = sock.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 8192 {
            return None;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let line = text.lines().next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    Some((method, path))
}

fn pump_ffmpeg(sock: &mut TcpStream, monitor: &str, fmt: &str) {
    let args = ffmpeg_args(monitor, fmt);
    let mut cmd = Command::new("ffmpeg");
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                set_pdeathsig();
                Ok(())
            });
        }
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return,
    };
    let Some(mut out) = child.stdout.take() else {
        let _ = child.kill();
        return;
    };
    let mut buf = [0u8; CHUNK_BYTES];
    loop {
        match out.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if sock.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn become_group_leader() {
    extern "C" {
        fn setpgid(pid: i32, pgid: i32) -> i32;
    }
    unsafe {
        let _ = setpgid(0, 0);
    }
}

#[cfg(not(unix))]
fn become_group_leader() {}

#[cfg(unix)]
fn install_term_handler() {
    extern "C" {
        fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
    }
    unsafe {
        signal(15, on_term);
        signal(2, on_term);
    }
}

#[cfg(not(unix))]
fn install_term_handler() {}

#[cfg(unix)]
extern "C" fn on_term(_: i32) {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
        fn getpid() -> i32;
        fn getpgrp() -> i32;
        fn _exit(code: i32);
    }
    unsafe {
        let pid = getpid();
        let pg = getpgrp();
        // Only signal our own group — never the parent shell / follow run.
        if pg == pid {
            let _ = kill(-pg, 9);
        }
        _exit(0);
    }
}

#[cfg(unix)]
fn set_pdeathsig() {
    extern "C" {
        fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
    }
    unsafe {
        let _ = prctl(1, 15, 0, 0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn route_wav_and_l16() {
        assert_eq!(stream_route("/siren.wav"), Some((STREAM_MIME, "wav")));
        assert_eq!(stream_route("/siren.wav?x=1"), Some((STREAM_MIME, "wav")));
        assert_eq!(stream_route("/siren.l16"), Some((STREAM_MIME_L16, "s16be")));
        assert_eq!(stream_route("/"), None);
        assert_eq!(stream_route("/index.html"), None);
    }

    #[test]
    fn ffmpeg_args_pulse_monitor() {
        let a = ffmpeg_args("Siren_Master.monitor", "wav");
        assert!(a.windows(2).any(|w| w[0] == "-f" && w[1] == "pulse"));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-i" && w[1] == "Siren_Master.monitor"));
        assert!(a.windows(2).any(|w| w[0] == "-f" && w[1] == "wav"));
        assert_eq!(a.last().map(String::as_str), Some("-"));
        let b = ffmpeg_args("Siren_Master.monitor", "s16be");
        assert!(b.windows(2).any(|w| w[0] == "-f" && w[1] == "s16be"));
    }

    #[test]
    fn head_wav_and_404_without_ffmpeg() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let t = thread::spawn(move || {
            let (s, _) = listener.accept().expect("accept");
            handle_client(s, "unused-monitor");
        });
        let mut c = TcpStream::connect(addr).expect("connect");
        c.write_all(b"HEAD /siren.wav HTTP/1.1\r\nHost: x\r\n\r\n")
            .expect("write");
        let _ = c.set_read_timeout(Some(Duration::from_secs(2)));
        let mut buf = Vec::new();
        let _ = c.read_to_end(&mut buf);
        let text = String::from_utf8_lossy(&buf);
        assert!(text.contains("200"), "got {text}");
        assert!(text.contains("audio/wav"), "got {text}");
        t.join().expect("join");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind2");
        let addr = listener.local_addr().expect("addr2");
        let t = thread::spawn(move || {
            let (s, _) = listener.accept().expect("accept2");
            handle_client(s, "unused-monitor");
        });
        let mut c = TcpStream::connect(addr).expect("connect2");
        c.write_all(b"GET /nope HTTP/1.1\r\nHost: x\r\n\r\n")
            .expect("write2");
        let _ = c.set_read_timeout(Some(Duration::from_secs(2)));
        let mut buf = Vec::new();
        let _ = c.read_to_end(&mut buf);
        let text = String::from_utf8_lossy(&buf);
        assert!(text.contains("404"), "got {text}");
        t.join().expect("join2");
    }

    #[test]
    fn term_handler_only_kills_own_group() {
        let src = std::fs::read_to_string("src/encode.rs").expect("encode.rs");
        let start = src.find("fn on_term").expect("on_term");
        let body = &src[start..start + 500];
        assert!(body.contains("getpid"), "must know our pid");
        assert!(body.contains("getpgrp"), "must know our group");
        assert!(
            body.contains("pg == pid"),
            "killpg only when we are the group leader"
        );
    }
}
