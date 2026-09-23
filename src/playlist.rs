//! Saved playlists — mirrors Python `playlist_*` (`faeOS/bin/siren`).
//! Dir: `~/.config/siren/playlists/*.json` (same shape, including m3u
//! cleanup on delete).

use crate::queue::QueueItem;
use serde_json::json;
use std::path::PathBuf;

fn dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let cfg = std::env::var("SIREN_CONFIG_DIR")
        .unwrap_or_else(|_| format!("{home}/.config/siren"));
    PathBuf::from(cfg).join("playlists")
}

fn file(name: &str) -> PathBuf {
    dir().join(format!("{name}.json"))
}

pub fn save(name: &str, items: &[QueueItem]) -> bool {
    let now = chrono_utc();
    let tracks: Vec<serde_json::Value> = items
        .iter()
        .map(|t| {
            json!({
                "path": t.path, "display": t.display, "title": t.title,
                "artist": t.artist, "duration": t.duration,
            })
        })
        .collect();
    let data = json!({
        "name": name, "created": now, "modified": now, "tracks": tracks,
    });
    if std::fs::create_dir_all(dir()).is_err() {
        return false;
    }
    std::fs::write(
        file(name),
        serde_json::to_string_pretty(&data).unwrap_or_default(),
    )
    .is_ok()
}

fn chrono_utc() -> String {
    // std-only UTC stamp (no chrono dep): seconds since epoch is enough
    // for created/modified bookkeeping.
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => format!("{}", d.as_secs()),
        Err(_) => "0".into(),
    }
}

pub fn load(name: &str) -> Vec<QueueItem> {
    let p = file(name);
    let raw = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut items = Vec::new();
    if let Some(tracks) = v.get("tracks").and_then(|t| t.as_array()) {
        for t in tracks {
            let path = t.get("path").and_then(|x| x.as_str()).unwrap_or("");
            if path.is_empty() || !std::path::Path::new(path).exists() {
                continue;
            }
            items.push(QueueItem {
                path: path.to_string(),
                display: t
                    .get("display")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                title: t
                    .get("title")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                artist: t
                    .get("artist")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                duration: t.get("duration").and_then(|x| x.as_f64()).unwrap_or(0.0),
            });
        }
    }
    items
}

pub fn names() -> Vec<String> {
    let d = dir();
    let entries = match std::fs::read_dir(&d) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .collect();
    out.sort();
    out
}

pub fn delete(name: &str) -> bool {
    let mut gone = false;
    for ext in ["json", "m3u"] {
        let p = dir().join(format!("{name}.{ext}"));
        if p.exists() && std::fs::remove_file(&p).is_ok() {
            gone = true;
        }
    }
    gone
}

/// Exact match, else unique prefix match (mirrors `playlist_find`).
pub fn find(name: &str) -> Option<String> {
    let all = names();
    let low = name.to_lowercase();
    if let Some(n) = all.iter().find(|n| n.to_lowercase() == low) {
        return Some(n.clone());
    }
    let matches: Vec<&String> = all.iter().filter(|n| n.to_lowercase().starts_with(&low)).collect();
    if matches.len() == 1 {
        return Some(matches[0].clone());
    }
    None
}
