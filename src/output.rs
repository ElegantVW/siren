//! Output — the ONE playback channel.
//!
//! File queue / playlists call `play_from`. Radio calls `play_station`
//! (one live URL) and does not write `queue.json`.
//! Routes by `audio_output`:
//! - local files → mpv playlist mirror
//! - local radio → mpv `loadfile` URL
//! - speaker files → DLNA cast
//! - speaker streams → TuneIn / `play_url`

use crate::config::SirenConfig;
use crate::{heos, player, queue};

/// Map a station title to a TuneIn hit: full title, then keyword
/// fallbacks ("OXIGÉNIO 102.6 FM" → "OXIGÉNIO 102" → "OXIGÉNIO").
/// Only genuinely related hits (rank ≤ 3) — never random junk.
fn tunein_find(ip: &str, title: &str) -> Option<heos::TuneinHit> {
    let mut queries = vec![title.trim().to_string()];
    let keys: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .take(3)
        .map(|s| s.to_string())
        .collect();
    let joined = keys.join(" ");
    // full title, keyword set, then each keyword alone ("Comercial"
    // finds "Radio Comercial" when the phrase search comes up empty)
    for q in [joined]
        .into_iter()
        .chain(keys.clone())
        .filter(|q| !q.is_empty() && q.to_lowercase() != title.trim().to_lowercase())
    {
        if !queries.iter().any(|x| x.to_lowercase() == q.to_lowercase()) {
            queries.push(q);
        }
    }
    // TuneIn search is accent-sensitive ("OXIGÉNIO" misses what "OXIGENIO"
    // finds), so also try every query ASCII-folded
    let mut folded = Vec::new();
    for q in &queries {
        let f = crate::radio::norm(q);
        if !f.is_empty() && !queries.iter().any(|x| x.to_lowercase() == f) {
            folded.push(f);
        }
    }
    queries.extend(folded);
    // one global pool across all fallback queries, ranked ONCE against
    // the real title — an exact "Radio Comercial" always beats an anagram
    // like "COMERCIAL RADIO" found under a fallback query
    let mut hits: Vec<heos::TuneinHit> = Vec::new();
    for q in &queries {
        for h in heos::tunein_search(ip, q) {
            if !hits.iter().any(|x| x.mid == h.mid) {
                hits.push(h);
            }
        }
    }
    let pool: Vec<crate::radio::Station> = hits
        .iter()
        .map(|h| crate::radio::Station {
            uuid: String::new(),
            name: h.name.clone(),
            url: String::new(),
            codec: String::new(),
            bitrate: 0,
            tags: String::new(),
            country: String::new(),
        })
        .collect();
    // rank against the title minus frequency noise: "OXIGÉNIO 102.6 FM"
    // ranks as "OXIGÉNIO", so "Rádio Oxigénio" wins by substring instead
    // of drowning among short junk. URL-named favs ("0r-lo-fi?ref=…")
    // yield no significant tokens and honestly miss.
    let sig: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .map(|t| crate::radio::norm(t))
        .filter(|t| {
            t.len() > 2
                && !t.chars().all(|c| c.is_numeric() || c == '.')
                && !["fm", "am", "mhz", "khz", "dab"].contains(&t.as_str())
        })
        .collect();
    if sig.is_empty() {
        return None;
    }
    let rank_q = sig.join(" ");
    let Some(best) = crate::radio::best_match(&rank_q, &pool) else {
        return None;
    };
    let ok = crate::radio::match_rank(title, &best.name, "") <= 3
        || crate::radio::match_rank(&best.name, title, "") <= 3
        || crate::radio::match_rank(&rank_q, &best.name, "") <= 2;
    if ok {
        return hits.into_iter().find(|h| h.name == best.name);
    }
    None
}

fn speaker_target(cfg: &SirenConfig) -> Option<(String, i64, String)> {
    let h = if cfg.audio_speaker.trim().is_empty() {
        None
    } else {
        Some(cfg.audio_speaker.clone())
    };
    heos::resolve(h.as_deref()).map(|(ip, pid, p)| (ip, pid, p.name))
}

/// Append a timestamped line to the play log. This is the audit
/// trail: when the UI and the speaker disagree, this file says what
/// siren actually decided and why.
fn play_log(msg: &str) {
    use std::io::Write;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = crate::paths::cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("play.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "[{ts}] {msg}");
    }
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
            Err("mpv not ready".into())
        };
    }
    let slice = &items[index..];
    if crate::meta::is_url(&slice[0].path) {
        let url = slice[0].path.clone();
        let title = if slice[0].display.is_empty() {
            url.clone()
        } else {
            slice[0].display.clone()
        };
        play_station(cfg, &url, &title)
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
        let (ip, pid, name) = speaker_target(cfg).ok_or_else(|| "no speaker found".to_string())?;
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

/// One live station. Does not write the file queue.
pub fn play_station(cfg: &SirenConfig, url: &str, title: &str) -> Result<String, String> {
    if cfg.audio_output != "heos" {
        if !player::spawn(cfg) {
            return Err("mpv not ready".into());
        }
        if !player::send(&serde_json::json!(["loadfile", url, "replace"])) {
            return Err("mpv not ready".into());
        }
        player::send(&serde_json::json!(["set_property", "pause", false]));
        player::send(&serde_json::json!(["set_property", "video", "no"]));
        queue::note_playing(title, false);
        return Ok(format!("▶ {title}"));
    }
    let (ip, pid, name) = speaker_target(cfg).ok_or_else(|| "no speaker found".to_string())?;
    if heos::url_alive(url) {
        play_log(&format!("direct: {url} -> {ip} pid={pid}"));
        if heos::play_url(&ip, pid, url) && heos::confirm_play_url(&ip, url, 4) {
            heos::note_radio(url, title);
            play_log(&format!("direct OK: {title} confirmed play"));
            return Ok(format!("▶ {title} on {name}"));
        }
        play_log(&format!("direct FAILED: {title} (ack or confirm failed)"));
    } else {
        play_log(&format!("direct SKIP (dead URL): {url}"));
    }
    let Some(hit) = tunein_find(&ip, title) else {
        return Err(format!("no TuneIn match: {title} (try output local)"));
    };
    if heos::tunein_play(&ip, pid, &hit) {
        if heos::confirm_play(&ip, pid, 4) {
            heos::note_radio(url, &hit.name);
            play_log(&format!(
                "tunein OK: {} mid={} confirmed play",
                hit.name, hit.mid
            ));
            return Ok(format!("▶ {} on {name}", hit.name));
        }
        play_log(&format!("tunein FAILED (no confirm): {}", hit.name));
        return Err(format!("{} not responding (stream may be down)", hit.name));
    }
    play_log(&format!("tunein FAILED (no ack): {}", hit.name));
    Err(format!("TuneIn play failed: {}", hit.name))
}

#[cfg(test)]
mod tests {
    #[test]
    fn radio_is_not_the_file_queue() {
        let src = std::fs::read_to_string("src/output.rs").unwrap();
        assert!(src.contains("pub fn play_station"));
        assert!(src.contains("mpv not ready"));
        let tui = std::fs::read_to_string("src/tui.rs").unwrap();
        let radio_play = tui
            .split("fn radio_play")
            .nth(1)
            .unwrap()
            .split("\nfn ")
            .next()
            .unwrap();
        assert!(
            radio_play.contains("play_station"),
            "TUI radio must not stage queue.json"
        );
        assert!(
            !radio_play.contains("queue::replace"),
            "radio_play must not replace the file queue"
        );
        let main = std::fs::read_to_string("src/main.rs").unwrap();
        assert!(
            main.contains("output::play_station"),
            "siren radio play uses play_station"
        );
    }
}
