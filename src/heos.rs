//! HEOS client — TCP/1255 newline `heos://` URIs + DLNA queueing.
//!
//! Shapes proven live this session (firmware 3.139.170):
//! - state/volume live in the `message` querystring (`pid=..&state=play`)
//! - `add_to_queue` cids keep RAW `$` (never %-encode)
//! - DLNA server `VANGUARDA-DLNA` (minidlna `:8200`), browse roots
//!   `1$4` (All Music) / `1$14` (Folders) / `64` / `1`

use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub const DEFAULT_SPEAKER: &str = "Vanguarda Office";

#[derive(Debug, Clone, Default)]
pub struct HeosPlayer {
    pub name: String,
    pub pid: i64,
    pub model: String,
    pub ip: String,
    pub network: String,
    pub state: Option<String>,
    pub volume: Option<i32>,
}

/// Split concatenated JSON objects (firmware sends several per reply).
fn split_objects(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i] as char).is_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        let mut depth = 0;
        let mut in_str = false;
        let mut esc = false;
        let start = i;
        while i < bytes.len() {
            let c = bytes[i];
            if in_str {
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == b'"' {
                    in_str = false;
                }
            } else if c == b'"' {
                in_str = true;
            } else if c == b'{' {
                depth += 1;
            } else if c == b'}' {
                depth -= 1;
                if depth == 0 {
                    i += 1;
                    break;
                }
            }
            i += 1;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&text[start..i.min(text.len())]) {
            out.push(v);
        }
    }
    out
}

/// Persistent command connections (one per speaker): kills the TCP
/// handshake-per-command latency. Serialized by mutex; never subscribed
/// to change events, so traffic stays strict request→reply.
static POOL: OnceLock<Mutex<HashMap<String, TcpStream>>> = OnceLock::new();

fn pool() -> &'static Mutex<HashMap<String, TcpStream>> {
    POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fresh_conn(ip: &str) -> Option<TcpStream> {
    let s = TcpStream::connect_timeout(
        &format!("{ip}:1255").parse().unwrap(),
        Duration::from_secs(3),
    )
    .ok()?;
    s.set_read_timeout(Some(Duration::from_millis(600))).ok()?;
    s.set_write_timeout(Some(Duration::from_secs(3))).ok()?;
    Some(s)
}

fn rpc_once(stream: &mut TcpStream, uris: &[String]) -> Option<Vec<Value>> {
    rpc_once_wait(stream, uris, 25)
}

fn rpc_once_wait(stream: &mut TcpStream, uris: &[String], rounds: u32) -> Option<Vec<Value>> {
    for u in uris {
        stream.write_all(format!("{u}\n").as_bytes()).ok()?;
    }
    // stop at the first quiet gap AFTER all expected replies arrived
    // (firmware sometimes sends "command under process" + the real reply,
    // so exact counting alone would cut off early)
    stream.set_read_timeout(Some(Duration::from_millis(120))).ok()?;
    let mut data = Vec::new();
    let mut buf = [0u8; 8192];
    let mut quiet_rounds = 0;
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                data.extend_from_slice(&buf[..n]);
                quiet_rounds = 0;
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                let text = String::from_utf8_lossy(&data);
                // "command under process" is a stub, not a reply — async
                // commands (browse/search) deliver the payload later
                let finals = split_objects(&text)
                    .iter()
                    .filter(|o| {
                        let msg = o
                            .get("heos")
                            .and_then(|h| h.get("message"))
                            .and_then(|m| m.as_str())
                            .unwrap_or("");
                        !msg.contains("command under process")
                    })
                    .count();
                if finals >= uris.len() {
                    break;
                }
                quiet_rounds += 1;
                if quiet_rounds >= rounds {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if data.is_empty() {
        return None;
    }
    Some(split_objects(&String::from_utf8_lossy(&data)))
}

pub fn rpc(ip: &str, uris: &[String]) -> Vec<Value> {
    rpc_wait(ip, uris, 25)
}

/// Same, with a longer quiet deadline (rounds × 120ms). TuneIn search
/// payloads can lag several seconds behind their "under process" stub.
pub fn rpc_wait(ip: &str, uris: &[String], rounds: u32) -> Vec<Value> {
    // try pooled connection, reconnect once on failure
    {
        let mut pool = pool().lock().unwrap();
        if let Some(s) = pool.get_mut(ip) {
            if let Some(v) = rpc_once_wait(s, uris, rounds) {
                return v;
            }
            pool.remove(ip);
        }
    }
    if let Some(mut s) = fresh_conn(ip) {
        if let Some(v) = rpc_once_wait(&mut s, uris, rounds) {
            pool().lock().unwrap().insert(ip.to_string(), s);
            return v;
        }
    }
    Vec::new()
}

fn ok(objs: &[Value]) -> bool {
    objs.iter()
        .any(|o| o.get("heos").and_then(|h| h.get("result")).and_then(|r| r.as_str()) == Some("success"))
}

/// Apply message-querystring fields (`state=`, `level=`) onto a player.
fn apply_msg(p: &mut HeosPlayer, objs: &[Value]) {
    for o in objs {
        let msg = o
            .get("heos")
            .and_then(|h| h.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        for kv in msg.split('&') {
            if let Some((k, v)) = kv.split_once('=') {
                match k {
                    "state" => p.state = Some(v.to_string()),
                    "level" => {
                        if let Ok(n) = v.parse() {
                            p.volume = Some(n);
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(pl) = o.get("payload") {
            if let Some(s) = pl.get("state").and_then(|x| x.as_str()) {
                p.state = Some(s.to_string());
            }
            if let Some(n) = pl.get("level").and_then(|x| x.as_i64()) {
                p.volume = Some(n as i32);
            }
        }
    }
}

pub fn roster() -> Vec<HeosPlayer> {
    // any fleet member answers with the whole roster; try cached IP first
    let mut out = Vec::new();
    for ip in sweep_known() {
        for o in rpc(&ip, &["heos://player/get_players".into()]) {
            if let Some(pl) = o.get("payload").and_then(|p| p.as_array()) {
                for p in pl {
                    out.push(HeosPlayer {
                        name: p.get("name").and_then(|x| x.as_str()).unwrap_or("?").into(),
                        pid: p.get("pid").and_then(|x| x.as_i64()).unwrap_or(0),
                        model: p.get("model").and_then(|x| x.as_str()).unwrap_or("?").into(),
                        ip: p.get("ip").and_then(|x| x.as_str()).unwrap_or(&ip).into(),
                        network: p.get("network").and_then(|x| x.as_str()).unwrap_or("").into(),
                        state: None,
                        volume: None,
                    });
                }
                if !out.is_empty() {
                    return out;
                }
            }
        }
    }
    out
}

/// IPs to try before a full sweep: ether cache + last-known fleet.
fn sweep_known() -> Vec<String> {
    let mut ips = Vec::new();
    // ether's cache (written by `ether speakers`)
    if let Ok(raw) = std::fs::read_to_string(
        std::env::var("HOME").map(|h| format!("{h}/.config/ether/heos.json")).unwrap_or_default(),
    ) {
        if let Ok(v) = serde_json::from_str::<Value>(&raw) {
            if let Some(ps) = v.get("players").and_then(|p| p.as_array()) {
                for p in ps {
                    if let Some(ip) = p.get("ip").and_then(|x| x.as_str()) {
                        if !ips.contains(&ip.to_string()) {
                            ips.push(ip.to_string());
                        }
                    }
                }
            }
        }
    }
    for fallback in ["192.168.8.184", "192.168.8.185"] {
        if !ips.contains(&fallback.to_string()) {
            ips.push(fallback.into());
        }
    }
    // verify each on 1255, then full sweep if none answer
    let live: Vec<String> = ips.into_iter().filter(|ip| tcp_open(ip, 1255, 400)).collect();
    if live.is_empty() {
        sweep()
    } else {
        live
    }
}

fn tcp_open(ip: &str, port: u16, ms: u64) -> bool {
    TcpStream::connect_timeout(
        &format!("{ip}:{port}").parse().unwrap(),
        Duration::from_millis(ms),
    )
    .is_ok()
}

pub fn local_ip() -> Option<String> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("1.1.1.1:80").ok()?;
    let ip = s.local_addr().ok()?.ip().to_string();
    if ip.starts_with("127.") {
        None
    } else {
        Some(ip)
    }
}

/// Parallel TCP/1255 sweep of the local /24 (unicast — SSDP is broken here).
pub fn sweep() -> Vec<String> {
    let local = match local_ip() {
        Some(ip) => ip,
        None => return Vec::new(),
    };
    let base = match local.rsplit_once('.') {
        Some((b, _)) => b.to_string(),
        None => return Vec::new(),
    };
    let found = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for chunk in (1..255).collect::<Vec<_>>().chunks(64) {
            let found = &found;
            let base = base.clone();
            let mine = local.clone();
            let chunk: Vec<u8> = chunk.to_vec();
            s.spawn(move || {
                for n in chunk {
                    let cand = format!("{base}.{n}");
                    if cand == mine {
                        continue;
                    }
                    if tcp_open(&cand, 1255, 600) {
                        found.lock().unwrap().push(cand);
                    }
                }
            });
        }
    });
    found.into_inner().unwrap()
}

pub fn enrich(players: &mut [HeosPlayer]) {
    let ip = match players.first().map(|p| p.ip.clone()) {
        Some(ip) if !ip.is_empty() => ip,
        _ => return,
    };
    for p in players.iter_mut() {
        if p.pid == 0 {
            continue;
        }
        let objs = rpc(
            &ip,
            &[
                format!("heos://player/get_play_state?pid={}", p.pid),
                format!("heos://player/get_volume?pid={}", p.pid),
            ],
        );
        apply_msg(p, &objs);
    }
}

/// Roster cache: discovery (sweep + probes) costs ~2s per CLI call.
/// Cache (name→pid/ip/model + ts) makes commands cost 1 action RPC.
const ROSTER_TTL: f64 = 120.0;

fn roster_cache_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    std::path::PathBuf::from(format!("{home}/.cache/siren/heos-roster.json"))
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn player_from_json(v: &Value) -> Option<HeosPlayer> {
    Some(HeosPlayer {
        name: v.get("name")?.as_str()?.to_string(),
        pid: v.get("pid")?.as_i64()?,
        model: v.get("model").and_then(|x| x.as_str()).unwrap_or("").into(),
        ip: v.get("ip")?.as_str()?.to_string(),
        network: v.get("network").and_then(|x| x.as_str()).unwrap_or("").into(),
        state: None,
        volume: None,
    })
}

fn read_roster_cache() -> Option<Vec<HeosPlayer>> {
    let raw = std::fs::read_to_string(roster_cache_path()).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let ts = v.get("ts")?.as_f64()?;
    if now_epoch() - ts > ROSTER_TTL {
        return None;
    }
    let arr = v.get("players")?.as_array()?;
    let ps: Vec<HeosPlayer> = arr.iter().filter_map(player_from_json).collect();
    if ps.is_empty() {
        return None;
    }
    Some(ps)
}

fn write_roster_cache(players: &[HeosPlayer]) {
    let arr: Vec<Value> = players
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name, "pid": p.pid, "model": p.model,
                "ip": p.ip, "network": p.network,
            })
        })
        .collect();
    let v = serde_json::json!({ "players": arr, "ts": now_epoch() });
    if let Some(parent) = roster_cache_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = roster_cache_path().with_extension("json.tmp");
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, roster_cache_path());
    }
}

/// Live discovery + rewrite the cache (timer calls this; best effort).
pub fn refresh_roster() {
    let players = roster();
    if !players.is_empty() {
        write_roster_cache(&players);
    }
}

/// Resolve a speaker by name fragment (default: Vanguarda Office).
pub fn resolve(hint: Option<&str>) -> Option<(String, i64, HeosPlayer)> {
    let mut players = read_roster_cache().unwrap_or_else(|| {
        let live = roster();
        if !live.is_empty() {
            write_roster_cache(&live);
        }
        live
    });
    if players.is_empty() {
        return None;
    }
    let pick = |players: &[HeosPlayer]| -> Option<HeosPlayer> {
        if let Some(h) = hint {
            let lo = h.to_lowercase();
            players
                .iter()
                .find(|p| {
                    p.name.to_lowercase().contains(&lo) || p.pid.to_string() == lo
                })
                .cloned()
        } else {
            players
                .iter()
                .find(|p| p.name.to_lowercase().contains("vanguarda"))
                .or_else(|| players.first())
                .cloned()
        }
    };
    pick(&players).map(|p| {
        let mut v = vec![p.clone()];
        enrich(&mut v);
        let e = v.into_iter().next().unwrap();
        (e.ip.clone(), e.pid, e)
    })
}

pub fn set_state(ip: &str, pid: i64, state: &str) -> bool {
    ok(&rpc(ip, &[format!("heos://player/set_play_state?pid={pid}&state={state}")]))
}

pub fn get_state(ip: &str, pid: i64) -> Option<String> {
    for o in rpc(ip, &[format!("heos://player/get_play_state?pid={pid}")]) {
        let msg = o
            .get("heos")
            .and_then(|h| h.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        for kv in msg.split('&') {
            if let Some((k, v)) = kv.split_once('=') {
                if k == "state" {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Toggle play/pause. Returns the state sent.
pub fn toggle(ip: &str, pid: i64) -> &'static str {
    let to = if get_state(ip, pid).as_deref() == Some("play") {
        "pause"
    } else {
        "play"
    };
    set_state(ip, pid, to);
    to
}

pub fn set_volume(ip: &str, pid: i64, level: i32) -> bool {
    let level = level.clamp(0, 100);
    ok(&rpc(ip, &[format!("heos://player/set_volume?pid={pid}&level={level}")]))
}

pub fn set_mute(ip: &str, pid: i64, on: bool) -> bool {
    ok(&rpc(
        ip,
        &[format!("heos://player/set_mute?pid={pid}&state={}", if on { "on" } else { "off" })],
    ))
}

/// NOTE (2026-09-24, verified live): `player/play_stream` with a raw URL
/// is a dead end on this HEOS 1 unit — accepted then back to `stop`.
/// But TuneIn browses fine without login (`available:false` lies), and
/// `browse/play_stream?pid&sid=3&mid=` SUSTAINS (Batida FM, M80 80s both
/// held `play` past 10s). So speaker radio goes through TuneIn, never
/// raw URLs.
pub fn play_next(ip: &str, pid: i64) -> bool {
    ok(&rpc(ip, &[format!("heos://player/play_next?pid={pid}")]))
}

pub fn play_previous(ip: &str, pid: i64) -> bool {
    ok(&rpc(ip, &[format!("heos://player/play_previous?pid={pid}")]))
}

// ---- speaker queue (verbs per HEOS CLI spec; aid 1=now 2=next 3=end 4=replace) ----

#[derive(Debug, Clone, Default)]
pub struct QueueEntry {
    pub qid: i64,
    pub song: String,
    pub artist: String,
    pub album: String,
    pub mid: String,
}

pub fn get_queue(ip: &str, pid: i64) -> Vec<QueueEntry> {
    let mut out = Vec::new();
    for o in rpc(ip, &[format!("heos://player/get_queue?pid={pid}")]) {
        if let Some(arr) = o.get("payload").and_then(|p| p.as_array()) {
            for it in arr {
                out.push(QueueEntry {
                    qid: it.get("qid").and_then(|x| x.as_i64()).unwrap_or(0),
                    song: it.get("song").and_then(|x| x.as_str()).unwrap_or("?").into(),
                    artist: it.get("artist").and_then(|x| x.as_str()).unwrap_or("").into(),
                    album: it.get("album").and_then(|x| x.as_str()).unwrap_or("").into(),
                    mid: it.get("mid").and_then(|x| x.as_str()).unwrap_or("").into(),
                });
            }
        }
    }
    out
}

pub fn play_queue(ip: &str, pid: i64, qid: i64) -> bool {
    ok(&rpc(ip, &[format!("heos://player/play_queue?pid={pid}&qid={qid}")]))
}

pub fn remove_from_queue(ip: &str, pid: i64, qid: i64) -> bool {
    ok(&rpc(ip, &[format!("heos://player/remove_from_queue?pid={pid}&qid={qid}")]))
}

pub fn clear_queue(ip: &str, pid: i64) -> bool {
    ok(&rpc(ip, &[format!("heos://player/clear_queue?pid={pid}")]))
}

pub fn move_queue_item(ip: &str, pid: i64, sqid: i64, dqid: i64) -> bool {
    ok(&rpc(ip, &[format!("heos://player/move_queue_item?pid={pid}&sqid={sqid}&dqid={dqid}")]))
}

// ---- DLNA casting (via VANGUARDA-DLNA / minidlna :8200) ----

fn find_dlna_sid(ip: &str) -> Option<i64> {
    for o in rpc(ip, &["heos://browse/get_music_sources".into()]) {
        if let Some(arr) = o.get("payload").and_then(|p| p.as_array()) {
            for src in arr {
                let is_server = src.get("type").and_then(|t| t.as_str()) == Some("heos_server");
                let name = src.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if is_server && name.to_uppercase().contains("VANGUARDA") {
                    return src.get("sid").and_then(|s| s.as_i64());
                }
            }
        }
    }
    // fallback: Local Music container
    for o in rpc(ip, &["heos://browse/browse?sid=1024".into()]) {
        if let Some(arr) = o.get("payload").and_then(|p| p.as_array()) {
            for src in arr {
                let name = src.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if name.to_uppercase().contains("VANGUARDA") {
                    return src.get("sid").and_then(|s| s.as_i64());
                }
            }
        }
    }
    None
}

fn norm(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// List one DLNA container's children (with paging for big libraries).
fn dlna_children(ip: &str, sid: i64, cid: &str) -> Vec<Value> {
    let mut items = Vec::new();
    let mut start = 0u32;
    for _ in 0..8 {
        let mut got = 0;
        for o in rpc(
            ip,
            &[format!("heos://browse/browse?sid={sid}&cid={cid}&range={start},50")],
        ) {
            if let Some(arr) = o.get("payload").and_then(|p| p.as_array()) {
                got = arr.len();
                items.extend(arr.iter().cloned());
            }
        }
        if got < 50 {
            break;
        }
        start += 50;
    }
    // server ignored range? fall back to a single unpaged browse
    if items.is_empty() {
        for o in rpc(ip, &[format!("heos://browse/browse?sid={sid}&cid={cid}")]) {
            if let Some(arr) = o.get("payload").and_then(|p| p.as_array()) {
                items.extend(arr.iter().cloned());
            }
        }
    }
    items
}

/// Recursive descent from the Music tree, following path components first.
/// Returns (container_cid, song_mid) for add_to_queue.
fn dlna_find(ip: &str, sid: i64, target: &std::path::Path) -> Option<(String, String)> {
    let stem = norm(
        &target
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    if stem.len() < 3 {
        return None;
    }
    // path components as breadcrumbs (e.g. trove, 102nd-old-time-music-hour)
    let crumbs: Vec<String> = target
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(norm))
        .filter(|c| c.len() > 2)
        .collect();
    let mut budget = 40; // RPC cap
    let mut stack = vec!["1".to_string(), "64".to_string()];
    while let Some(cid) = stack.pop() {
        if budget == 0 {
            return None;
        }
        budget -= 1;
        let kids = dlna_children(ip, sid, &cid);
        // songs first
        for item in &kids {
            let is_song = item.get("type").and_then(|t| t.as_str()) == Some("song");
            let playable = item.get("playable").and_then(|p| p.as_str()) == Some("yes");
            if !is_song && !playable {
                continue;
            }
            let name = norm(item.get("name").and_then(|n| n.as_str()).unwrap_or(""));
            if !name.is_empty() && (name.contains(&stem) || stem.contains(&name)) {
                if let Some(m) = item.get("mid").and_then(|x| x.as_str()) {
                    return Some((cid.clone(), m.to_string()));
                }
            }
        }
        // then containers: crumb-matching first
        let mut subs: Vec<(bool, String)> = Vec::new();
        for item in &kids {
            let is_container = item.get("container").and_then(|c| c.as_str()) == Some("yes");
            if !is_container {
                continue;
            }
            if let Some(c) = item.get("cid").and_then(|x| x.as_str()) {
                let name = norm(item.get("name").and_then(|n| n.as_str()).unwrap_or(""));
                let hit = crumbs.iter().any(|cr| name.contains(cr) || cr.contains(&name));
                subs.push((hit, c.to_string()));
            }
        }
        subs.sort_by_key(|(hit, _)| !hit);
        for (_, c) in subs {
            stack.push(c);
        }
    }
    None
}

/// Record a cast for the TUI's elapsed-time clock (firmware gives no
/// position API). Readers (TUI strip) interpolate from this.
pub fn note_cast(path: &std::path::Path) {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let dir = format!("{home}/.cache/siren");
    let _ = std::fs::create_dir_all(&dir);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let v = serde_json::json!({
        "path": path.to_string_lossy(),
        "ts": now,
    });
    let _ = std::fs::write(format!("{dir}/heos-now.json"), v.to_string());
}

/// A TuneIn station hit (search `scid=4`): `mid` plays via
/// `browse/play_stream` with NO cid.
#[derive(Debug, Clone, Default)]
pub struct TuneinHit {
    pub mid: String,
    pub name: String,
}

/// Search TuneIn stations (no login needed, verified live).
pub fn tunein_search(ip: &str, query: &str) -> Vec<TuneinHit> {
    let mut enc = String::with_capacity(query.len());
    for b in query.trim().bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            enc.push(b as char);
        } else if b == b' ' {
            enc.push_str("%20");
        } else {
            enc.push_str(&format!("%{b:02X}"));
        }
    }
    let mut out = Vec::new();
    // Patient read on a fresh connection: TuneIn answers the search stub
    // fast, then delivers the payload up to seconds later and closes.
    // (The shared rpc() quiet-gap loop exits on the stub.)
    let t0 = std::time::Instant::now();
    let objs = match fresh_conn(ip) {
        Some(mut s) => {
            use std::io::{Read, Write};
            let cmd = format!("heos://browse/search?sid=3&scid=4&search={enc}\n");
            s.write_all(cmd.as_bytes()).ok();
            s.set_read_timeout(Some(std::time::Duration::from_millis(500)))
                .ok();
            let mut data = Vec::new();
            let mut buf = [0u8; 65536];
            let mut quiet_since_data = t0;
            loop {
                match s.read(&mut buf) {
                    Ok(0) => break, // server closed: reply complete
                    Ok(n) => {
                        data.extend_from_slice(&buf[..n]);
                        quiet_since_data = std::time::Instant::now();
                    }
                    Err(_) => {
                        if !data.is_empty()
                            && quiet_since_data.elapsed().as_secs() >= 2
                        {
                            break;
                        }
                        if t0.elapsed().as_secs() > 12 {
                            break;
                        }
                    }
                }
            }
            split_objects(&String::from_utf8_lossy(&data))
        }
        None => Vec::new(),
    };
    for o in objs {
        if let Some(arr) = o.get("payload").and_then(|p| p.as_array()) {
            for it in arr {
                let mid = it.get("mid").and_then(|x| x.as_str()).unwrap_or("");
                let name = it.get("name").and_then(|x| x.as_str()).unwrap_or("");
                let playable = it.get("playable").and_then(|x| x.as_str()).unwrap_or("yes");
                if mid.is_empty() || name.is_empty() || playable != "yes" {
                    continue;
                }
                out.push(TuneinHit {
                    mid: mid.into(),
                    name: name.into(),
                });
            }
        }
    }
    out
}

/// Play a TuneIn hit on the speaker. True = command accepted
/// (state turns `play` a few seconds later; verified live).
pub fn tunein_play(ip: &str, pid: i64, hit: &TuneinHit) -> bool {
    ok(&rpc(
        ip,
        &[format!(
            "heos://browse/play_stream?pid={pid}&sid=3&mid={}",
            hit.mid
        )],
    ))
}

/// Record a radio play for the elapsed clock + now displays.
pub fn note_radio(url: &str, title: &str) {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let dir = format!("{home}/.cache/siren");
    let _ = std::fs::create_dir_all(&dir);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let v = serde_json::json!({
        "path": url,
        "title": title,
        "ts": now,
    });
    let _ = std::fs::write(format!("{dir}/heos-now.json"), v.to_string());
}

/// Station title for the current radio play, if any.
pub fn last_radio_title() -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let raw = std::fs::read_to_string(format!("{home}/.cache/siren/heos-now.json")).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v.get("title").and_then(|t| t.as_str()).map(|s| s.to_string())
}

/// Last cast recorded by any siren (CLI or TUI): (path, epoch secs).
pub fn last_cast() -> Option<(std::path::PathBuf, f64)> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let raw = std::fs::read_to_string(format!("{home}/.cache/siren/heos-now.json")).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let p = v.get("path")?.as_str()?;
    let ts = v.get("ts")?.as_f64()?;
    Some((std::path::PathBuf::from(p), ts))
}

/// Cast a playlist in order: first aid=1 (play-now), rest aid=3 (append).
/// Returns (ok_count, total). Slow per track (one DLNA browse each) but exact.
pub fn dlna_cast_many(ip: &str, pid: i64, paths: &[std::path::PathBuf]) -> (usize, usize) {
    let mut ok = 0;
    for (i, p) in paths.iter().enumerate() {
        if i > 0 {
            // HEOS 1 CLI server wedges if add_to_queue is hammered; 1.5s
            // between appends keeps port 1255 alive (verified live).
            std::thread::sleep(Duration::from_millis(1500));
        }
        let aid = if i == 0 { 1 } else { 3 };
        if dlna_cast(ip, pid, p, aid).is_ok() {
            ok += 1;
        }
    }
    (ok, paths.len())
}

/// Cast a local file to a speaker via DLNA. Copies into /tmp/dlna when the
/// file isn't already served (~/Music and /tmp/dlna are minidlna roots).
/// `aid`: 1 play-now (default), 2 play-next, 3 add-to-end, 4 replace-and-play.
pub fn dlna_cast(ip: &str, pid: i64, path: &std::path::Path, aid: i64) -> Result<(), String> {
    let served = ["/tmp/dlna", &format!("{}/Music", std::env::var("HOME").unwrap_or_default())];
    let abs = path.to_string_lossy().into_owned();
    let in_dlna = served.iter().any(|d| abs.starts_with(d));
    let target: std::path::PathBuf;
    if in_dlna {
        target = path.to_path_buf();
    } else {
        let dl = std::path::PathBuf::from("/tmp/dlna");
        std::fs::create_dir_all(&dl).map_err(|e| format!("dlna dir: {e}"))?;
        target = dl.join(path.file_name().ok_or("bad filename")?);
        let same_size = target.exists()
            && target.metadata().map(|m| m.len()).unwrap_or(0)
                == path.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
        if !same_size {
            std::fs::copy(path, &target).map_err(|e| format!("copy to DLNA: {e}"))?;
            std::thread::sleep(Duration::from_millis(1200));
            // nudge a rescan (best effort)
            let _ = std::process::Command::new("pkill")
                .args(["-USR1", "minidlnad"])
                .output();
            std::thread::sleep(Duration::from_millis(800));
        }
    }
    let mut sid = find_dlna_sid(ip);
    if sid.is_none() {
        std::thread::sleep(Duration::from_secs(2));
        sid = find_dlna_sid(ip);
    }
    let sid = sid.ok_or_else(|| {
        "DLNA VANGUARDA-DLNA not found — is minidlna on 192.168.8.186:8200 running?".to_string()
    })?;
    // Fresh files (downloads, transcodes) may not be indexed yet — the
    // rescan nudge above can't signal root's minidlnad, so poll a while.
    // Only for recent files; genuine misses still fail fast.
    let fresh = target
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .map(|e| e.as_secs() < 15 * 60)
        .unwrap_or(false);
    let mut found = dlna_find(ip, sid, &target);
    if found.is_none() && fresh {
        std::thread::sleep(Duration::from_secs(5));
        for _ in 0..11 {
            found = dlna_find(ip, sid, &target);
            if found.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    }

    match found {
        Some((cid, mid)) => {
            let aid = aid.clamp(1, 4);
            // RAW $ in cid/mid (never %-encode) — firmware rejects %24
            let cmd = format!(
                "heos://browse/add_to_queue?pid={pid}&sid={sid}&cid={cid}&mid={mid}&aid={aid}"
            );
            if ok(&rpc(ip, &[cmd])) {
                return Ok(());
            }
            Err(format!("queue rejected for '{cid}/{mid}' (sid {sid})"))
        }
        None => {
            let stem = target
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            Err(format!("track not in DLNA for '{stem}' (sid {sid})"))
        }
    }
}
