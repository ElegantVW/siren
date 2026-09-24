//! siren — the Aether's music vessel (faeOS media player).
//!
//! Port slices landed: config, library/fuzzy, mpv IPC, queue/playlist,
//! transport CLI, two-box TUI, audio/HEOS, trove, tags, dir browser.

mod config;
mod heos;
mod library;
mod meta;
mod player;
mod playlist;
mod queue;
mod spectrum;
mod trove;
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
    /// Free & legal music (Internet Archive)
    Trove {
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
    /// Speaker queue: list | play <n> | rm <n> | clear | move <from> <to>
    /// (local queue stays `siren queue …`; this is the HEOS box)
    Queue {
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
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
    if use_heos(&cfg) {
        let paths: Vec<std::path::PathBuf> =
            queue::snapshot().iter().map(|t| std::path::PathBuf::from(&t.path)).collect();
        return play_paths_heos(&cfg, &paths, Some(name));
    }
    if queue::play_queue_from(0, false) {
        println!("Playing playlist: {name}");
        0
    } else {
        println!("Playlist not found: {name}");
        1
    }
}

/// Play local paths on the speaker in order (first play-now, rest append).
fn play_paths_heos(cfg: &SirenConfig, paths: &[std::path::PathBuf], what: Option<&str>) -> i32 {
    if paths.is_empty() {
        println!("Queue empty.");
        return 1;
    }
    let tgt = match heos_target(cfg, None) {
        Some(t) => t,
        None => {
            eprintln!("no speaker found");
            return 1;
        }
    };
    println!("casting {} track(s) → {}…", paths.len(), tgt.2);
    let (ok, n) = heos::dlna_cast_many(&tgt.0, tgt.1, paths);
    if ok > 0 {
        heos::note_cast(&paths[0]);
    }
    match what {
        Some(name) => println!("Playing playlist: {name} ({ok}/{n} on {})", tgt.2),
        None => println!("Playing queue ({ok}/{n} on {})", tgt.2),
    }
    if ok == 0 { 1 } else { 0 }
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
        "play" | "p" => {
            if use_heos(&cfg) {
                let paths: Vec<std::path::PathBuf> = queue::snapshot()
                    .iter()
                    .map(|t| std::path::PathBuf::from(&t.path))
                    .collect();
                play_paths_heos(&cfg, &paths, None)
            } else {
                queue::cli_play()
            }
        }
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
            let siren = format!(
                "{}/bin/siren",
                std::env::var("HOME").unwrap_or_else(|_| "/root".into())
            );
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
    if let Some(rest) = p.strip_prefix("~/") {
        format!("{}/{rest}", std::env::var("HOME").unwrap_or_else(|_| "/root".into()))
    } else {
        p.to_string()
    }
}

fn now_cache_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    std::path::PathBuf::from(format!("{home}/.cache/siren/now.json"))
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Gather current playback (output-aware) for the cache / JSON.
fn now_snapshot(cfg: &SirenConfig) -> serde_json::Value {
    if cfg.audio_output == "heos" {
        if let Some(t) = heos_target(cfg, None) {
            let mut ps = vec![heos::HeosPlayer {
                name: t.2.clone(),
                pid: t.1,
                model: String::new(),
                ip: t.0.clone(),
                network: String::new(),
                state: None,
                volume: None,
            }];
            heos::enrich(&mut ps);
            let p = &ps[0];
            let label = format!(
                "{} — {}",
                p.name,
                p.state.as_deref().unwrap_or("?")
            );
            return serde_json::json!({
                "label": label,
                "output": "heos",
                "state": p.state,
                "vol": p.volume,
                "ts": now_epoch(),
            });
        }
        return serde_json::json!({
            "label": "", "output": "heos",
            "state": null, "vol": null, "ts": now_epoch(),
        });
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
fn cmd_now_refresh(cfg: &SirenConfig, json: bool) -> i32 {
    // keep the roster cache warm too (best effort, never fails the refresh)
    heos::refresh_roster();
    let snap = now_snapshot(cfg);
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
    0
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

fn cmd_audio(sub: Option<AudioCmd>) -> i32 {
    let mut cfg = SirenConfig::load();
    match sub {
        None | Some(AudioCmd::Status) => {
            println!("output:  {}", cfg.audio_output);
            println!("speaker: {}", cfg.audio_speaker);
            match heos_target(&cfg, None) {
                Some((ip, pid, name)) => {
                    let mut ps = vec![heos::HeosPlayer {
                        name: name.clone(),
                        pid,
                        model: String::new(),
                        ip: ip.clone(),
                        network: String::new(),
                        state: None,
                        volume: None,
                    }];
                    heos::enrich(&mut ps);
                    let p = &ps[0];
                    println!(
                        "live:    {} ({}) state={} vol={}",
                        p.name,
                        p.ip,
                        p.state.as_deref().unwrap_or("?"),
                        p.volume.map(|v| format!("{v}%")).unwrap_or("?".into())
                    );
                }
                None => println!("live:    no speaker found"),
            }
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
                        model: String::new(),
                        ip: tgt.0.clone(),
                        network: String::new(),
                        state: None,
                        volume: None,
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
                    let lvl: i32 = match v.trim().trim_end_matches('%').parse() {
                        Ok(n) => n,
                        Err(_) => {
                            eprintln!("volume must be 0-100");
                            return 1;
                        }
                    };
                    if heos::set_volume(&tgt.0, tgt.1, lvl) {
                        println!("{} volume → {}%", tgt.2, lvl.clamp(0, 100));
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
        return trove::run_trove(10, &[], None, format);
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
    let mut n = 10u32;
    if let Some(first) = words.first() {
        if let Ok(v) = first.parse::<u32>() {
            n = v;
            words.remove(0);
        }
    }
    trove::run_trove(n, &words, None, format)
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
        Some(Cmd::Pause) => {
            if use_heos(&cfg) {
                match heos_target(&cfg, None) {
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
        Some(Cmd::Stop) => {
            if use_heos(&cfg) {
                match heos_target(&cfg, None) {
                    Some(t) => {
                        heos::set_state(&t.0, t.1, "stop");
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
            if use_heos(&cfg) {
                match heos_target(&cfg, None) {
                    Some(t) => {
                        let mut ps = vec![heos::HeosPlayer {
                            name: t.2.clone(),
                            pid: t.1,
                            model: String::new(),
                            ip: t.0.clone(),
                            network: String::new(),
                            state: None,
                            volume: None,
                        }];
                        heos::enrich(&mut ps);
                        let p = &ps[0];
                        println!(
                            "{} — {}, {}",
                            p.name,
                            p.state.as_deref().unwrap_or("?"),
                            p.volume.map(|v| format!("{v}%")).unwrap_or("?".into())
                        );
                        0
                    }
                    None => {
                        eprintln!("no speaker found");
                        1
                    }
                }
            } else {
                let label = queue::now_label();
                if label.is_empty() {
                    println!("Not playing");
                } else {
                    println!("{label}");
                }
                0
            }
            }
        }
        Some(Cmd::Status) => {
            if use_heos(&cfg) {
                match heos_target(&cfg, None) {
                    Some(t) => {
                        let mut ps = vec![heos::HeosPlayer {
                            name: t.2.clone(),
                            pid: t.1,
                            model: String::new(),
                            ip: t.0.clone(),
                            network: String::new(),
                            state: None,
                            volume: None,
                        }];
                        heos::enrich(&mut ps);
                        let p = &ps[0];
                        println!("status: {}", p.state.as_deref().unwrap_or("?"));
                        println!("speaker: {} ({})", p.name, p.ip);
                        println!(
                            "vol:    {}",
                            p.volume.map(|v| format!("{v}%")).unwrap_or("?".into())
                        );
                        0
                    }
                    None => {
                        eprintln!("no speaker found");
                        1
                    }
                }
            } else if !player::alive() {
                let lib = library::scan_library(&cfg);
                println!("status: idle (no mpv)");
                println!("library tracks: {}", lib.len());
                0
            } else {
                for line in queue::status_lines(&cfg) {
                    println!("{line}");
                }
                0
            }
        }
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
