//! siren — the Aether's music vessel (faeOS media player).
//!
//! Port slices landed: config, library/fuzzy, mpv IPC, queue/playlist,
//! transport CLI. Still Python-only: TUI, audio menu, trove (out of v1).

mod config;
mod library;
mod player;
mod playlist;
mod queue;

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

fn cmd_play(cfg: &SirenConfig, rest: &[String]) -> i32 {
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = SirenConfig::load();
    let code = match cli.cmd {
        // back-compat bare query → play
        None if !cli.query.is_empty() => cmd_play(&cfg, &cli.query),
        None => {
            println!("siren: TUI still lives in Python — try play | status | queue | playlist | config | audio.");
            0
        }
        Some(Cmd::Play { query }) => cmd_play(&cfg, &query),
        Some(Cmd::Pause) => {
            queue::cmd_pause();
            println!("Playback paused / resumed");
            0
        }
        Some(Cmd::Stop) => {
            queue::cmd_stop();
            println!("Playback stopped");
            0
        }
        Some(Cmd::Next) => {
            queue::cmd_next();
            std::thread::sleep(std::time::Duration::from_millis(150));
            println!("Next: {}", queue::now_label());
            0
        }
        Some(Cmd::Prev) => {
            queue::cmd_prev();
            std::thread::sleep(std::time::Duration::from_millis(150));
            println!("Previous: {}", queue::now_label());
            0
        }
        Some(Cmd::Now) => {
            let label = queue::now_label();
            if label.is_empty() {
                println!("Not playing");
            } else {
                println!("{label}");
            }
            0
        }
        Some(Cmd::Status) => {
            if !player::alive() {
                let lib = library::scan_library(&cfg);
                println!("status: idle (no mpv)");
                println!("library tracks: {}", lib.len());
            } else {
                for line in queue::status_lines(&cfg) {
                    println!("{line}");
                }
            }
            0
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
        Some(Cmd::Audio { sub }) => match sub {
            None | Some(AudioCmd::Status) => {
                println!("audio: output=local (mpv) — heos routing lands with the audio menu slice.");
                0
            }
        },
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
