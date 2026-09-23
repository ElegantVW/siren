//! siren — the Aether's music vessel (faeOS media player).
//!
//! Port slices landed: config, library/fuzzy, mpv IPC, queue/playlist,
//! transport CLI. Still Python-only: TUI, audio menu, trove (out of v1).

mod config;
mod heos;
mod library;
mod player;
mod playlist;
mod queue;
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
    Now,
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
    },
    /// Free & legal music (Internet Archive)
    Trove {
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
        return cmd_cast(cfg, rest, None);
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
    if queue::play_queue_from(0, false) {
        println!("Playing playlist: {name}");
        0
    } else {
        println!("Playlist not found: {name}");
        1
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
        "play" | "p" => queue::cli_play(),
        "next" | "n" => queue::cli_queue_next(),
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
                    match heos::dlna_cast(&tgt.0, tgt.1, &p) {
                        Ok(()) => {
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

fn cmd_cast(cfg: &SirenConfig, query: &[String], speaker: Option<&str>) -> i32 {
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
    match heos::dlna_cast(&tgt.0, tgt.1, top) {
        Ok(()) => {
            println!("cast {} → {}", top.display(), tgt.2);
            0
        }
        Err(e) => {
            eprintln!("cast failed: {e}");
            1
        }
    }
}

fn cmd_trove(args: &[String]) -> i32 {
    if args.is_empty() {
        return trove::run_trove(10, &[], None);
    }
    let sub = args[0].to_lowercase();
    if sub == "get" || sub == "g" {
        if args.len() < 2 {
            eprintln!("usage: siren trove get <identifier>");
            return 2;
        }
        return trove::run_get(&args[1]);
    }
    if sub == "about" || sub == "help" || sub == "-h" || sub == "--help" {
        for line in trove::about_text() {
            println!("  {line}");
        }
        return 0;
    }
    // optional leading count, then kind detection inside run_trove
    let mut words: Vec<String> = args.to_vec();
    let mut n = 10u32;
    if let Some(first) = words.first() {
        if let Ok(v) = first.parse::<u32>() {
            n = v;
            words.remove(0);
        }
    }
    trove::run_trove(n, &words, None)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = SirenConfig::load();
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
        Some(Cmd::Now) => {
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
        Some(Cmd::Cast { query, speaker }) => cmd_cast(&cfg, &query, speaker.as_deref()),
        Some(Cmd::Trove { args }) => cmd_trove(&args),
        Some(Cmd::Queue { args }) => cmd_queue(&args),
        Some(Cmd::Playlist { args }) => cmd_playlist(&args),
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
