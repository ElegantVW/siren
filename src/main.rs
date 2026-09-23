//! siren — the Aether's music vessel (faeOS media player).
//!
//! Port slices landed: config, library/fuzzy. Playback (mpv IPC), queue,
//! TUI and audio menu are still live in the Python `faeOS/bin/siren`.

mod config;
mod library;

use anyhow::Result;
use clap::{Parser, Subcommand};

use config::SirenConfig;

#[derive(Parser)]
#[command(name = "siren", version, about = "faeOS music vessel")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show library + engine status
    Status,
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
    /// Resolve a query against ~/Music (dry — mpv playback still Python)
    Resolve {
        query: Vec<String>,
    },
}

#[derive(Subcommand)]
enum AudioCmd {
    /// Show current audio routing
    Status,
}

fn cmd_config(args: (Option<String>, Option<String>, Option<String>)) -> i32 {
    let (action, key, value) = args;
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let code = match cli.cmd {
        None => {
            println!("siren: try play | status | config | audio (playback still Python until mpv slice).");
            0
        }
        Some(Cmd::Status) => {
            let cfg = SirenConfig::load();
            let lib = library::scan_library(&cfg);
            println!("siren status (rust):");
            println!("  library tracks: {}", lib.len());
            println!("  roots: {}", cfg.library_roots.join(", "));
            println!("  mpv: not yet wired (python owns playback)");
            0
        }
        Some(Cmd::Config { action, key, value }) => cmd_config((action, key, value)),
        Some(Cmd::Audio { sub }) => match sub {
            None | Some(AudioCmd::Status) => {
                println!("audio: output=local (mpv) — heos routing lands with the audio menu slice.");
                0
            }
        },
        Some(Cmd::Resolve { query }) => {
            let cfg = SirenConfig::load();
            let hits =
                library::resolve_play_args(&cfg, &query.iter().map(|s| s.to_string()).collect::<Vec<_>>());
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
