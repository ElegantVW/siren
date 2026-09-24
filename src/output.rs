//! Output — the ONE playback channel.
//!
//! Sources (library queue, playlists, trove, radio) never touch mpv or
//! HEOS directly. They stage `QueueItem`s and call `play_from`; output
//! routes by `audio_output` + item kind:
//! - local files → mpv playlist mirror (files and stream URLs alike)
//! - speaker files → DLNA cast in order (play-now + appends)
//! - speaker streams → TuneIn search by station name + `browse/play_stream`
//!   (raw-URL `play_stream` won't hold on this HEOS 1 unit — verified)
//!
//! Stream items are queue items whose path is an `http(s)` URL with the
//! station name in `display`.

use crate::config::SirenConfig;
use crate::{heos, queue};

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
    let mut pool: Vec<crate::radio::Station> = hits
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
        // Speaker radio rides TuneIn: map the station name to a TuneIn
        // mid and play it. Raw-URL play_stream won't hold on this unit.
        // Rank gate (≤3) keeps URL-named favs from matching random junk.
        let title = if slice[0].display.is_empty() {
            slice[0].path.clone()
        } else {
            slice[0].display.clone()
        };
        let Some(hit) = tunein_find(&ip, &title) else {
            return Err(format!("no TuneIn match: {title} (try output local)"));
        };
        if heos::tunein_play(&ip, pid, &hit) {
            heos::note_radio(&slice[0].path, &hit.name);
            Ok(format!("▶ {} on {name}", hit.name))
        } else {
            Err(format!("TuneIn play failed: {}", hit.name))
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
