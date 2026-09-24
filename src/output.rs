//! Output — the ONE playback channel.
//!
//! Sources (library queue, playlists, trove, radio) never touch mpv or
//! HEOS directly. They stage `QueueItem`s and call `play_from`; output
//! routes by `audio_output` + item kind:
//! - local files → mpv playlist mirror (files and stream URLs alike)
//! - speaker files → DLNA cast in order (play-now + appends)
//! - speaker streams (radio) → `play_stream` (one at a time)
//!
//! Stream items are queue items whose path is an `http(s)` URL with the
//! station name in `display`.

use crate::config::SirenConfig;
use crate::{heos, queue};

fn speaker_target(cfg: &SirenConfig) -> Option<(String, i64, String)> {
    let h = if cfg.audio_speaker.trim().is_empty() {
        None
    } else {
        Some(cfg.audio_speaker.clone())
    };
    heos::resolve(h.as_deref()).map(|(ip, pid, p)| (ip, pid, p.name))
}

/// Play the staged queue from `index` through the configured output.
/// `Ok`/`Err` carry the human message (faeOS cli-voice, no superlatives).
pub fn play_from(cfg: &SirenConfig, index: usize) -> Result<String, String> {
    let items = queue::snapshot();
    if items.is_empty() || index >= items.len() {
        return Err("Queue empty.".into());
    }
    if cfg.audio_output != "heos" {
        return if queue::play_queue_from(index, false) {
            Ok("playing from queue".into())
        } else {
            Err("Queue empty.".into())
        };
    }
    let (ip, pid, name) = speaker_target(cfg).ok_or_else(|| "no speaker found".to_string())?;
    let slice = &items[index..];
    if crate::meta::is_url(&slice[0].path) {
        let title = if slice[0].display.is_empty() {
            slice[0].path.clone()
        } else {
            slice[0].display.clone()
        };
        if heos::play_stream(&ip, pid, &slice[0].path) {
            heos::note_stream(&slice[0].path, &title);
            Ok(format!("▶ {title} on {name}"))
        } else {
            Err(format!("stream failed: {title}"))
        }
    } else {
        // files cast in order; stray streams after files can't ride DLNA
        let files: Vec<std::path::PathBuf> = slice
            .iter()
            .filter(|t| !crate::meta::is_url(&t.path))
            .map(|t| std::path::PathBuf::from(&t.path))
            .collect();
        let skipped = slice.len() - files.len();
        if files.is_empty() {
            return Err("Queue empty.".into());
        }
        let (ok, n) = heos::dlna_cast_many(&ip, pid, &files);
        if ok > 0 {
            heos::note_cast(&files[0]);
        }
        let mut msg = format!("playing queue ({ok}/{n} on {name})");
        if skipped > 0 {
            msg.push_str(&format!(" · {skipped} stream(s) skipped (files only)"));
        }
        if ok == 0 {
            return Err(msg);
        }
        Ok(msg)
    }
}
