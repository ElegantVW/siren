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

pub const DEFAULT_SPEAKER: &str = "Cave";
pub const HEOS_CLI_PORT: u16 = 1255;
/// Per-host TCP budget for the /24 scan. 64 workers × 254 hosts ≲ 2s.
pub const SWEEP_MS: u64 = 350;
pub const SWEEP_WORKERS: usize = 64;

/// Worst-case wall time for a full /24 with SWEEP_WORKERS (ms).
pub fn sweep_budget_ms() -> u64 {
    let hosts = 254u64;
    let w = SWEEP_WORKERS as u64;
    hosts.div_ceil(w) * SWEEP_MS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeVerdict {
    Ok,
    IpMoved,
    NotOnLan,
}

impl ProbeVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::IpMoved => "ip-moved",
            Self::NotOnLan => "not-on-lan",
        }
    }
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Ok => 0,
            Self::NotOnLan => 1,
            Self::IpMoved => 2,
        }
    }
}

/// Cached row + LAN 1255 hits → one of three honest verdicts.
pub fn probe_verdict(cached_live: usize, lan_hits: usize) -> ProbeVerdict {
    if cached_live > 0 {
        ProbeVerdict::Ok
    } else if lan_hits > 0 {
        ProbeVerdict::IpMoved
    } else {
        ProbeVerdict::NotOnLan
    }
}

#[derive(Debug, Clone)]
pub struct CachedRow {
    pub name: String,
    pub ip: String,
    pub arp: bool,
    pub cli: bool,
    pub upnp: bool,
}

#[derive(Debug, Clone)]
pub struct ProbeReport {
    pub cached: Vec<CachedRow>,
    pub lan_1255: Vec<String>,
    pub verdict: ProbeVerdict,
}

#[derive(Debug, Clone, Default)]
pub struct HeosPlayer {
    pub name: String,
    pub pid: i64,
    pub model: String,
    pub ip: String,
    pub network: String,
    pub state: Option<String>,
    pub volume: Option<i32>,
    pub song: Option<String>,
    /// HTTP(S) stream from `get_now_playing_media` `mid` (TuneIn radio).
    /// Empty for file casts and TuneIn ids that are not URLs.
    pub stream_url: Option<String>,
}


/// A HEOS speaker group (from `group/get_groups`).
#[derive(Debug, Clone, Default)]
pub struct HeosGroup {
    pub name: String,
    pub gid: i64,
    pub players: Vec<GroupPlayer>,
}

#[derive(Debug, Clone, Default)]
pub struct GroupPlayer {
    pub name: String,
    pub pid: i64,
    pub role: String,
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
            if let Some(s) = pl.get("song").and_then(|x| x.as_str()) {
                if !s.is_empty() {
                    p.song = Some(s.to_string());
                }
            }
            if let Some(mid) = pl.get("mid").and_then(|x| x.as_str()) {
                p.stream_url = crate::stream::http_url(mid);
            }
        }
    }
}

pub fn parse_get_players(objs: &[Value], fallback_ip: &str) -> Vec<HeosPlayer> {
    let mut out = Vec::new();
    for o in objs {
        if let Some(pl) = o.get("payload").and_then(|p| p.as_array()) {
            for p in pl {
                out.push(HeosPlayer {
                    name: p.get("name").and_then(|x| x.as_str()).unwrap_or("?").into(),
                    pid: p.get("pid").and_then(|x| x.as_i64()).unwrap_or(0),
                    model: p.get("model").and_then(|x| x.as_str()).unwrap_or("?").into(),
                    ip: p.get("ip").and_then(|x| x.as_str()).unwrap_or(fallback_ip).into(),
                    network: p.get("network").and_then(|x| x.as_str()).unwrap_or("").into(),
                    state: None,
                    volume: None,
                    song: None,
                    stream_url: None,
                });
            }
            if !out.is_empty() {
                return out;
            }
        }
    }
    out
}

fn roster_from(ips: Vec<String>) -> Vec<HeosPlayer> {
    // any fleet member answers with the whole roster
    for ip in ips {
        let objs = rpc(&ip, &["heos://player/get_players".into()]);
        let out = parse_get_players(&objs, &ip);
        if !out.is_empty() {
            return out;
        }
    }
    Vec::new()
}

pub fn roster() -> Vec<HeosPlayer> {
    // known IPs only — the /24 sweep belongs to refresh_roster (deadline-bounded)
    roster_from(sweep_known())
}

fn push_unique(ips: &mut Vec<String>, ip: &str) {
    if !ip.is_empty() && !ips.iter().any(|x| x == ip) {
        ips.push(ip.to_string());
    }
}

/// IPs to try before a full sweep: ether cache + last roster (any LAN).
/// Shop 192.168.8.x is not a fallback — identity is not a cached IP.
fn sweep_known() -> Vec<String> {
    let mut ips = Vec::new();
    if let Ok(raw) = std::fs::read_to_string(crate::paths::ether_heos()) {
        if let Ok(v) = serde_json::from_str::<Value>(&raw) {
            if let Some(ps) = v.get("players").and_then(|p| p.as_array()) {
                for p in ps {
                    if let Some(ip) = p.get("ip").and_then(|x| x.as_str()) {
                        push_unique(&mut ips, ip);
                    }
                }
            }
        }
    }
    if let Some(ps) = read_roster_stale() {
        for p in ps {
            push_unique(&mut ips, &p.ip);
        }
    }
    ips.into_iter()
        .filter(|ip| tcp_open(ip, 1255, 400))
        .collect()
}

fn tcp_open(ip: &str, port: u16, ms: u64) -> bool {
    TcpStream::connect_timeout(
        &format!("{ip}:{port}").parse().unwrap(),
        Duration::from_millis(ms),
    )
    .is_ok()
}

/// Parallel TCP connect. One SYN per addr, capped at SWEEP_WORKERS.
pub fn parallel_open(addrs: &[(String, u16)], timeout_ms: u64) -> Vec<(String, u16)> {
    if addrs.is_empty() {
        return Vec::new();
    }
    let i = Mutex::new(0usize);
    let found = Mutex::new(Vec::new());
    let nworkers = SWEEP_WORKERS.min(addrs.len()).max(1);
    std::thread::scope(|s| {
        for _ in 0..nworkers {
            s.spawn(|| {
                loop {
                    let idx = {
                        let mut g = i.lock().unwrap();
                        if *g >= addrs.len() {
                            return;
                        }
                        let n = *g;
                        *g += 1;
                        n
                    };
                    let (host, port) = &addrs[idx];
                    if tcp_open(host, *port, timeout_ms) {
                        found.lock().unwrap().push((host.clone(), *port));
                    }
                }
            });
        }
    });
    let mut v = found.into_inner().unwrap();
    v.sort();
    v
}

/// /proc/net/arp flag 0x2 = complete (NUD_REACHABLE / STALE with a MAC).
pub fn arp_flag_complete(flags: &str) -> bool {
    let t = flags.trim().trim_start_matches("0x");
    u32::from_str_radix(t, 16).unwrap_or(0) & 0x2 != 0
}

pub fn arp_complete(ip: &str) -> bool {
    let Ok(raw) = std::fs::read_to_string("/proc/net/arp") else {
        return false;
    };
    for line in raw.lines().skip(1) {
        let mut cols = line.split_whitespace();
        let addr = cols.next().unwrap_or("");
        let _hwtype = cols.next();
        let flags = cols.next().unwrap_or("0x0");
        if addr == ip {
            return arp_flag_complete(flags);
        }
    }
    false
}

/// One IPv4 on a named iface (`ip -o -4 addr`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceAddr {
    pub name: String,
    pub ip: String,
    pub prefix: u8,
    pub is_usb: bool,
}

fn skip_iface_name(name: &str) -> bool {
    matches!(name, "lo" | "tailscale0" | "docker0")
        || name.starts_with("veth")
        || name.starts_with("br-")
        || name.starts_with("virbr")
        || name.starts_with("wg")
}

pub fn is_rfc1918(ip: &str) -> bool {
    if ip.starts_with("10.") || ip.starts_with("192.168.") {
        return true;
    }
    if let Some(rest) = ip.strip_prefix("172.") {
        if let Some(oct) = rest.split('.').next() {
            if let Ok(n) = oct.parse::<u8>() {
                return (16..=31).contains(&n);
            }
        }
    }
    false
}

/// Parse `ip -o -4 addr show` lines.
pub fn parse_ip_o_addr(text: &str) -> Vec<(String, String, u8)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 || parts[2] != "inet" {
            continue;
        }
        let name = parts[1].trim_end_matches(':');
        let Some((ip, pfx)) = parts[3].split_once('/') else {
            continue;
        };
        let prefix: u8 = pfx.parse().unwrap_or(32);
        out.push((name.to_string(), ip.to_string(), prefix));
    }
    out
}

pub fn iface_is_usb(name: &str) -> bool {
    if name.to_ascii_lowercase().contains("usb") {
        return true;
    }
    let sub = format!("/sys/class/net/{name}/device/subsystem");
    std::fs::read_link(&sub)
        .ok()
        .and_then(|p| p.file_name().map(|s| s == "usb"))
        .unwrap_or(false)
}

/// Speaker LAN: wired RFC1918 /24, not the USB tether, not the default-route src
/// when another private iface exists. `prefer_iface` is `SIREN_LAN_IFACE`.
pub fn pick_lan_ipv4(
    ifaces: &[IfaceAddr],
    default_src: Option<&str>,
    prefer_iface: Option<&str>,
) -> Option<String> {
    let usable = |i: &IfaceAddr| {
        !skip_iface_name(&i.name)
            && is_rfc1918(&i.ip)
            && (8..=24).contains(&i.prefix)
            && !i.ip.starts_with("127.")
            && !i.ip.starts_with("169.254.")
            && !i.ip.starts_with("100.")
    };
    let wired: Vec<&IfaceAddr> = ifaces
        .iter()
        .filter(|i| usable(i) && !i.is_usb)
        .collect();
    let cands = if wired.is_empty() {
        ifaces.iter().filter(|i| usable(i)).collect()
    } else {
        wired
    };
    if cands.is_empty() {
        return None;
    }
    if let Some(want) = prefer_iface {
        if let Some(i) = cands.iter().find(|i| i.name == want) {
            return Some(i.ip.clone());
        }
    }
    if let Some(i) = cands.iter().find(|i| i.name == "enp39s0") {
        return Some(i.ip.clone());
    }
    if let Some(def) = default_src {
        if let Some(i) = cands.iter().find(|i| i.ip != def) {
            return Some(i.ip.clone());
        }
    }
    Some(cands[0].ip.clone())
}

fn default_route_src() -> Option<String> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("1.1.1.1:80").ok()?;
    let ip = s.local_addr().ok()?.ip().to_string();
    if ip.starts_with("127.") {
        None
    } else {
        Some(ip)
    }
}

fn list_ifaces() -> Vec<IfaceAddr> {
    let out = std::process::Command::new("ip")
        .args(["-o", "-4", "addr", "show"])
        .output();
    let Ok(out) = out else {
        return Vec::new();
    };
    parse_ip_o_addr(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .map(|(name, ip, prefix)| IfaceAddr {
            is_usb: iface_is_usb(&name),
            name,
            ip,
            prefix,
        })
        .collect()
}

/// Address speakers can reach (follow wav, /24 sweep). Never the USB
/// tether when `enp39s0` (or another wired RFC1918 /24) is up.
/// Override: `SIREN_LAN_IP` or `SIREN_LAN_IFACE`.
pub fn local_ip() -> Option<String> {
    if let Ok(ip) = std::env::var("SIREN_LAN_IP") {
        let ip = ip.trim();
        if !ip.is_empty() {
            return Some(ip.to_string());
        }
    }
    let prefer = std::env::var("SIREN_LAN_IFACE").ok();
    let default_src = default_route_src();
    pick_lan_ipv4(
        &list_ifaces(),
        default_src.as_deref(),
        prefer.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty()),
    )
    .or(default_src)
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
    let addrs: Vec<(String, u16)> = (1u8..255)
        .filter_map(|n| {
            let cand = format!("{base}.{n}");
            if cand == local {
                None
            } else {
                Some((cand, HEOS_CLI_PORT))
            }
        })
        .collect();
    parallel_open(&addrs, SWEEP_MS)
        .into_iter()
        .map(|(ip, _)| ip)
        .collect()
}

/// Unpooled RPC to host:port — tests bind ephemeral ports; production uses :1255.
pub fn rpc_at(host: &str, port: u16, uris: &[String]) -> Vec<Value> {
    let s = TcpStream::connect_timeout(
        &format!("{host}:{port}").parse().ok().unwrap_or_else(|| "0.0.0.0:80".parse().unwrap()),
        Duration::from_secs(3),
    );
    let Ok(mut s) = s else {
        return Vec::new();
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(600)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(3)));
    rpc_once_wait(&mut s, uris, 25).unwrap_or_default()
}

/// LAN doctor: cached IPs vs a fast /24 :1255 scan. `ip-moved` adopts the new roster.
pub fn run_probe() -> ProbeReport {
    let cached_ps = read_roster_stale().unwrap_or_default();
    let mut cached = Vec::new();
    let mut cached_live = 0usize;
    for p in &cached_ps {
        let cli = tcp_open(&p.ip, HEOS_CLI_PORT, SWEEP_MS);
        let upnp = tcp_open(&p.ip, 60006, SWEEP_MS);
        if cli {
            cached_live += 1;
        }
        cached.push(CachedRow {
            name: p.name.clone(),
            ip: p.ip.clone(),
            arp: arp_complete(&p.ip),
            cli,
            upnp,
        });
    }
    let lan_1255 = sweep();
    let verdict = probe_verdict(cached_live, lan_1255.len());
    if verdict == ProbeVerdict::IpMoved {
        let players = roster_from(lan_1255.clone());
        if !players.is_empty() {
            write_roster_cache(&players);
        }
    }
    ProbeReport {
        cached,
        lan_1255,
        verdict,
    }
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
                format!("heos://player/get_now_playing_media?pid={}", p.pid),
            ],
        );
        apply_msg(p, &objs);
    }
}

/// Roster cache: discovery (sweep + probes) costs ~2s per CLI call.
/// Cache (name→pid/ip/model + ts) makes commands cost 1 action RPC.
const ROSTER_TTL: f64 = 120.0;

fn roster_cache_path() -> std::path::PathBuf {
    crate::paths::join_cache("heos-roster.json")
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
        song: None,
        stream_url: None,
    })
}

fn players_from_roster_file() -> Option<(Vec<HeosPlayer>, f64)> {
    let raw = std::fs::read_to_string(roster_cache_path()).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let ts = v.get("ts")?.as_f64()?;
    let arr = v.get("players")?.as_array()?;
    let ps: Vec<HeosPlayer> = arr.iter().filter_map(player_from_json).collect();
    if ps.is_empty() {
        return None;
    }
    Some((ps, ts))
}

pub fn read_roster_cache() -> Option<Vec<HeosPlayer>> {
    let (ps, ts) = players_from_roster_file()?;
    if now_epoch() - ts > ROSTER_TTL {
        return None;
    }
    Some(ps)
}

/// Last-known fleet even after TTL. Status commands use this so a
/// down speaker still has a name instead of a blank "unreachable".
pub fn read_roster_stale() -> Option<Vec<HeosPlayer>> {
    players_from_roster_file().map(|(ps, _)| ps)
}

/// Pick by name fragment / pid, else Cave, else first.
/// `pick_named("Office")` still finds Office when it joins.
pub fn pick_named<'a>(players: &'a [HeosPlayer], hint: &str) -> Option<&'a HeosPlayer> {
    let lo = hint.to_lowercase();
    let named = if lo.is_empty() {
        None
    } else {
        players
            .iter()
            .find(|p| p.name.to_lowercase().contains(&lo) || p.pid.to_string() == lo)
    };
    named
        .or_else(|| {
            players
                .iter()
                .find(|p| p.name.to_lowercase().contains("cave"))
        })
        .or_else(|| players.first())
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
    write_ether_cache(players);
}

/// Refresh Ether's HEOS file so `ether status` and sweep_known share one fleet.
fn write_ether_cache(players: &[HeosPlayer]) {
    if players.is_empty() {
        return;
    }
    let path = crate::paths::ether_heos();
    if path.as_os_str().is_empty() {
        return;
    }
    let path = path.to_string_lossy().into_owned();
    let arr: Vec<Value> = players
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name, "pid": p.pid, "model": p.model,
                "ip": p.ip, "network": p.network,
            })
        })
        .collect();
    let v = serde_json::json!({ "ts": now_epoch() as i64, "players": arr });
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = format!("{path}.tmp");
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Live discovery + rewrite the cache (timer calls this; best effort).
/// Known IPs first; /24 sweep only when those are silent.
pub fn refresh_roster() {
    let mut players = roster();
    if players.is_empty() {
        players = roster_from(sweep());
    }
    if !players.is_empty() {
        write_roster_cache(&players);
    }
}

/// Resolve a speaker by name fragment (default: Cave).
pub fn resolve(hint: Option<&str>) -> Option<(String, i64, HeosPlayer)> {
    let players = read_roster_cache()
        .or_else(read_roster_stale)
        .unwrap_or_else(|| {
            let live = roster();
            if !live.is_empty() {
                write_roster_cache(&live);
            }
            live
        });
    if players.is_empty() {
        return None;
    }
    let h = hint.unwrap_or(DEFAULT_SPEAKER);
    pick_named(&players, h).cloned().map(|p| {
        let mut v = vec![p];
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

/// Mute flag from a HEOS `message` query string. Own parser — `apply_msg`
/// treats `state=` as play/pause and would clobber transport.
pub fn parse_mute_state(msg: &str) -> Option<bool> {
    let q = msg.split('?').nth(1).unwrap_or(msg);
    for kv in q.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            if k == "state" {
                return match v {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                };
            }
        }
    }
    None
}

pub fn get_mute(ip: &str, pid: i64) -> Option<bool> {
    let objs = rpc(ip, &[format!("heos://player/get_mute?pid={pid}")]);
    for o in objs {
        let msg = o
            .get("heos")
            .and_then(|h| h.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        if let Some(b) = parse_mute_state(msg) {
            return Some(b);
        }
    }
    None
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
    let dir = crate::paths::cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let v = serde_json::json!({
        "path": path.to_string_lossy(),
        "ts": now,
    });
    let _ = std::fs::write(dir.join("heos-now.json"), v.to_string());
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

/// Play a raw stream URL on the speaker. True = command accepted.
/// NOTE: acceptance is NOT playback — the speaker resolves the URL
/// server-side and may silently stop on 404. Always follow with
/// `confirm_play`. Percent-encodes the URL per HEOS CLI spec
/// (&, =, % must be encoded in attribute values).
pub fn play_url(ip: &str, pid: i64, url: &str) -> bool {
    let mut enc = String::with_capacity(url.len());
    for b in url.bytes() {
        match b {
            b'&' => enc.push_str("%26"),
            b'=' => enc.push_str("%3D"),
            b'%' => enc.push_str("%25"),
            b' ' => enc.push_str("%20"),
            _ => enc.push(b as char),
        }
    }
    ok(&rpc(
        ip,
        &[format!("heos://browse/play_stream?pid={pid}&url={enc}")],
    ))
}

fn http_looks_alive(head: &str) -> bool {
    let h = head.to_lowercase();
    h.contains(" 200")
        || h.contains(" 206")
        || h.contains(" 301")
        || h.contains(" 302")
        || h.contains("audio/")
        || h.contains("mpegurl")
        || h.contains("x-mpegurl")
}

fn http_no_retry(head: &str) -> bool {
    let h = head.to_lowercase();
    h.contains(" 404") || h.contains(" 410") || h.contains(" 401")
}

fn http_exchange(host: &str, port: u16, req: &str) -> String {
    use std::io::{Read, Write};
    let mut s = match std::net::TcpStream::connect_timeout(
        &format!("{host}:{port}").parse().ok().unwrap_or_else(|| "0.0.0.0:80".parse().unwrap()),
        std::time::Duration::from_secs(3),
    ) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(3)));
    if s.write_all(req.as_bytes()).is_err() {
        return String::new();
    }
    let mut buf = [0u8; 1024];
    let n = s.read(&mut buf).unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// Probe a stream URL before telling the speaker to play it.
/// HEAD first; Icecast/Shoutcast often 400/405 HEAD while GET 200
/// (OXIGÉNIO 102.6 is that shape). 404/410/401 on HEAD stay dead.
pub fn url_alive(url: &str) -> bool {
    // HTTPS: no TLS in std, so we cannot check. Assume alive and let
    // `confirm_play` decide — it catches every failure mode except a
    // speaker that reports `play` forever on a 404 (rare; the TuneIn
    // path below is the common case and is fully verified).
    if url.starts_with("https://") {
        return true;
    }
    let (host, port, path) = match parse_http_url(url) {
        Some(t) => t,
        None => return false,
    };
    let head_req = format!(
        "HEAD {path} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: siren\r\nConnection: close\r\n\r\n"
    );
    let head = http_exchange(&host, port, &head_req);
    if http_looks_alive(&head) {
        return true;
    }
    if http_no_retry(&head) {
        return false;
    }
    let get_req = format!(
        "GET {path} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: siren\r\nConnection: close\r\n\r\n"
    );
    http_looks_alive(&http_exchange(&host, port, &get_req))
}

/// Split http(s)://host[:port][/path] into parts. HTTPS treated as
/// port 443 (no TLS — HEAD check only needs the status line, and
/// most stream hosts serve plain HTTP anyway; HTTPS returns false
/// rather than blocking).
fn parse_http_url(url: &str) -> Option<(String, u16, String)> {
    // HTTPS: no TLS in std — retry as plain HTTP on :80. Most
    // Shoutcast/Icecast hosts serve both; if not, we return false
    // and the caller falls back to TuneIn. Safe direction.
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match hostport.find(':') {
        Some(i) => (hostport[..i].to_string(), hostport[i + 1..].parse().unwrap_or(80)),
        None => (hostport.to_string(), 80),
    };
    if host.is_empty() {
        return None;
    }
    Some((host, port, path))
}

/// ONE deferred play-state check. Never poll 1255 in a loop: this
/// firmware wedges its CLI under rapid requests (fuzzing proved it,
/// and a 800ms-confirm loop wedged the speaker badly enough to flash
/// its status LED). One RPC after `secs` is safe; a loop is not.
pub fn confirm_play(ip: &str, pid: i64, secs: u64) -> bool {
    std::thread::sleep(std::time::Duration::from_secs(secs));
    get_state(ip, pid).as_deref() == Some("play")
}

/// Verify a direct-URL play via UPnP GetPositionInfo (port 60006).
/// The UPnP service has survived heavy probing all session without
/// wedging — unlike the 1255 CLI. Returns true when the speaker's
/// TrackURI matches the URL we sent (it may normalize case or
/// percent-encoding, so we compare loosely).
pub fn confirm_play_url(ip: &str, url: &str, secs: u64) -> bool {
    std::thread::sleep(std::time::Duration::from_secs(secs));
    let uri = upnp_track_uri(ip);
    if uri.is_empty() {
        return false;
    }
    // Loose match: speaker echoes what we sent, but don't trust
    // exact bytes. Compare the path tail + host substring.
    let norm = |s: &str| -> String {
        s.trim().trim_end_matches('/').to_ascii_lowercase()
    };
    let n_uri = norm(&uri);
    let n_url = norm(url);
    n_uri == n_url || n_uri.contains(&n_url) || n_url.contains(&n_uri)
}

/// UPnP GetPositionInfo -> TrackURI (empty on failure). Raw TCP SOAP,
/// no dependencies. Port 60006, one request, one response.
pub fn upnp_track_uri(ip: &str) -> String {
    use std::io::{Read, Write};
    let body = concat!(
        r#"<?xml version="1.0"?>"#,
        r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">"#,
        r#"<s:Body><u:GetPositionInfo xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">"#,
        r#"<InstanceID>0</InstanceID></u:GetPositionInfo></s:Body></s:Envelope>"#,
    );
    let req = format!(
        "POST /upnp/control/renderer_dvc/AVTransport HTTP/1.1\r\n         Host: {ip}:60006\r\n         Content-Type: text/xml; charset=\"utf-8\"\r\n         SOAPACTION: \"urn:schemas-upnp-org:service:AVTransport:1#GetPositionInfo\"\r\n         Content-Length: {len}\r\n         Connection: close\r\n\r\n{body}",
        ip = ip, len = body.len(), body = body,
    );
    let mut s = match std::net::TcpStream::connect_timeout(
        &format!("{ip}:60006").parse().ok().unwrap_or_else(|| "0.0.0.0:80".parse().unwrap()),
        std::time::Duration::from_secs(5),
    ) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(6)));
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
    let text = String::from_utf8_lossy(&out);
    let Some(start) = text.find("<TrackURI>") else { return String::new() };
    let rest = &text[start + 11..];
    let Some(end) = rest.find("</TrackURI>") else { return String::new() };
    rest[..end].trim().to_string()
}


/// Parse a `group/get_groups` payload into structured groups.
/// Pure function — no I/O, fully testable.
pub fn parse_groups(objs: &[Value]) -> Vec<HeosGroup> {
    let mut out = Vec::new();
    for o in objs {
        let Some(list) = o.get("payload").and_then(|p| p.as_array()) else { continue };
        for g in list {
            let players = g
                .get("players")
                .and_then(|p| p.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|p| GroupPlayer {
                            name: p.get("name").and_then(|v| v.as_str()).unwrap_or("").into(),
                            pid: p.get("pid").and_then(|v| v.as_i64()).unwrap_or(0),
                            role: p.get("role").and_then(|v| v.as_str()).unwrap_or("").into(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.push(HeosGroup {
                name: g.get("name").and_then(|v| v.as_str()).unwrap_or("").into(),
                gid: g.get("gid").and_then(|v| v.as_i64()).unwrap_or(0),
                players,
            });
        }
    }
    out
}

/// List speaker groups (`group/get_groups`). Empty when nothing is grouped.
pub fn get_groups(ip: &str) -> Vec<HeosGroup> {
    let objs = rpc(ip, &[String::from("heos://group/get_groups")]);
    parse_groups(&objs)
}

/// Build the `set_group` pid list: leader first, then members.
/// Pure function — no I/O, fully testable.
pub fn group_pid_list(leader: i64, members: &[i64]) -> String {
    let mut pids = vec![leader];
    pids.extend_from_slice(members);
    pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",")
}

/// Create or modify a group. `leader` is the group leader; `members` are the
/// other pids. Pass an empty `members` to ungroup the leader.
/// `heos://group/set_group?pid=leader,member1,member2`
pub fn set_group(ip: &str, leader: i64, members: &[i64]) -> bool {
    let list = group_pid_list(leader, members);
    ok(&rpc(
        ip,
        &[format!("heos://group/set_group?pid={list}")],
    ))
}

/// Ungroup every player in the group (leader pid only).
pub fn ungroup(ip: &str, leader: i64) -> bool {
    ok(&rpc(
        ip,
        &[format!("heos://group/set_group?pid={leader}")],
    ))
}

/// `+10` / `-10` / `26` / `26%` against a current level.
pub fn parse_vol_arg(arg: &str, current: i32) -> Option<i32> {
    let a = arg.trim().trim_end_matches('%').trim();
    if a.is_empty() {
        return None;
    }
    if a.starts_with('+') || a.starts_with('-') {
        let d: i32 = a.parse().ok()?;
        return Some((current + d).clamp(0, 100));
    }
    let n: i32 = a.parse().ok()?;
    Some(n.clamp(0, 100))
}

/// Group id for a player, if it sits in a group.
pub fn gid_for_player(groups: &[HeosGroup], pid: i64) -> Option<i64> {
    groups
        .iter()
        .find(|g| g.gid == pid || g.players.iter().any(|p| p.pid == pid))
        .map(|g| g.gid)
}

/// Following → group volume when grouped, else player volume.
pub fn apply_volume(ip: &str, pid: i64, level: i32, following: bool) -> bool {
    let level = level.clamp(0, 100);
    if following {
        if let Some(gid) = gid_for_player(&get_groups(ip), pid) {
            return group_set_volume(ip, gid, level);
        }
    }
    set_volume(ip, pid, level)
}

/// Group volume (0-100). Applies to every member.
pub fn group_set_volume(ip: &str, gid: i64, level: i32) -> bool {
    let level = level.clamp(0, 100);
    ok(&rpc(
        ip,
        &[format!("heos://group/set_volume?gid={gid}&level={level}")],
    ))
}

/// Group mute toggle.
pub fn group_set_mute(ip: &str, gid: i64, on: bool) -> bool {
    ok(&rpc(
        ip,
        &[format!("heos://group/set_mute?gid={gid}&state={}", if on { "on" } else { "off" })],
    ))
}

/// Group play/pause (leader pid).
pub fn group_set_state(ip: &str, leader: i64, state: &str) -> bool {
    set_state(ip, leader, state)
}

/// Record a radio play for the elapsed clock + now displays.
pub fn note_radio(url: &str, title: &str) {
    let dir = crate::paths::cache_dir();
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
    let _ = std::fs::write(dir.join("heos-now.json"), v.to_string());
}

/// Station title for the current radio play, if any.
pub fn last_radio_title() -> Option<String> {
    let raw = std::fs::read_to_string(crate::paths::join_cache("heos-now.json")).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v.get("title").and_then(|t| t.as_str()).map(|s| s.to_string())
}

/// Last cast recorded by any siren (CLI or TUI): (path, epoch secs).
pub fn last_cast() -> Option<(std::path::PathBuf, f64)> {
    let raw = std::fs::read_to_string(crate::paths::join_cache("heos-now.json")).ok()?;
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
    let music = crate::paths::music_dir().to_string_lossy().into_owned();
    let served = ["/tmp/dlna".to_string(), music];
    let abs = path.to_string_lossy().into_owned();
    let in_dlna = served.iter().any(|d| abs.starts_with(d.as_str()));
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
        "DLNA VANGUARDA-DLNA not found — is minidlna on :8200 running?".to_string()
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---------- group_pid_list ----------

    #[test]
    fn group_pid_list_leader_first() {
        assert_eq!(group_pid_list(258957015, &[-105329903]), "258957015,-105329903");
    }

    #[test]
    fn group_pid_list_single_player_is_just_leader() {
        assert_eq!(group_pid_list(258957015, &[]), "258957015");
    }

    #[test]
    fn group_pid_list_multiple_members() {
        assert_eq!(group_pid_list(1, &[2, 3, 4]), "1,2,3,4");
    }

    #[test]
    fn group_pid_list_preserves_negative_pids() {
        assert_eq!(group_pid_list(-105329903, &[258957015]), "-105329903,258957015");
    }

    // ---------- parse_groups ----------

    #[test]
    fn parse_groups_empty_payload() {
        let objs = vec![json!({"heos": {"command": "group/get_groups", "result": "success", "message": ""}, "payload": []})];
        assert!(parse_groups(&objs).is_empty());
    }

    #[test]
    fn parse_groups_no_payload_key() {
        let objs = vec![json!({"heos": {"command": "group/get_groups", "result": "success", "message": ""}})];
        assert!(parse_groups(&objs).is_empty());
    }

    #[test]
    fn parse_groups_single_group() {
        let objs = vec![json!({
            "heos": {"command": "group/get_groups", "result": "success", "message": ""},
            "payload": [{
                "name": "Cave + Vanguarda Office",
                "gid": 258957015,
                "players": [
                    {"name": "Cave", "pid": 258957015, "role": "leader"},
                    {"name": "Vanguarda Office", "pid": -105329903, "role": "member"}
                ]
            }]
        })];
        let groups = parse_groups(&objs);
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.name, "Cave + Vanguarda Office");
        assert_eq!(g.gid, 258957015);
        assert_eq!(g.players.len(), 2);
        assert_eq!(g.players[0].name, "Cave");
        assert_eq!(g.players[0].pid, 258957015);
        assert_eq!(g.players[0].role, "leader");
        assert_eq!(g.players[1].name, "Vanguarda Office");
        assert_eq!(g.players[1].pid, -105329903);
        assert_eq!(g.players[1].role, "member");
    }

    #[test]
    fn parse_groups_multiple_groups() {
        let objs = vec![json!({
            "heos": {"command": "group/get_groups", "result": "success", "message": ""},
            "payload": [
                {"name": "A", "gid": 1, "players": [{"name": "P1", "pid": 1, "role": "leader"}]},
                {"name": "B", "gid": 2, "players": [{"name": "P2", "pid": 2, "role": "leader"}]}
            ]
        })];
        let groups = parse_groups(&objs);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "A");
        assert_eq!(groups[1].name, "B");
    }

    #[test]
    fn parse_groups_missing_fields_default() {
        let objs = vec![json!({
            "heos": {"command": "group/get_groups", "result": "success", "message": ""},
            "payload": [{"name": "X", "gid": 9}]
        })];
        let groups = parse_groups(&objs);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "X");
        assert_eq!(groups[0].gid, 9);
        assert!(groups[0].players.is_empty());
    }

    #[test]
    fn parse_groups_ignores_non_array_payload() {
        let objs = vec![json!({
            "heos": {"command": "group/get_groups", "result": "success", "message": ""},
            "payload": "not-an-array"
        })];
        assert!(parse_groups(&objs).is_empty());
    }

    #[test]
    fn parse_groups_handles_split_json_objects() {
        let objs = vec![
            json!({"heos": {"command": "group/get_groups", "result": "success", "message": ""}, "payload": []}),
            json!({"heos": {"command": "group/get_groups", "result": "success", "message": ""}, "payload": [{"name": "Late", "gid": 5, "players": []}]}),
        ];
        let groups = parse_groups(&objs);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Late");
    }

    #[test]
    fn parse_mute_state_reads_on_off() {
        assert_eq!(
            parse_mute_state("heos://player/get_mute?pid=1&state=on"),
            Some(true)
        );
        assert_eq!(
            parse_mute_state("command=player/get_mute&pid=1&state=off"),
            Some(false)
        );
        assert_eq!(parse_mute_state("pid=1"), None);
        let src = std::fs::read_to_string("src/heos.rs").expect("heos.rs");
        let start = src.find("pub fn get_mute").expect("get_mute");
        let end = src[start..]
            .find("\n///")
            .or_else(|| src[start..].find("\npub fn "))
            .map(|i| start + i)
            .unwrap_or(src.len());
        assert!(
            !src[start..end].contains("apply_msg"),
            "get_mute must not reuse apply_msg"
        );
    }

    #[test]
    fn apply_msg_copies_http_mid() {
        let mut p = HeosPlayer::default();
        apply_msg(
            &mut p,
            &[json!({
                "heos": {"command": "player/get_now_playing_media", "result": "success"},
                "payload": {
                    "song": "Jazz",
                    "mid": "http://example.invalid/stream.aac",
                    "type": "station"
                }
            })],
        );
        assert_eq!(
            p.stream_url.as_deref(),
            Some("http://example.invalid/stream.aac")
        );
        apply_msg(&mut p, &[json!({"payload": {"mid": "s12345"}})]);
        assert_eq!(p.stream_url, None);
        apply_msg(
            &mut p,
            &[json!({"payload": {"mid": "https://ex/s.mp3"}})],
        );
        assert_eq!(p.stream_url.as_deref(), Some("https://ex/s.mp3"));
    }

    fn office() -> HeosPlayer {
        HeosPlayer {
            name: "Vanguarda Office".into(),
            pid: -105329903,
            model: "HEOS 1".into(),
            ip: "192.168.8.184".into(),
            network: "wifi".into(),
            state: None,
            volume: None,
            song: None,
            stream_url: None,
        }
    }

    #[test]
    fn pick_named_matches_speaker_hint() {
        let cave = HeosPlayer { name: "Cave".into(), pid: 1, ip: "192.168.8.185".into(), ..HeosPlayer::default() };
        let ps = vec![cave, office()];
        assert_eq!(pick_named(&ps, "Office").unwrap().ip, "192.168.8.184");
        assert_eq!(pick_named(&ps, "cave").unwrap().name, "Cave");
    }

    #[test]
    fn pick_named_defaults_to_cave() {
        let ps = vec![
            office(),
            HeosPlayer { name: "Cave".into(), ..HeosPlayer::default() },
        ];
        assert_eq!(pick_named(&ps, "").unwrap().name, "Cave");
        assert_eq!(pick_named(&ps, "Office").unwrap().name, "Vanguarda Office");
        let office_only = vec![office()];
        assert_eq!(pick_named(&office_only, "").unwrap().name, "Vanguarda Office");
    }

    #[test]
    fn write_roster_refreshes_ether_cache() {
        let src = std::fs::read_to_string("src/heos.rs").expect("heos.rs");
        let start = src.find("fn write_roster_cache").expect("write_roster_cache");
        let end = src[start..]
            .find("\n/// Live discovery")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(body.contains("write_ether_cache"), "live roster updates Ether");
        assert!(src.contains(".config/ether/heos.json"), "Ether file path");
    }

    #[test]
    fn http_looks_alive_accepts_200_and_audio() {
        assert!(http_looks_alive("HTTP/1.0 200 OK\r\n"));
        assert!(http_looks_alive("HTTP/1.1 200 ok\r\ncontent-type: audio/aac"));
        assert!(http_looks_alive("icy-name: x\r\ncontent-type: audio/mpeg"));
        assert!(!http_looks_alive("HTTP/1.0 400 Bad Request\r\n"));
        assert!(!http_looks_alive("HTTP/1.0 404 Not Found\r\n"));
    }

    #[test]
    fn http_no_retry_on_404() {
        assert!(http_no_retry("HTTP/1.0 404 Not Found\r\n"));
        assert!(!http_no_retry("HTTP/1.0 400 Bad Request\r\n"));
    }

    #[test]
    fn sweep_budget_fits_refresh_deadline() {
        assert!(
            sweep_budget_ms() < 4000,
            "sweep budget {}ms must fit in the 5s now --refresh deadline",
            sweep_budget_ms()
        );
    }

    #[test]
    fn probe_verdict_table() {
        assert_eq!(probe_verdict(1, 0), ProbeVerdict::Ok);
        assert_eq!(probe_verdict(1, 3), ProbeVerdict::Ok);
        assert_eq!(probe_verdict(0, 1), ProbeVerdict::IpMoved);
        assert_eq!(probe_verdict(0, 0), ProbeVerdict::NotOnLan);
        assert_eq!(ProbeVerdict::Ok.exit_code(), 0);
        assert_eq!(ProbeVerdict::NotOnLan.exit_code(), 1);
        assert_eq!(ProbeVerdict::IpMoved.exit_code(), 2);
        assert_eq!(ProbeVerdict::NotOnLan.as_str(), "not-on-lan");
    }

    fn dual_home() -> Vec<IfaceAddr> {
        vec![
            IfaceAddr {
                name: "enp39s0".into(),
                ip: "192.168.11.125".into(),
                prefix: 24,
                is_usb: false,
            },
            IfaceAddr {
                name: "enp49s0f3u1".into(),
                ip: "192.168.48.49".into(),
                prefix: 24,
                is_usb: true,
            },
            IfaceAddr {
                name: "lo".into(),
                ip: "127.0.0.1".into(),
                prefix: 8,
                is_usb: false,
            },
            IfaceAddr {
                name: "tailscale0".into(),
                ip: "100.80.90.117".into(),
                prefix: 32,
                is_usb: false,
            },
        ]
    }

    #[test]
    fn pick_lan_prefers_wired_over_usb_tether() {
        let ip = pick_lan_ipv4(&dual_home(), Some("192.168.48.49"), None);
        assert_eq!(ip.as_deref(), Some("192.168.11.125"));
    }

    #[test]
    fn pick_lan_usb_only_falls_back_to_tether() {
        let only = vec![IfaceAddr {
            name: "enp49s0f3u1".into(),
            ip: "192.168.48.49".into(),
            prefix: 24,
            is_usb: true,
        }];
        assert_eq!(
            pick_lan_ipv4(&only, Some("192.168.48.49"), None).as_deref(),
            Some("192.168.48.49")
        );
    }

    #[test]
    fn pick_lan_honor_prefer_iface() {
        let ip = pick_lan_ipv4(&dual_home(), Some("192.168.48.49"), Some("enp39s0"));
        assert_eq!(ip.as_deref(), Some("192.168.11.125"));
    }

    #[test]
    fn parse_ip_o_addr_reads_prefix() {
        let text = "\
2: enp39s0    inet 192.168.11.125/24 metric 100 brd 192.168.11.255 scope global dynamic enp39s0
6: enp49s0f3u1    inet 192.168.48.49/24 metric 100 scope global dynamic enp49s0f3u1
";
        let v = parse_ip_o_addr(text);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], ("enp39s0".into(), "192.168.11.125".into(), 24));
        assert_eq!(v[1], ("enp49s0f3u1".into(), "192.168.48.49".into(), 24));
    }

    #[test]
    fn rfc1918_table() {
        assert!(is_rfc1918("192.168.11.125"));
        assert!(is_rfc1918("10.0.0.1"));
        assert!(is_rfc1918("172.16.0.1"));
        assert!(is_rfc1918("172.31.255.1"));
        assert!(!is_rfc1918("172.15.0.1"));
        assert!(!is_rfc1918("8.8.8.8"));
        assert!(!is_rfc1918("100.80.90.117"));
    }

    #[test]
    fn sweep_known_has_no_shop_ip_fallback() {
        let src = std::fs::read_to_string("src/heos.rs").expect("heos.rs");
        let start = src.find("fn sweep_known").expect("sweep_known");
        let end = src[start..]
            .find("\nfn tcp_open")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            !body.contains("192.168.8.187") && !body.contains("192.168.8.192"),
            "shop IPs are last-known roster, not hardcoded fallbacks"
        );
        assert!(body.contains("read_roster_stale"), "try last roster on any LAN");
    }

    #[test]
    fn tui_dlna_probe_is_loopback() {
        let src = std::fs::read_to_string("src/tui.rs").expect("tui.rs");
        assert!(
            !src.contains("192.168.8.186"),
            "TUI must not pin shop IP for minidlna"
        );
        assert!(src.contains("127.0.0.1:8200"), "DLNA liveness is local :8200");
    }

    #[test]
    fn arp_flag_complete_parses_proc_net_arp() {
        assert!(arp_flag_complete("0x2"));
        assert!(arp_flag_complete("0x6"));
        assert!(!arp_flag_complete("0x0"));
        assert!(!arp_flag_complete("0x1"));
    }

    #[test]
    fn parse_get_players_reads_payload() {
        let objs = vec![json!({
            "heos": {"command": "player/get_players", "result": "success", "message": ""},
            "payload": [{
                "name": "Cave", "pid": 258957015, "ip": "192.168.8.185",
                "model": "HEOS 5", "network": "wifi"
            }]
        })];
        let ps = parse_get_players(&objs, "127.0.0.1");
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].name, "Cave");
        assert_eq!(ps[0].ip, "192.168.8.185");
        assert_eq!(ps[0].pid, 258957015);
    }

    #[test]
    fn parallel_open_finds_listeners() {
        use std::net::TcpListener;
        let a = TcpListener::bind("127.0.0.1:0").unwrap();
        let b = TcpListener::bind("127.0.0.1:0").unwrap();
        let pa = a.local_addr().unwrap().port();
        let pb = b.local_addr().unwrap().port();
        let addrs = vec![
            ("127.0.0.1".into(), pa),
            ("127.0.0.1".into(), pb),
            ("127.0.0.1".into(), 1),
        ];
        let found = parallel_open(&addrs, 200);
        let ports: Vec<u16> = found.iter().map(|(_, p)| *p).collect();
        assert!(ports.contains(&pa), "missing {pa} in {ports:?}");
        assert!(ports.contains(&pb), "missing {pb} in {ports:?}");
        assert!(!ports.contains(&1));
    }

    #[test]
    fn rpc_at_mock_get_players() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf);
                let body = concat!(
                    r#"{"heos":{"command":"player/get_players","result":"success","message":""},"payload":[{"name":"Cave","pid":1,"ip":"192.168.8.185","model":"HEOS 5","network":"wifi"}]}"#,
                    "\r\n"
                );
                let _ = s.write_all(body.as_bytes());
            }
        });
        thread::sleep(std::time::Duration::from_millis(30));
        let objs = rpc_at("127.0.0.1", port, &["heos://player/get_players".into()]);
        let ps = parse_get_players(&objs, "127.0.0.1");
        assert_eq!(ps.len(), 1, "objs={objs:?}");
        assert_eq!(ps[0].name, "Cave");
    }

    #[test]
    fn parse_vol_arg_relative_and_absolute() {
        assert_eq!(parse_vol_arg("+10", 26), Some(36));
        assert_eq!(parse_vol_arg("-10", 26), Some(16));
        assert_eq!(parse_vol_arg("-40", 26), Some(0));
        assert_eq!(parse_vol_arg("+90", 26), Some(100));
        assert_eq!(parse_vol_arg("38", 0), Some(38));
        assert_eq!(parse_vol_arg("38%", 0), Some(38));
        assert_eq!(parse_vol_arg("nope", 26), None);
        assert_eq!(parse_vol_arg("", 26), None);
    }

    #[test]
    fn gid_for_player_matches_leader_or_member() {
        let g = HeosGroup {
            name: "Office + Cave".into(),
            gid: -105329903,
            players: vec![
                GroupPlayer { name: "Office".into(), pid: -105329903, role: "leader".into() },
                GroupPlayer { name: "Cave".into(), pid: 258957015, role: "member".into() },
            ],
        };
        assert_eq!(gid_for_player(&[g.clone()], -105329903), Some(-105329903));
        assert_eq!(gid_for_player(&[g.clone()], 258957015), Some(-105329903));
        assert_eq!(gid_for_player(&[g], 1), None);
    }
}

#[cfg(test)]
mod url_alive_tests {
    use super::url_alive;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    fn spawn_head400_get200() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut data = Vec::new();
                let mut buf = [0u8; 512];
                loop {
                    match s.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            data.extend_from_slice(&buf[..n]);
                            if data.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let req = String::from_utf8_lossy(&data);
                let body = if req.starts_with("HEAD") {
                    "HTTP/1.0 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
                } else {
                    "HTTP/1.0 200 OK\r\nConnection: close\r\nContent-Type: audio/aac\r\n\r\nxxxx"
                };
                let _ = s.write_all(body.as_bytes());
            }
        });
        thread::sleep(Duration::from_millis(30));
        port
    }

    fn spawn_head404() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.0 404 Not Found\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
            }
        });
        thread::sleep(Duration::from_millis(30));
        port
    }

    #[test]
    fn icecast_head400_get200_is_alive() {
        // OXIGÉNIO shape: HEAD 400, GET 200
        let port = spawn_head400_get200();
        assert!(url_alive(&format!("http://127.0.0.1:{port}/oxigenio_aac")));
    }

    #[test]
    fn head_404_stays_dead() {
        let port = spawn_head404();
        assert!(!url_alive(&format!("http://127.0.0.1:{port}/missing")));
    }
}

#[cfg(test)]
mod url_alive_live_tests {
    // These hit the real network. Slow but honest.
    use super::url_alive;

    #[test]
    #[ignore]
    fn live_bauer_is_alive() {
        // HTTPS passes through (no TLS in std) — confirm_play decides
        assert!(url_alive("https://stream-icy.bauermedia.pt/cidade.mp3"));
    }

    #[test]
    #[ignore]
    fn live_oxigenio_get_is_alive() {
        // HEAD 400, GET 200 — url_alive must retry GET
        assert!(url_alive("http://proic1.evspt.com/oxigenio_aac"));
    }
}

#[cfg(test)]
mod live_lan_tests {
    /// Speakers powered ≠ HEOS CLI on this ethernet LAN.
    /// `cargo test -- --ignored` when Office/Cave should answer :1255 here.
    #[test]
    #[ignore]
    fn live_lan_has_heos_cli() {
        let hits = super::sweep();
        assert!(
            !hits.is_empty(),
            "no HEOS CLI (:1255) on the LAN — AP isolation, guest WiFi, or WiFi sleep. hits={hits:?}"
        );
    }
}
