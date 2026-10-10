//! siren — the Aether's music vessel (faeOS media player).
//!
//! Port slices landed: config, library/fuzzy, mpv IPC, queue/playlist,
//! transport CLI, two-box TUI, audio/HEOS, trove, tags, dir browser.

mod config;
mod heos;
mod library;
mod meta;
mod output;
mod player;
mod radio;
mod playlist;
mod queue;
mod mixer;
mod paths;
mod spectrum;
mod stream;
mod theme;
mod trove;
mod viz;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};

use config::SirenConfig;

#[derive(Parser)]
#[command(name = "siren", version, about = "faeOS music vessel")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Back-compat: `siren <query...>` plays matches (like Python fallback)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    query: Vec<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Play ~/Music matches (or resume)
    Play {
        query: Vec<String>,
    },
    /// Play/pause toggle
    Pause,
    /// Stop playback
    Stop,
    /// Next track
    Next,
    /// Previous track
    Prev,
    /// Seek: +5 | -5 | 30% | mm:ss | seconds
    Seek {
        arg: String,
    },
    /// Now playing
    Now {
        /// Read ~/.cache/siren/now.json instead of live state
        #[arg(long)]
        cached: bool,
        /// Refresh the cache file (for the timer; quiet)
        #[arg(long)]
        refresh: bool,
        /// Emit JSON {label,output,state,vol,age} (for starship-box)
        #[arg(long)]
        json: bool,
    },
    /// Show library + playback status
    Status,
    /// Play the whole library shuffled
    Random,
    /// Configuration: get|set <key> [value]
    Config {
        action: Option<String>,
        key: Option<String>,
        value: Option<String>,
    },
    /// Audio configuration: output, speaker, volume
    Audio {
        #[command(subcommand)]
        sub: Option<AudioCmd>,
    },
    /// Cast a library match to the speaker (DLNA)
    Cast {
        query: Vec<String>,
        #[arg(long, short)]
        speaker: Option<String>,
        /// Queue play-next instead of play-now
        #[arg(long)]
        next: bool,
        /// Append to end of speaker queue
        #[arg(long)]
        append: bool,
    },
    /// Free & legal music (Internet Archive + ccMixter)
    Trove {
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Sleep timer: `siren sleep 30` stops playback in 30min, `off` cancels
    Sleep {
        args: Vec<String>,
    },
    /// Queue: add|list|clear|play|next|remove|move
    Queue {
        args: Vec<String>,
    },
    /// Playlists: save|load|list|remove
    Playlist {
        args: Vec<String>,
    },
    /// Resolve a query against ~/Music (dry)
    Resolve {
        query: Vec<String>,
    },
    /// Debug: analyze a file's spectrum, print bars (no playback)
    Spectrum {
        path: String,
    },
    /// Community radio (radio-browser.info, keyless)
    Radio {
        args: Vec<String>,
    },
    /// Play a video's audio (YouTube etc. via yt-dlp): local or speaker
    Video {
        /// Video URL or "ytsearch:query"
        url: Vec<String>,
    },
}

#[derive(Subcommand)]
enum AudioCmd {
    /// Show current audio routing
    Status,
    /// Set output: local | heos
    Output {
        value: Option<String>,
    },
    /// Set/show preferred speaker
    Speaker {
        value: Option<String>,
    },
    /// Set/show speaker volume (0-100)
    Vol {
        value: Option<String>,
    },
    /// Cast now-playing (or first queue/library track) to the speaker
    Test,
    /// LAN doctor: cached IPs vs a fast /24 :1255 scan
    Probe {
        #[arg(long)]
        json: bool,
    },
    /// Speaker queue: list | play <n> | rm <n> | clear | move <from> <to>
    /// (local queue stays `siren queue …`; this is the HEOS box)
    Queue {
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Siren music → Cave (PCM follow). Headset stays analog.
    Follow {
        /// on | off | run (run = on, then wait for the encoder — systemd)
        action: String,
    },
}



fn cmd_config(action: Option<String>, key: Option<String>, value: Option<String>) -> i32 {
    let mut cfg = SirenConfig::load();
    match action.as_deref() {
        None => {
            for k in SirenConfig::keys() {
                println!("{k} = {}", cfg.get(k).unwrap_or_default());
            }
            0
        }
        Some("get") => match key {
            Some(k) => match cfg.get(&k) {
                Some(v) => {
                    println!("{k} = {v}");
                    0
                }
                None => {
                    eprintln!("unknown key: {k}");
                    1
                }
            },
            None => {
                eprintln!("usage: siren config get <key>");
                1
            }
        },
        Some("set") => match (key, value) {
            (Some(k), Some(v)) => match cfg.set(&k, &v) {
                Ok(shown) => {
                    if let Err(e) = cfg.save() {
                        eprintln!("save failed: {e:#}");
                        return 1;
                    }
                    println!("{k} = {shown}");
                    0
                }
                Err(msg) => {
                    eprintln!("{k}: {msg}");
                    1
                }
            },
            _ => {
                eprintln!("usage: siren config set <key> <value>");
                1
            }
        },
        Some(other) => {
            eprintln!("usage: siren config [get|set] <key> [value] (got {other})");
            1
        }
    }
}

fn use_heos(cfg: &SirenConfig) -> bool {
    cfg.audio_output == "heos"
}

fn cmd_pause(cfg: &SirenConfig) -> i32 {
    if crate::stream::following() {
        let ip = crate::stream::follow_speaker_ip().or_else(|| heos_target(cfg, None).map(|t| t.0));
        match ip {
            Some(ip) => {
                let to = crate::stream::toggle_follow_pause(&ip);
                println!("{to}");
                0
            }
            None => {
                eprintln!("no speaker found");
                1
            }
        }
    } else if use_heos(cfg) {
        match heos_target(cfg, None) {
            Some(t) => {
                let to = heos::toggle(&t.0, t.1);
                println!("{} → {to}", t.2);
                0
            }
            None => {
                eprintln!("no speaker found");
                1
            }
        }
    } else {
        queue::cmd_pause();
        println!("Playback paused / resumed");
        0
    }
}

fn cmd_stop(cfg: &SirenConfig) -> i32 {
    if use_heos(cfg) || crate::stream::following() {
        match heos_target(cfg, None) {
            Some(t) => {
                let uri = crate::stream::track_uri(&t.0);
                heos::set_state(&t.0, t.1, "stop");
                let _ = crate::stream::stop_playback(&t.0);
                if crate::stream::following() && crate::stream::is_follow_uri(&uri) {
                    crate::stream::mark_follow_want_stop();
                } else {
                    let _ = crate::stream::maybe_reclaim();
                }
                println!("{} stopped", t.2);
                0
            }
            None => {
                eprintln!("no speaker found");
                1
            }
        }
    } else {
        queue::cmd_stop();
        println!("Playback stopped");
        0
    }
}

fn cmd_seek(cfg: &SirenConfig, arg: &str) -> i32 {
    if crate::stream::following() {
        eprintln!("seek skipped (live stream)");
        return 0;
    }
    let r = if use_heos(cfg) {
        match heos_target(cfg, None) {
            Some(t) => crate::stream::apply_heos_seek(&t.0, arg),
            None => Err("no speaker found".into()),
        }
    } else {
        player::apply_local_seek(arg)
    };
    match r {
        Ok(m) => {
            println!("{m}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            if e.starts_with("seek skipped") {
                0
            } else {
                1
            }
        }
    }
}

fn cmd_play(cfg: &SirenConfig, rest: &[String]) -> i32 {
    if use_heos(cfg) {
        // speaker routing: resume, or cast the top match
        let tgt = match heos_target(cfg, None) {
            Some(t) => t,
            None => {
                eprintln!("no speaker found");
                return 1;
            }
        };
        if rest.is_empty() {
            heos::set_state(&tgt.0, tgt.1, "play");
            println!("Playing: {}", tgt.2);
            return 0;
        }
        return cmd_cast(cfg, rest, None, false, false);
    }
    if rest.is_empty()
        && player::alive()
        && player::get("playlist-count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            > 0
    {
        player::send(&serde_json::json!(["set_property", "pause", false]));
        if queue::now_label().is_empty() {
            player::send(&serde_json::json!(["playlist-play-index", 0]));
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
        let label = queue::now_label();
        if label.is_empty() {
            println!("Playback resumed");
        } else {
            println!("Playing: {label}");
        }
        return 0;
    }
    if rest.len() == 1 {
        if let Some(hit) = playlist::find(&rest[0]) {
            return cmd_playlist_load(&hit);
        }
    }
    let files = library::resolve_play_args(
        cfg,
        &rest.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    );
    if files.is_empty() {
        println!("No tracks match: {}", rest.join(" "));
        return 1;
    }
    let paths: Vec<String> = files.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    if queue::start_playlist(cfg, &paths, false) {
        0
    } else {
        1
    }
}

fn cmd_playlist_load(name: &str) -> i32 {
    let tracks = playlist::load(name);
    if tracks.is_empty() {
        println!("Playlist not found: {name}");
        return 1;
    }
    // mirror Python play_playlist: replace QUEUE, play from 0
    queue::replace(tracks);
    let cfg = SirenConfig::load();
    match output::play_from(&cfg, 0) {
        Ok(_) => {
            println!("Playing playlist: {name}");
            0
        }
        Err(e) => {
            println!("{e}");
            1
        }
    }
}

fn cmd_queue(rest: &[String]) -> i32 {
    let cfg = SirenConfig::load();
    let sub = rest.first().map(|s| s.to_lowercase()).unwrap_or_default();
    match sub.as_str() {
        "add" | "a" => {
            let args: Vec<String> = rest[1..].iter().map(|s| s.to_string()).collect();
            let prepend = args.iter().any(|a| a == "--next" || a == "-n");
            let query: Vec<String> = args
                .into_iter()
                .filter(|a| a != "--next" && a != "-n")
                .collect();
            queue::cli_add(&cfg, &query.join(" "), prepend)
        }
        "list" | "l" | "ls" => queue::cli_list(),
        "clear" | "c" => queue::cli_clear(),
        "play" | "p" => match output::play_from(&cfg, 0) {
            Ok(m) => {
                println!("{m}");
                0
            }
            Err(e) => {
                println!("{e}");
                1
            }
        },
        "next" | "n" => {
            if use_heos(&cfg) {
                match heos_target(&cfg, None) {
                    Some(t) => {
                        heos::play_next(&t.0, t.1);
                        println!("Next: {}", t.2);
                        0
                    }
                    None => {
                        eprintln!("no speaker found");
                        1
                    }
                }
            } else {
                queue::cli_queue_next()
            }
        }
        "remove" | "rm" | "del" | "d" => {
            if rest.len() > 1 {
                queue::cli_remove(&rest[1])
            } else {
                println!("Usage: siren queue remove <index>");
                1
            }
        }
        "move" | "mv" => {
            if rest.len() >= 3 {
                queue::cli_move(&rest[1], &rest[2])
            } else {
                println!("Usage: siren queue move <from> <to>");
                1
            }
        }
        _ => {
            println!("Queue commands: add, list, clear, play, next, remove, move");
            1
        }
    }
}

fn cmd_playlist(rest: &[String]) -> i32 {
    let sub = rest.first().map(|s| s.to_lowercase()).unwrap_or_default();
    match sub.as_str() {
        "save" => {
            if rest.len() < 2 {
                println!("Usage: siren playlist save <name>");
                return 1;
            }
            // snapshot current queue
            let items = queue::snapshot();
            if playlist::save(&rest[1], &items) {
                println!("Saved playlist: {}", rest[1]);
                0
            } else {
                println!("Failed to save playlist.");
                1
            }
        }
        "load" => {
            if rest.len() < 2 {
                println!("Usage: siren playlist load <name>");
                return 1;
            }
            let name = playlist::find(&rest[1]).unwrap_or_else(|| rest[1].clone());
            cmd_playlist_load(&name)
        }
        "list" | "ls" => {
            let names = playlist::names();
            if names.is_empty() {
                println!("(no saved playlists)");
                return 0;
            }
            for n in names {
                println!("  {n}");
            }
            0
        }
        "remove" | "rm" | "del" => {
            if rest.len() < 2 {
                println!("Usage: siren playlist remove <name>");
                return 1;
            }
            if playlist::delete(&rest[1]) {
                println!("Deleted playlist: {}", rest[1]);
                0
            } else {
                println!("Playlist not found: {}", rest[1]);
                1
            }
        }
        _ => {
            println!("Playlist commands: save, load, list, remove");
            1
        }
    }
}

/// Sleep timer via a transient user timer (no daemon).
/// `siren sleep 30` → stop in 30min · `siren sleep off` → cancel ·
/// `siren sleep` → remaining.
fn cmd_sleep(args: &[String]) -> i32 {
    use std::process::Command;
    let unit = "siren-sleep";
    let ctl = |a: &[&str]| -> Option<String> {
        Command::new("systemctl")
            .arg("--user")
            .args(a)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    match args.first().map(|s| s.to_lowercase()).as_deref() {
        None | Some("") => {
            // status: time left on the transient timer
            let out = ctl(&["list-timers", "--no-legend", "--no-pager"]).unwrap_or_default();
            let line = out.lines().find(|l| l.contains(unit));
            match line {
                Some(l) => {
                    // NEXT is "Dow YYYY-MM-DD HH:MM:SS TZ" (4 tokens) or "n/a";
                    // LEFT follows ("44min" transient, "44min left" persistent)
                    let toks: Vec<&str> = l.split_whitespace().collect();
                    let left = if toks.first() == Some(&"n/a") {
                        toks.get(1).unwrap_or(&"?").to_string()
                    } else if toks.len() > 4 {
                        let mut s = toks[4].to_string();
                        if toks.get(5) == Some(&"left") {
                            s.push_str(" left");
                        }
                        s
                    } else {
                        "?".into()
                    };
                    println!("sleep: stops in {left}");
                    0
                }
                None => {
                    println!("sleep: off");
                    0
                }
            }
        }
        Some("off") | Some("cancel") | Some("stop") => {
            ctl(&["stop", &format!("{unit}.timer")]);
            ctl(&["reset-failed", &format!("{unit}.*")]);
            println!("sleep: off");
            0
        }
        Some(m) => {
            let mins: f64 = match m.parse() {
                Ok(v) if v > 0.0 => v,
                _ => {
                    eprintln!("usage: siren sleep [minutes|off]");
                    return 2;
                }
            };
            let siren = crate::paths::home_dir()
                .join("bin")
                .join("siren")
                .to_string_lossy()
                .into_owned();
            // cancel any previous timer first
            ctl(&["stop", &format!("{unit}.timer")]);
            let st = Command::new("systemd-run")
                .args([
                    "--user",
                    "--quiet",
                    &format!("--on-active={mins}min"),
                    &format!("--unit={unit}"),
                    &siren,
                    "stop",
                ])
                .status();
            match st {
                Ok(s) if s.success() => {
                    println!("sleep: stops in {mins}min");
                    0
                }
                _ => {
                    eprintln!("sleep: systemd-run failed (user manager up?)");
                    1
                }
            }
        }
    }
}

fn shellexpand(p: &str) -> String {
    crate::paths::expand_home(p).to_string_lossy().into_owned()
}

fn now_cache_path() -> std::path::PathBuf {
    crate::paths::join_cache("now.json")
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn heos_snap(p: &heos::HeosPlayer, stale: bool) -> serde_json::Value {
    serde_json::json!({
        "label": format!("{} — {}", p.name, p.state.as_deref().unwrap_or("?")),
        "output": "heos",
        "state": p.state,
        "vol": p.volume,
        "name": p.name,
        "ip": p.ip,
        "ts": now_epoch(),
        "stale": stale,
    })
}

/// Same shape as `now_snapshot` but never touches the network: reads
/// the roster file even after TTL. Used when the refresh deadline
/// expires (speakers off the network) so the command still answers
/// instead of hanging — and still names Cave/Office.
fn now_snapshot_cached(cfg: &SirenConfig) -> serde_json::Value {
    // Follow is Cave's transport even when audio_output is local.
    // Must not recurse into now_snapshot (that path hits the network,
    // and heos_target-fail would loop).
    if crate::stream::following() || cfg.audio_output == "heos" {
        if let Some(players) = heos::read_roster_stale() {
            if let Some(p) = heos::pick_named(&players, &cfg.audio_speaker) {
                return heos_snap(p, true);
            }
        }
        return serde_json::json!({
            "label": "speakers unreachable", "output": "heos",
            "state": null, "vol": null, "name": null, "ip": null,
            "ts": now_epoch(), "stale": true,
        });
    }
    // local, follow off: mpv state is on a unix socket, cheap
    now_snapshot(cfg)
}

/// Gather current playback (output-aware) for the cache / JSON.
fn now_snapshot(cfg: &SirenConfig) -> serde_json::Value {
    // Follow is Cave's transport even when audio_output is local (mpv rides Master).
    if crate::stream::following() || cfg.audio_output == "heos" {
        if let Some(t) = heos_target(cfg, None) {
            let mut ps = vec![heos::HeosPlayer {
                name: t.2.clone(),
                pid: t.1,
                ip: t.0.clone(),
                ..Default::default()
            }];
            heos::enrich(&mut ps);
            let (uri, _, _) = crate::stream::position_info(&t.0);
            let upnp = crate::stream::transport_state(&t.0);
            let published = crate::stream::publish_state(ps[0].state.as_deref(), &upnp);
            let suffix = crate::stream::now_suffix(
                &uri,
                ps[0].song.as_deref(),
                published.as_deref(),
            );
            let stale = published.is_none();
            let mut snap = heos_snap(&ps[0], stale);
            snap["state"] = published
                .clone()
                .map(|s| serde_json::Value::String(s))
                .unwrap_or(serde_json::Value::Null);
            snap["label"] = serde_json::Value::String(format!("{} — {suffix}", ps[0].name));
            if crate::stream::is_follow_uri(&uri) {
                snap["uri"] = serde_json::json!(uri);
            }
            return snap;
        }
        return now_snapshot_cached(cfg);
    }
    let label = queue::now_label();
    let state = if !player::alive() {
        "idle"
    } else if player::get_bool("pause", false) {
        "pause"
    } else if label.is_empty() {
        "stop"
    } else {
        "play"
    };
    let vol = player::get("volume").and_then(|v| v.as_i64());
    serde_json::json!({
        "label": label, "output": "local",
        "state": state, "vol": vol, "ts": now_epoch(),
    })
}

/// `siren now --refresh`: rewrite the cache, quiet. Timer calls this.
/// Run a closure on a worker thread with a hard deadline. Returns
/// None if it does not finish in time. The network path (a full /24
/// sweep, 254 hosts) can take minutes when speakers are down — a
/// status command must never hang on it.
fn with_deadline<T, F>(secs: u64, f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(std::time::Duration::from_secs(secs)).ok()
}

fn cmd_now_refresh(cfg: &SirenConfig, json: bool) -> i32 {
    // keep the roster cache warm too (best effort, never fails the
    // refresh) — but bounded: if the speakers are off the network the
    // sweep would otherwise hang forever.
    let cfg2 = cfg.clone();
    let snap = with_deadline(5, move || {
        heos::refresh_roster();
        now_snapshot(&cfg2)
    });
    let snap = match snap {
        Some(s) => s,
        None => {
            // deadline hit — fall back to whatever we already know
            eprintln!("siren: speakers unreachable (using cached state)");
            now_snapshot_cached(cfg)
        }
    };
    if let Some(parent) = now_cache_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = now_cache_path().with_extension("json.tmp");
    match serde_json::to_string(&snap) {
        Ok(text) => {
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, now_cache_path());
            }
        }
        Err(_) => return 1,
    }
    if json {
        println!("{snap}");
    }
    let _ = crate::stream::maybe_reclaim();
    0
}

/// Live `siren now`: try the speaker, fall back to stale roster names.
fn cmd_now_live(cfg: &SirenConfig, json: bool) -> i32 {
    let cfg2 = cfg.clone();
    let snap = with_deadline(5, move || now_snapshot(&cfg2)).unwrap_or_else(|| now_snapshot_cached(cfg));
    if json {
        println!("{snap}");
        return 0;
    }
    let label = snap.get("label").and_then(|x| x.as_str()).unwrap_or("");
    if label.is_empty() {
        println!("Not playing");
    } else {
        println!("{label}");
    }
    0
}

fn print_heos_status(snap: &serde_json::Value) {
    let stale = snap.get("stale").and_then(|x| x.as_bool()).unwrap_or(false);
    let state = snap
        .get("state")
        .and_then(|x| x.as_str())
        .unwrap_or(if stale { "unreachable" } else { "?" });
    println!("status: {state}");
    let name = snap.get("name").and_then(|x| x.as_str()).unwrap_or("");
    let ip = snap.get("ip").and_then(|x| x.as_str()).unwrap_or("");
    if !name.is_empty() {
        if ip.is_empty() {
            println!("speaker: {name}");
        } else {
            println!("speaker: {name} ({ip})");
        }
    } else {
        let label = snap.get("label").and_then(|x| x.as_str()).unwrap_or("no speaker found");
        println!("speaker: {label}");
    }
    match snap.get("vol").and_then(|x| x.as_i64()) {
        Some(v) => println!("vol:    {v}%"),
        None => println!("vol:    ?"),
    }
    if stale {
        println!("note:   speakers unreachable (cached roster)");
    }
}

fn cmd_status(cfg: &SirenConfig) -> i32 {
    if use_heos(cfg) || crate::stream::following() {
        let cfg2 = cfg.clone();
        let snap = with_deadline(5, move || now_snapshot(&cfg2)).unwrap_or_else(|| now_snapshot_cached(cfg));
        print_heos_status(&snap);
        return 0;
    }
    if !player::alive() {
        let lib = library::scan_library(cfg);
        println!("status: idle (no mpv)");
        println!("library tracks: {}", lib.len());
        0
    } else {
        for line in queue::status_lines(cfg) {
            println!("{line}");
        }
        0
    }
}

/// `siren now --cached [--json]`: instant read for prompts/boxes.
fn cmd_now_cached(json: bool) -> i32 {
    let raw = std::fs::read_to_string(now_cache_path()).unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::json!({}));
    let ts = v.get("ts").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let age = (now_epoch() - ts).max(0.0);
    if json {
        let mut m = v.as_object().cloned().unwrap_or_default();
        m.insert("age".into(), serde_json::json!(age));
        println!("{}", serde_json::Value::Object(m));
        return 0;
    }
    let label = v.get("label").and_then(|x| x.as_str()).unwrap_or("");
    if label.is_empty() {
        println!("Not playing");
    } else {
        println!("{label}");
    }
    0
}

fn heos_target(cfg: &SirenConfig, hint: Option<&str>) -> Option<(String, i64, String)> {
    let h = hint.map(|s| s.to_string()).or_else(|| {
        if cfg.audio_speaker.trim().is_empty() {
            None
        } else {
            Some(cfg.audio_speaker.clone())
        }
    });
    heos::resolve(h.as_deref()).map(|(ip, pid, p)| (ip, pid, p.name))
}

fn cmd_audio_probe(json: bool) -> i32 {
    let report = heos::run_probe();
    if json {
        let cached: Vec<serde_json::Value> = report
            .cached
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name, "ip": r.ip,
                    "arp": r.arp, "cli": r.cli, "upnp": r.upnp,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "verdict": report.verdict.as_str(),
                "cached": cached,
                "lan_1255": report.lan_1255,
                "exit": report.verdict.exit_code(),
            })
        );
        return report.verdict.exit_code();
    }
    if report.cached.is_empty() {
        println!("cached:  (none)");
    }
    for r in &report.cached {
        println!(
            "cached:  {}  {}  arp={}  1255={}  60006={}",
            r.name,
            r.ip,
            if r.arp { "ok" } else { "dead" },
            if r.cli { "up" } else { "down" },
            if r.upnp { "up" } else { "down" },
        );
    }
    if report.lan_1255.is_empty() {
        println!("lan 1255: (none)");
    } else {
        println!("lan 1255: {}", report.lan_1255.join(" "));
    }
    println!("verdict: {}", report.verdict.as_str());
    report.verdict.exit_code()
}

fn cmd_audio_follow(cfg: &SirenConfig, action: &str) -> i32 {
    match action.trim().to_ascii_lowercase().as_str() {
        "on" | "start" | "1" | "run" => {
            let wait = action.trim().eq_ignore_ascii_case("run");
            let report = heos::run_probe();
            if report.verdict == heos::ProbeVerdict::NotOnLan {
                let sink = crate::stream::default_sink()
                    .unwrap_or_else(|| crate::stream::FALLBACK_SINK.to_string());
                eprintln!("follow: not-on-lan — leaving headset {sink}");
                return 1;
            }
            let tgt = match heos_target(cfg, None) {
                Some(t) => t,
                None => {
                    eprintln!("no speaker found");
                    return 1;
                }
            };
            if crate::stream::follow_on(crate::stream::DEFAULT_PORT, &tgt.0) {
                let def = crate::stream::default_sink()
                    .unwrap_or_else(|| "headset".into());
                println!(
                    "follow on → {} ({})  music={}  default={}",
                    tgt.2,
                    tgt.0,
                    crate::stream::MASTER,
                    def
                );
                if wait {
                    crate::stream::wait_while_encoder();
                }
                0
            } else {
                1
            }
        }
        "off" | "stop" | "0" => {
            crate::stream::follow_off(true);
            println!("follow off");
            0
        }
        other => {
            eprintln!("usage: siren audio follow on|off|run  (got {other})");
            1
        }
    }
}

fn cmd_audio(sub: Option<AudioCmd>) -> i32 {
    let mut cfg = SirenConfig::load();
    match sub {
        None | Some(AudioCmd::Status) => {
            println!("output:  {}", cfg.audio_output);
            println!("speaker: {}", cfg.audio_speaker);
            let cfg2 = cfg.clone();
            let snap = with_deadline(5, move || now_snapshot(&cfg2)).unwrap_or_else(|| now_snapshot_cached(&cfg));
            let name = snap.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let ip = snap.get("ip").and_then(|x| x.as_str()).unwrap_or("");
            let state = snap.get("state").and_then(|x| x.as_str()).unwrap_or("?");
            let vol = snap
                .get("vol")
                .and_then(|x| x.as_i64())
                .map(|v| format!("{v}%"))
                .unwrap_or_else(|| "?".into());
            let stale = snap.get("stale").and_then(|x| x.as_bool()).unwrap_or(false);
            if name.is_empty() {
                println!("live:    no speaker found");
            } else if stale {
                println!("live:    {name} ({ip}) unreachable (cached) vol={vol}");
            } else {
                println!("live:    {name} ({ip}) state={state} vol={vol}");
            }
            println!("{}", crate::stream::health_line());
            0
        }
        Some(AudioCmd::Output { value }) => match value {
            None => {
                println!("{}", cfg.audio_output);
                0
            }
            Some(v) => match cfg.set("audio_output", &v) {
                Ok(shown) => {
                    if cfg.save().is_err() {
                        eprintln!("save failed");
                        return 1;
                    }
                    println!("output = {shown}");
                    0
                }
                Err(msg) => {
                    eprintln!("audio_output: {msg}");
                    1
                }
            },
        },
        Some(AudioCmd::Speaker { value }) => match value {
            None => {
                println!("{}", cfg.audio_speaker);
                0
            }
            Some(v) => match cfg.set("audio_speaker", &v) {
                Ok(shown) => {
                    if cfg.save().is_err() {
                        eprintln!("save failed");
                        return 1;
                    }
                    println!("speaker = {shown}");
                    0
                }
                Err(msg) => {
                    eprintln!("audio_speaker: {msg}");
                    1
                }
            },
        },
        Some(AudioCmd::Vol { value }) => {
            let tgt = match heos_target(&cfg, None) {
                Some(t) => t,
                None => {
                    eprintln!("no speaker found");
                    return 1;
                }
            };
            match value {
                None => {
                    let mut ps = vec![heos::HeosPlayer {
                        name: tgt.2.clone(),
                        pid: tgt.1,
                        ip: tgt.0.clone(),
                        ..Default::default()
                    }];
                    heos::enrich(&mut ps);
                    println!(
                        "{} volume: {}",
                        tgt.2,
                        ps[0].volume.map(|v| format!("{v}%")).unwrap_or("?".into())
                    );
                    0
                }
                Some(v) => {
                    let mut ps = vec![heos::HeosPlayer {
                        name: tgt.2.clone(),
                        pid: tgt.1,
                        ip: tgt.0.clone(),
                        ..Default::default()
                    }];
                    heos::enrich(&mut ps);
                    let cur = ps[0].volume.unwrap_or(0);
                    let Some(lvl) = heos::parse_vol_arg(&v, cur) else {
                        eprintln!("volume must be 0-100 or +N/-N");
                        return 1;
                    };
                    if heos::apply_volume(&tgt.0, tgt.1, lvl, crate::stream::following()) {
                        println!("{} volume → {}%", tgt.2, lvl);
                        0
                    } else {
                        eprintln!("volume failed");
                        1
                    }
                }
            }
        }
        Some(AudioCmd::Queue { args }) => {
            let cfg = SirenConfig::load();
            cmd_speaker_queue(&cfg, &args)
        },
        Some(AudioCmd::Probe { json }) => cmd_audio_probe(json),
        Some(AudioCmd::Follow { action }) => cmd_audio_follow(&cfg, &action),
        Some(AudioCmd::Test) => {
            // now-playing → queue top → library top
            let pick = {
                let p = player::now_path();
                if !p.is_empty() {
                    Some(std::path::PathBuf::from(p))
                } else {
                    None
                }
            }
            .or_else(|| queue::snapshot().first().map(|it| std::path::PathBuf::from(&it.path)))
            .or_else(|| library::scan_library(&cfg).first().cloned());
            match pick {
                Some(p) => {
                    let tgt = match heos_target(&cfg, None) {
                        Some(t) => t,
                        None => {
                            eprintln!("no speaker found");
                            return 1;
                        }
                    };
                    match heos::dlna_cast(&tgt.0, tgt.1, &p, 1) {
                        Ok(()) => {
                            heos::note_cast(&p);
                            println!("cast {} → {}", p.display(), tgt.2);
                            0
                        }
                        Err(e) => {
                            eprintln!("cast failed: {e}");
                            1
                        }
                    }
                }
                None => {
                    eprintln!("nothing to cast");
                    1
                }
            }
        }
    }
}

fn cmd_cast(
    cfg: &SirenConfig,
    query: &[String],
    speaker: Option<&str>,
    next: bool,
    append: bool,
) -> i32 {
    let hits = library::resolve_play_args(
        cfg,
        &query.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    );
    if hits.is_empty() {
        println!("No tracks match: {}", query.join(" "));
        return 1;
    }
    let top = &hits[0];
    let tgt = match heos_target(cfg, speaker) {
        Some(t) => t,
        None => {
            eprintln!("no speaker found");
            return 1;
        }
    };
    let aid = if next { 2 } else if append { 3 } else { 1 };
    match heos::dlna_cast(&tgt.0, tgt.1, top, aid) {
        Ok(()) => {
            if aid == 1 {
                heos::note_cast(top);
            }
            let how = match aid {
                2 => " (play-next)",
                3 => " (appended)",
                _ => "",
            };
            println!("cast {} → {}{}", top.display(), tgt.2, how);
            0
        }
        Err(e) => {
            eprintln!("cast failed: {e}");
            1
        }
    }
}

/// `siren audio queue …` — the SPEAKER's queue (local stays `siren queue`).
/// Indices are 1-based into the current listing.
fn cmd_speaker_queue(cfg: &SirenConfig, args: &[String]) -> i32 {
    // optional --speaker/-s NAME anywhere in args
    let mut hint: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if a == "--speaker" || a == "-s" {
            hint = it.next().cloned();
        } else if let Some(v) = a.strip_prefix("--speaker=") {
            hint = Some(v.to_string());
        } else {
            rest.push(a.clone());
        }
    }
    let tgt = match heos_target(cfg, hint.as_deref()) {
        Some(t) => t,
        None => {
            eprintln!("no speaker found");
            return 1;
        }
    };
    let args = rest;
    let sub = args.first().map(|s| s.to_lowercase()).unwrap_or_default();
    if sub.is_empty() || sub == "list" || sub == "ls" {
        let items = heos::get_queue(&tgt.0, tgt.1);
        if items.is_empty() {
            println!("(speaker queue empty)");
            return 0;
        }
        for (i, it) in items.iter().enumerate() {
            let artist = if it.artist.is_empty() { "".into() } else { format!(" — {}", it.artist) };
            println!("  {:3}. {}{}", i + 1, it.song, artist);
        }
        return 0;
    }
    let items = heos::get_queue(&tgt.0, tgt.1);
    let qid_at = |n: usize| -> Option<i64> {
        if n >= 1 && n <= items.len() {
            Some(items[n - 1].qid)
        } else {
            None
        }
    };
    match sub.as_str() {
        "play" | "p" => {
            let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            match qid_at(n) {
                Some(qid) if heos::play_queue(&tgt.0, tgt.1, qid) => {
                    println!("playing #{} on {}", n, tgt.2);
                    0
                }
                _ => {
                    eprintln!("usage: siren audio queue play <1-{}>", items.len().max(1));
                    1
                }
            }
        }
        "rm" | "remove" | "del" | "d" => {
            let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            match qid_at(n) {
                Some(qid) if heos::remove_from_queue(&tgt.0, tgt.1, qid) => {
                    println!("removed #{} from {}", n, tgt.2);
                    0
                }
                _ => {
                    eprintln!("usage: siren audio queue rm <1-{}>", items.len().max(1));
                    1
                }
            }
        }
        "clear" | "c" => {
            if heos::clear_queue(&tgt.0, tgt.1) {
                println!("{} queue cleared", tgt.2);
                0
            } else {
                eprintln!("clear failed");
                1
            }
        }
        "move" | "mv" => {
            let f: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let t: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            match (qid_at(f), qid_at(t)) {
                (Some(sqid), Some(dqid)) if heos::move_queue_item(&tgt.0, tgt.1, sqid, dqid) => {
                    println!("moved #{} → #{} on {}", f, t, tgt.2);
                    0
                }
                _ => {
                    eprintln!("usage: siren audio queue move <from> <to>");
                    1
                }
            }
        }
        _ => {
            eprintln!("usage: siren audio queue [list|play <n>|rm <n>|clear|move <from> <to>]");
            1
        }
    }
}

/// Split `--format EXT` / `--format=EXT` out of trove args.
fn trove_format_arg(args: &[String]) -> (Option<String>, Vec<String>) {
    let mut format: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if a == "--format" {
            if let Some(v) = it.next() {
                format = Some(v.to_lowercase());
            }
        } else if let Some(v) = a.strip_prefix("--format=") {
            format = Some(v.to_lowercase());
        } else {
            rest.push(a.clone());
        }
    }
    (format, rest)
}

fn cmd_trove(args: &[String]) -> i32 {
    let (format, args) = trove_format_arg(args);
    if args.is_empty() {
        return trove::run_trove(trove::PAGE_SIZE, &[], None, format);
    }
    let sub = args[0].to_lowercase();
    if sub == "get" || sub == "g" {
        if args.len() < 2 {
            eprintln!("usage: siren trove get <identifier> [--format EXT|all]");
            return 2;
        }
        return trove::run_get(&args[1], format);
    }
    if sub == "about" || sub == "help" || sub == "-h" || sub == "--help" {
        for line in trove::about_text() {
            println!("  {line}");
        }
        return 0;
    }
    // optional leading count, then kind detection inside run_trove
    let mut words: Vec<String> = args;
    let mut n = trove::PAGE_SIZE;
    if let Some(first) = words.first() {
        if let Ok(v) = first.parse::<u32>() {
            n = v;
            words.remove(0);
        }
    }
    trove::run_trove(n, &words, None, format)
}

/// `siren radio …` — community stations, same output channel as music.
fn cmd_radio(args: &[String]) -> i32 {
    let sub = args.first().map(|s| s.to_lowercase()).unwrap_or_default();
    match sub.as_str() {
        "countries" | "country" | "c" => match radio::countries() {
            Ok(list) => {
                for c in &list {
                    println!("  {} ({})", c.name, c.count);
                }
                0
            }
            Err(e) => {
                eprintln!("radio countries failed: {e}");
                1
            }
        },
        "search" | "s" => {
            let q = args[1..].join(" ");
            if q.trim().is_empty() {
                eprintln!("usage: siren radio search <words>");
                return 2;
            }
            match radio::search(&q, radio::PAGE_SIZE, 0) {
                Ok(stations) if stations.is_empty() => {
                    println!("No stations match: {q}");
                    1
                }
                Ok(stations) => {
                    for (i, s) in stations.iter().enumerate() {
                        let bit = if s.bitrate > 0 {
                            format!(" {}k", s.bitrate)
                        } else {
                            String::new()
                        };
                        println!("  {:>2}.  {}{}  [{}]", i + 1, s.name, bit, s.country);
                    }
                    0
                }
                Err(e) => {
                    eprintln!("radio search failed: {e}");
                    1
                }
            }
        }
        "stations" | "top" | "t" => {
            let country = args.get(1).cloned().or_else(radio::last_country);
            let Some(country) = country else {
                eprintln!("usage: siren radio stations <country> (or pick one in the TUI)");
                return 2;
            };
            match radio::by_country(&country, radio::PAGE_SIZE, 0) {
                Ok(stations) if stations.is_empty() => {
                    println!("No stations in: {country}");
                    1
                }
                Ok(stations) => {
                    for (i, s) in stations.iter().enumerate() {
                        println!("  {:>2}.  {}  [{}]", i + 1, s.name, s.codec);
                    }
                    0
                }
                Err(e) => {
                    eprintln!("radio stations failed: {e}");
                    1
                }
            }
        }
        "play" | "p" => {
            let q = args[1..].join(" ");
            if q.trim().is_empty() {
                eprintln!("usage: siren radio play <words>");
                return 2;
            }
            let stations = match radio::search_many(&q, 60) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("radio search failed: {e}");
                    return 1;
                }
            };
            let Some(st) = radio::best_match(&q, &stations) else {
                println!("No stations match: {q}");
                return 1;
            };
            queue::replace(vec![radio::to_queue_item(&st)]);
            println!("tuning: {} ({})", st.name, st.url);
            let cfg = SirenConfig::load();
            match output::play_from(&cfg, 0) {
                Ok(m) => {
                    println!("{m}");
                    0
                }
                Err(e) => {
                    println!("{e}");
                    1
                }
            }
        }
        "fav" | "favs" | "f" => {
            if args.len() == 1 {
                let favs = radio::favs();
                if favs.is_empty() {
                    println!("(no favorites — siren radio fav <words>)");
                    return 0;
                }
                for (i, s) in favs.iter().enumerate() {
                    println!("  {:>2}.  {}  [{}]", i + 1, s.name, s.country);
                }
                return 0;
            }
            let q = args[1..].join(" ");
            let stations = match radio::search_many(&q, 60) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("radio search failed: {e}");
                    return 1;
                }
            };
            let Some(st) = radio::best_match(&q, &stations) else {
                println!("No stations match: {q}");
                return 1;
            };
            if radio::toggle_fav(&st) {
                println!("fav: {}", st.name);
            } else {
                println!("unfaved: {}", st.name);
            }
            0
        }
        "add" | "url" => {
            let Some(url) = args.get(1).cloned() else {
                eprintln!("usage: siren radio add <stream-url> [name]");
                return 2;
            };
            if !url.starts_with("http://") && !url.starts_with("https://") {
                eprintln!("need an http(s) stream url");
                return 2;
            }
            let name = args[2..].join(" ");
            let st = radio::add_url(&url, &name);
            println!("fav: {} ({})", st.name, st.url);
            0
        }
        "about" | "help" | "-h" | "--help" | "" => {
            for line in radio::about_text() {
                println!("  {line}");
            }
            0
        }
        _ => {
            // bare words → search
            let q = args.join(" ");
            match radio::search(&q, radio::PAGE_SIZE, 0) {
                Ok(stations) if stations.is_empty() => {
                    println!("No stations match: {q}");
                    1
                }
                Ok(stations) => {
                    for (i, s) in stations.iter().enumerate() {
                        println!("  {:>2}.  {}  [{}]", i + 1, s.name, s.country);
                    }
                    0
                }
                Err(e) => {
                    eprintln!("radio search failed: {e}");
                    1
                }
            }
        }
    }
}

/// `siren video <url|ytsearch:query>` — fetch audio, cache, play it
/// through the output channel (local mpv, or DLNA-cast to the speaker).
/// Live pages can't cast (speaker needs finite files) — verified.
fn cmd_video(args: &[String]) -> i32 {
    let query = args.join(" ");
    if query.trim().is_empty() {
        eprintln!("usage: siren video <url> | <ytsearch:query>");
        return 2;
    }
    let is_url = query.starts_with("http://") || query.starts_with("https://");
    if !is_url && !query.starts_with("ytsearch:") {
        eprintln!("need a url, or prefix a search with ytsearch:");
        return 2;
    }
    println!("fetching audio…");
    let title = video_title(&query);
    let out = std::process::Command::new("yt-dlp")
        .args([
            "-x",
            "--audio-format",
            "mp3",
            "--audio-quality",
            "5",
            "--no-playlist",
            "--print",
            "after_move:filepath",
            "--paths",
            crate::paths::cache_dir().join("videos").to_str().unwrap_or("."),
            &query,
        ])
        .output()
        .map_err(|e| format!("yt-dlp missing/failed: {e}"));
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    if !out.status.success() {
        eprintln!(
            "yt-dlp failed: {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("?")
        );
        return 1;
    }
    let path = String::from_utf8_lossy(&out.stdout)
        .lines()
        .last()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if path.is_empty() || !std::path::Path::new(&path).exists() {
        eprintln!("no file after download");
        return 1;
    }
    let title = title.unwrap_or_else(|| {
        std::path::Path::new(&path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone())
    });
    let (artist, _) = crate::meta::tags_for_sync(&path);
    queue::replace(vec![queue::QueueItem {
        path: path.clone(),
        display: title.clone(),
        title,
        artist,
        duration: 0.0,
    }]);
    let cfg = SirenConfig::load();
    match output::play_from(&cfg, 0) {
        Ok(m) => {
            println!("{m}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// Video title from yt-dlp (best effort, ~2s); None on failure.
fn video_title(query: &str) -> Option<String> {
    let out = std::process::Command::new("yt-dlp")
        .args(["--no-playlist", "--get-title", "--socket-timeout", "10", query])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = SirenConfig::load();
    queue::ensure_loaded();
    let code = match cli.cmd {
        // back-compat bare query → play
        None if !cli.query.is_empty() => cmd_play(&cfg, &cli.query),
        None => match tui::run() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("tui failed: {e:#}");
                1
            }
        },
        Some(Cmd::Play { query }) => cmd_play(&cfg, &query),
        Some(Cmd::Pause) => cmd_pause(&cfg),
        Some(Cmd::Stop) => cmd_stop(&cfg),
        Some(Cmd::Seek { arg }) => cmd_seek(&cfg, &arg),
        Some(Cmd::Next) => {
            if use_heos(&cfg) {
                match heos_target(&cfg, None) {
                    Some(t) => {
                        heos::play_next(&t.0, t.1);
                        println!("Next: {}", t.2);
                        0
                    }
                    None => {
                        eprintln!("no speaker found");
                        1
                    }
                }
            } else {
                queue::cmd_next();
                std::thread::sleep(std::time::Duration::from_millis(150));
                println!("Next: {}", queue::now_label());
                0
            }
        }
        Some(Cmd::Prev) => {
            if use_heos(&cfg) {
                match heos_target(&cfg, None) {
                    Some(t) => {
                        heos::play_previous(&t.0, t.1);
                        println!("Previous: {}", t.2);
                        0
                    }
                    None => {
                        eprintln!("no speaker found");
                        1
                    }
                }
            } else {
                queue::cmd_prev();
                std::thread::sleep(std::time::Duration::from_millis(150));
                println!("Previous: {}", queue::now_label());
                0
            }
        }
        Some(Cmd::Now { cached, refresh, json }) => {
            if refresh {
                cmd_now_refresh(&cfg, json)
            } else if cached {
                cmd_now_cached(json)
            } else {
                cmd_now_live(&cfg, json)
            }
        }
        Some(Cmd::Status) => cmd_status(&cfg),
        Some(Cmd::Random) => {
            let lib = library::scan_library(&cfg);
            if lib.is_empty() {
                println!("No tracks found in library.");
                1
            } else if use_heos(&cfg) {
                // speaker: cast one shuffled pick (whole library would be a crawl)
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut h = DefaultHasher::new();
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
                    .hash(&mut h);
                let pick = &lib[(h.finish() as usize) % lib.len()];
                match heos_target(&cfg, None) {
                    Some(t) => match heos::dlna_cast(&t.0, t.1, pick, 1) {
                        Ok(()) => {
                            heos::note_cast(pick);
                            println!("cast {} → {}", pick.display(), t.2);
                            0
                        }
                        Err(e) => {
                            eprintln!("cast failed: {e}");
                            1
                        }
                    },
                    None => {
                        eprintln!("no speaker found");
                        1
                    }
                }
            } else {
                let paths: Vec<String> =
                    lib.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                if queue::start_playlist(&cfg, &paths, true) {
                    0
                } else {
                    1
                }
            }
        }
        Some(Cmd::Config { action, key, value }) => cmd_config(action, key, value),
        Some(Cmd::Audio { sub }) => cmd_audio(sub),
        Some(Cmd::Cast { query, speaker, next, append }) => {
            cmd_cast(&cfg, &query, speaker.as_deref(), next, append)
        }
        Some(Cmd::Trove { args }) => cmd_trove(&args),
        Some(Cmd::Radio { args }) => cmd_radio(&args),
        Some(Cmd::Video { url }) => cmd_video(&url),
        Some(Cmd::Sleep { args }) => cmd_sleep(&args),
        Some(Cmd::Queue { args }) => cmd_queue(&args),
        Some(Cmd::Playlist { args }) => cmd_playlist(&args),
        Some(Cmd::Spectrum { path }) => {
            use std::path::PathBuf;
            let p = PathBuf::from(shellexpand(&path));
            let t0 = std::time::Instant::now();
            crate::spectrum::request_analyze(&p);
            // wait for the background worker (bounded)
            let mut spec = None;
            for _ in 0..120 {
                if let Some(s) = crate::spectrum::spectrum_for(&p) {
                    spec = Some(s);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            match spec {
                Some(s) => {
                    println!(
                        "frames: {}  frame_dur: {:.3}s  total: {:.1}s  (took {:.1}s)",
                        s.frames.len(),
                        s.frame_dur,
                        s.duration,
                        t0.elapsed().as_secs_f32()
                    );
                    for t in [5.0, 30.0, 300.0, 1800.0] {
                        if t < s.duration {
                            let row: String =
                                s.at(t).iter().map(|v| crate::spectrum::bar_glyph(*v)).collect();
                            println!("  t={t:>6.0}s {row}");
                        }
                    }
                    0
                }
                None => {
                    eprintln!("analyze failed");
                    1
                }
            }
        }
        Some(Cmd::Resolve { query }) => {
            let hits = library::resolve_play_args(
                &cfg,
                &query.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            );
            if hits.is_empty() {
                println!("No tracks match: {}", query.join(" "));
                1
            } else {
                for (i, p) in hits.iter().take(20).enumerate() {
                    println!("{:3}. {}", i + 1, p.display());
                }
                if hits.len() > 20 {
                    println!("… {} more", hits.len() - 20);
                }
                0
            }
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod deadline_tests {
    use super::with_deadline;

    /// A fast closure returns its value.
    #[test]
    fn deadline_returns_value_when_fast() {
        let v = with_deadline(2, || 42);
        assert_eq!(v, Some(42));
    }

    /// A slow closure times out and returns None — never hangs.
    #[test]
    fn deadline_times_out_and_does_not_hang() {
        let start = std::time::Instant::now();
        let v = with_deadline(1, || {
            std::thread::sleep(std::time::Duration::from_secs(30));
            1
        });
        assert!(v.is_none(), "must return None on timeout, got {v:?}");
        assert!(start.elapsed().as_secs() < 3, "must return promptly, took {:?}", start.elapsed());
    }

    /// The worker still completes later; we just stop waiting.
    #[test]
    fn deadline_does_not_block_the_caller() {
        let start = std::time::Instant::now();
        let _ = with_deadline(1, || {
            std::thread::sleep(std::time::Duration::from_secs(2));
        });
        assert!(start.elapsed().as_secs() < 2, "caller must not wait for the worker");
    }

    #[test]
    fn follow_on_fail_closed_when_not_on_lan() {
        let src = std::fs::read_to_string("src/main.rs").expect("main.rs");
        let start = src.find("fn cmd_audio_follow").expect("cmd_audio_follow");
        let end = src[start..]
            .find("\nfn cmd_audio(")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains("heos::run_probe()"),
            "follow on must probe before feeding Cave"
        );
        assert!(
            body.contains("ProbeVerdict::NotOnLan"),
            "not-on-lan must fail closed"
        );
        assert!(
            body.contains("leaving"),
            "not-on-lan message names the sink we leave alone"
        );
        assert!(
            !body.contains("follow_on") || body.find("NotOnLan").unwrap() < body.rfind("follow_on").unwrap(),
            "NotOnLan check must run before follow_on"
        );
        assert!(body.contains("wait_while_encoder"), "follow run waits on encoder");
        assert!(body.contains("\"run\""), "systemd ExecStart is follow run");
    }

    #[test]
    fn cmd_audio_prints_follow_health() {
        let src = std::fs::read_to_string("src/main.rs").expect("main.rs");
        let start = src.find("fn cmd_audio(").expect("cmd_audio");
        let end = src[start..]
            .find("\nfn cmd_speaker_queue")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(body.contains("health_line"), "siren audio names encoder/ESTAB/UPnP");
        assert!(src.contains("mark_follow_want_stop"), "stop on wav does not reclaim");
        let st = src.find("fn cmd_status").expect("cmd_status");
        let st_end = src[st..]
            .find("\nfn print_heos_status")
            .or_else(|| src[st..].find("\nfn cmd_"))
            .map(|i| st + i)
            .unwrap_or(src.len());
        // cmd_status sits before print_heos_status in older trees; live
        // tree has print_heos_status first. Slice the function by next fn.
        let st_body = &src[st..st_end.min(st + 800)];
        assert!(
            st_body.contains("following()"),
            "siren status uses Cave UPnP while follow is on"
        );
    }

    #[test]
    fn stale_heos_snap_names_the_speaker() {
        let p = crate::heos::HeosPlayer {
            name: "Vanguarda Office".into(),
            ip: "192.168.8.184".into(),
            ..Default::default()
        };
        let v = super::heos_snap(&p, true);
        assert_eq!(v["name"], "Vanguarda Office");
        assert_eq!(v["ip"], "192.168.8.184");
        assert_eq!(v["stale"], true);
        assert!(
            v["label"].as_str().unwrap().contains("Vanguarda Office"),
            "label={}",
            v["label"]
        );
    }
}
