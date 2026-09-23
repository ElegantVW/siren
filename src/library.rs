//! Library scan + fuzzy resolve — mirrors Python `scan_library`,
//! `resolve_library`, `fuzzy_score`, `resolve_play_args`.
//!
//! Simplification (documented): the Python scorer also matches against
//! `meta_display` (cached audio tags). Rust v1 matches against the path
//! plus the file stem; tag matching lands with the metadata-cache slice.

use crate::config::SirenConfig;
use std::path::{Path, PathBuf};

pub const AUDIO_EXT: &[&str] = &[
    ".mp3", ".flac", ".ogg", ".m4a", ".wav", ".opus", ".mp4", ".mkv", ".mka",
    ".aac",
];

fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        PathBuf::from(home).join(rest)
    } else {
        PathBuf::from(p)
    }
}

pub fn library_roots(cfg: &SirenConfig) -> Vec<PathBuf> {
    cfg.library_roots.iter().map(|r| expand_home(r)).collect()
}

fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    // deterministic order like Python's os.walk + final sort
    let mut names: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|x| x.path())).collect();
    names.sort();
    for p in names {
        if p.is_dir() {
            visit(&p, out);
        } else if p.is_file() {
            let lower = p.to_string_lossy().to_lowercase();
            if AUDIO_EXT.iter().any(|e| lower.ends_with(e)) {
                out.push(p);
            }
        }
    }
}

/// Walk every root, de-dupe, sort case-insensitively (Python parity).
pub fn scan_library(cfg: &SirenConfig) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for root in library_roots(cfg) {
        let mut local = Vec::new();
        if root.is_file() {
            local.push(root);
        } else {
            visit(&root, &mut local);
        }
        for p in local {
            let key = p.to_string_lossy().into_owned();
            if seen.insert(key) {
                files.push(p);
            }
        }
    }
    files.sort_by_key(|p| p.to_string_lossy().to_lowercase());
    files
}

fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// (tier, bonus); (0,0) = no match. Tiers mirror Python exactly:
/// 10 exact · 9 prefix · 8 substring · 7 all tokens · 6 ordered subsequence.
pub fn fuzzy_score(query: &str, text: &str) -> (i32, i32) {
    let q = query.trim().to_lowercase();
    let t = text.to_lowercase();
    if q.is_empty() {
        return (0, 0);
    }
    if q == t {
        return (10, q.len() as i32);
    }
    if t.starts_with(&q) {
        return (9, q.len() as i32);
    }
    if t.contains(&q) {
        return (8, q.len() as i32);
    }
    let qt = tokens(&q);
    let tt = tokens(&t);
    if qt.is_empty() {
        return (0, 0);
    }
    if qt.iter().all(|tok| tt.contains(tok)) {
        return (7, qt.iter().map(|x| x.len() as i32).sum());
    }
    let joined = tt.concat();
    let mut pos = 0;
    let mut matched = 0;
    for tok in &qt {
        match joined[pos..].find(tok.as_str()) {
            Some(idx) => {
                pos += idx + tok.len();
                matched += tok.len() as i32;
            }
            None => break,
        }
    }
    if matched > 0 {
        return (6, matched);
    }
    (0, 0)
}

fn stem_of(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Fuzzy-ranked matches for a query; empty query returns the whole library.
pub fn resolve_library(cfg: &SirenConfig, query: &str) -> Vec<PathBuf> {
    let lib = scan_library(cfg);
    if query.trim().is_empty() {
        return lib;
    }
    let mut scored: Vec<(i32, i32, PathBuf)> = Vec::new();
    for p in lib {
        let path_s = p.to_string_lossy().into_owned();
        let (mut best_t, mut best_b) = fuzzy_score(query, &path_s);
        let (t2, b2) = fuzzy_score(query, &stem_of(&p));
        if t2 > best_t || (t2 == best_t && b2 > best_b) {
            best_t = t2;
            best_b = b2;
        }
        if best_t > 0 {
            scored.push((best_t, best_b, p));
        }
    }
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(
            a.2.to_string_lossy()
                .to_lowercase()
                .cmp(&b.2.to_string_lossy().to_lowercase()),
        )
    });
    scored.into_iter().map(|(_, _, p)| p).collect()
}

/// Single existing audio file wins; otherwise fuzzy library search.
pub fn resolve_play_args(cfg: &SirenConfig, targets: &[String]) -> Vec<PathBuf> {
    if targets.is_empty() {
        return scan_library(cfg);
    }
    if targets.len() == 1 {
        let p = expand_home(&targets[0]);
        if p.is_file() {
            let lower = p.to_string_lossy().to_lowercase();
            if AUDIO_EXT.iter().any(|e| lower.ends_with(e)) {
                return vec![p];
            }
        }
    }
    resolve_library(cfg, &targets.join(" "))
}
