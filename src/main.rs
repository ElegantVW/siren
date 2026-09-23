//! siren — the Aether's music vessel (faeOS media player).
//!
//! Scaffold (v0.1.0): CLI dispatch only. The Python siren
//! (`faeOS/bin/siren`) remains the live player until port slices land.

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "siren", version, about = "faeOS music vessel")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show status (mpv-local for now)
    Status,
    /// Audio configuration: output, speaker, volume
    Audio {
        #[command(subcommand)]
        sub: Option<AudioCmd>,
    },
}

#[derive(Subcommand)]
enum AudioCmd {
    /// Show current audio routing
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        None => {
            println!("siren 0.1.0 (rust scaffold) — TUI + playback still live in faeOS/bin/siren (python).");
            println!("try: siren status | siren audio status");
        }
        Some(Cmd::Status) => {
            println!("siren status: rust engine not yet wired to mpv — use faeOS/bin/siren.");
        }
        Some(Cmd::Audio { sub }) => match sub {
            None | Some(AudioCmd::Status) => {
                println!("audio: output=local (mpv) — heos routing lands with the audio menu slice.");
            }
        },
    }
    Ok(())
}
