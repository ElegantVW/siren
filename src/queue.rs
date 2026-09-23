//! Session queue + playback orchestration — mirrors Python `Queue`,
//! `play_queue_from`, `start_playlist`, `cmd_next/prev/stop/pause`,
//! `now_label`, `cmd_status` (`faeOS/bin/siren`).
//!
//! Simplification (documented): item `display` uses the file stem.
//! Python snapshots mutagen tags; tag display lands with the
//! metadata-cache slice.

use crate::config::SirenConfig;
use crate::player;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItem {
    pub path: String,
    #[serde(default)]
    pub display: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub duration: f64,
}

pub fn display_of(path: &str) -> String {
    let p = path.strip_prefix("file://").unwrap_or(path);
    Path::new(p)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| p.to_string())
}

fn basename(p: &str) -> &str {
    Path::new(p)
        .file_name()
        .map(|s| s.to_str().unwrap_or(p))
        .unwrap_or(p)
}

pub struct Queue {
    pub items: Vec<QueueItem>,
}

impl Queue {
    pub fn add(&mut self, path: &str, prepend: bool) -> usize {
        let item = QueueItem {
            path: path.to_string(),
            display: display_of(path),
            title: String::new(),
            artist: String::new(),
            duration: 0.0,
        };
        if prepend {
            self.items.insert(0, item);
        } else {
            self.items.push(item);
        }
        self.items.len()
    }

    /// Drop missing files; returns removed (path, reason) pairs.
    pub fn validate(&mut self) -> Vec<(QueueItem, String)> {
        let mut removed = Vec::new();
        self.items.retain(|it| {
            if Path::new(&it.path).exists() {
                true
            } else {
                removed.push((it.clone(), "missing".to_string()));
                false
            }
        });
        removed
    }

    pub fn remove(&mut self, index: usize) -> Option<QueueItem> {
        if index < self.items.len() {
            Some(self.items.remove(index))
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

static QUEUE: OnceLock<Mutex<Queue>> = OnceLock::new();
static QUEUE_DRIVEN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static LAST_LABEL: OnceLock<Mutex<String>> = OnceLock::new();
static QUEUE_LOADED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn queue_file() -> std::path::PathBuf {
    let base = std::env::var("SIREN_CONFIG_DIR").unwrap_or_else(|_| {
        format!(
            "{}/.config/siren",
            std::env::var("HOME").unwrap_or_else(|_| "/root".into())
        )
    });
    std::path::PathBuf::from(base).join("queue.json")
}

fn queue() -> std::sync::MutexGuard<'static, Queue> {
    QUEUE
        .get_or_init(|| Mutex::new(Queue { items: Vec::new() }))
        .lock()
        .unwrap()
}

/// Persist the session queue (survives processes/restarts).
fn save_queue() {
    let q = queue();
    let v: Vec<serde_json::Value> = q
        .items
        .iter()
        .map(|t| {
            serde_json::json!({
                "path": t.path, "display": t.display, "title": t.title,
                "artist": t.artist, "duration": t.duration,
            })
        })
        .collect();
    drop(q);
    let p = queue_file();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_string_pretty(&v).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, p);
    }
}

/// Load persisted queue once per process (drops missing files).
pub fn ensure_loaded() {
    if QUEUE_LOADED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let raw = match std::fs::read_to_string(queue_file()) {
        Ok(s) => s,
        Err(_) => return,
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return,
    };
    let arr = match v.as_array() {
        Some(a) => a,
        None => return,
    };
    let mut items = Vec::new();
    for t in arr {
        let path = t.get("path").and_then(|x| x.as_str()).unwrap_or("");
        if path.is_empty() || !std::path::Path::new(path).exists() {
            continue;
        }
        items.push(QueueItem {
            path: path.to_string(),
            display: t.get("display").and_then(|x| x.as_str()).unwrap_or("").into(),
            title: t.get("title").and_then(|x| x.as_str()).unwrap_or("").into(),
            artist: t.get("artist").and_then(|x| x.as_str()).unwrap_or("").into(),
            duration: t.get("duration").and_then(|x| x.as_f64()).unwrap_or(0.0),
        });
    }
    if !items.is_empty() {
        queue().items = items;
    }
}

fn set_driven(v: bool) {
    QUEUE_DRIVEN.store(v, std::sync::atomic::Ordering::SeqCst);
}
fn driven() -> bool {
    QUEUE_DRIVEN.load(std::sync::atomic::Ordering::SeqCst)
}
fn set_last_label(s: &str) {
    *LAST_LABEL
        .get_or_init(|| Mutex::new(String::new()))
        .lock()
        .unwrap() = s.to_string();
}

/// Time-seeded Fisher-Yates (no rand dep; mirrors `random.shuffle` role).
fn shuffle_in_place<T>(v: &mut [T]) {
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e3779b97f4a7c15);
    if seed == 0 {
        seed = 0x9e3779b97f4a7c15;
    }
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for i in (1..v.len()).rev() {
        let j = (next() % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
}

pub fn start_playlist(cfg: &SirenConfig, files: &[String], shuffle: bool) -> bool {
    if files.is_empty() {
        println!("No tracks found to play.");
        return false;
    }
    let mut ordered: Vec<String> = files.to_vec();
    if shuffle {
        shuffle_in_place(&mut ordered);
    }
    if !player::spawn(cfg) {
        return false;
    }
    player::send(&json!(["playlist-clear"]));
    player::send(&json!(["loadfile", ordered[0], "replace"]));
    player::send(&json!(["set_property", "pause", false]));
    player::send(&json!(["set_property", "video", "no"]));
    for f in &ordered[1..] {
        player::send(&json!(["loadfile", f, "append"]));
    }
    set_driven(false);
    set_last_label(&display_of(&ordered[0]));
    println!("Playing {} track(s)", ordered.len());
    true
}

pub fn sync_mpv_playlist(paths: &[String]) -> bool {
    if paths.is_empty() {
        player::send(&json!(["stop"]));
        return true;
    }
    if !player::send(&json!(["playlist-clear"])) {
        return false;
    }
    player::send(&json!(["loadfile", paths[0], "replace"]));
    for p in &paths[1..] {
        player::send(&json!(["loadfile", p, "append"]));
    }
    player::send(&json!(["set_property", "pause", false]));
    player::send(&json!(["set_property", "video", "no"]));
    true
}

/// Mirror the queue into mpv from `index`; wrap rotates the order.
pub fn play_queue_from(index: usize, wrap: bool) -> bool {
    let q = queue();
    if q.items.is_empty() || index >= q.items.len() {
        return false;
    }
    let ordered: Vec<QueueItem> = if wrap {
        q.items[index..]
            .iter()
            .chain(q.items[..index].iter())
            .cloned()
            .collect()
    } else {
        q.items[index..].to_vec()
    };
    let label = ordered
        .first()
        .map(|it| {
            if it.display.is_empty() {
                display_of(&it.path)
            } else {
                it.display.clone()
            }
        })
        .unwrap_or_default();
    let paths: Vec<String> = ordered.iter().map(|it| it.path.clone()).collect();
    drop(q);
    if !sync_mpv_playlist(&paths) {
        return false;
    }
    set_driven(true);
    set_last_label(&label);
    true
}

pub fn maybe_resync_queue() {
    if !driven() {
        return;
    }
    let q = queue();
    if q.items.is_empty() {
        drop(q);
        set_driven(false);
        player::send(&json!(["playlist-clear"]));
        player::send(&json!(["stop"]));
        return;
    }
    let cur = player::now_path();
    let mut idx = 0;
    for (i, it) in q.items.iter().enumerate() {
        if it.path == cur {
            idx = i;
            break;
        }
    }
    let paths: Vec<String> = q.items.iter().map(|it| it.path.clone()).collect();
    drop(q);
    sync_mpv_playlist(&paths);
    if idx > 0 {
        player::send(&json!(["set_property", "playlist-pos", idx]));
    }
}

pub fn queue_add_and_play(path: &str, prepend: bool) -> bool {
    let idx = {
        let mut q = queue();
        match q.items.iter().position(|it| it.path == path) {
            Some(i) => i,
            None => {
                q.add(path, prepend);
                q.items.iter().position(|it| it.path == path).unwrap_or(0)
            }
        }
    };
    play_queue_from(idx, false)
}

pub fn cmd_next() {
    let cur = player::now_path();
    let q = queue();
    if !q.items.is_empty() && !cur.is_empty() {
        if let Some(i) = q.items.iter().position(|it| it.path == cur) {
            let n = q.items.len();
            drop(q);
            play_queue_from((i + 1) % n, true);
            return;
        }
    }
    drop(q);
    player::send(&json!(["playlist-next"]));
}

pub fn cmd_prev() {
    let cur = player::now_path();
    let q = queue();
    if !q.items.is_empty() && !cur.is_empty() {
        if let Some(i) = q.items.iter().position(|it| it.path == cur) {
            let n = q.items.len();
            drop(q);
            play_queue_from((i + n - 1) % n, true);
            return;
        }
    }
    drop(q);
    player::send(&json!(["playlist-prev"]));
}

pub fn cmd_stop() {
    set_driven(false);
    set_last_label("");
    player::send(&json!(["stop"]));
}

pub fn cmd_pause() {
    player::send(&json!(["cycle", "pause"]));
}

pub fn now_label() -> String {
    let p = player::now_path();
    if !p.is_empty() {
        return display_of(&p);
    }
    LAST_LABEL
        .get_or_init(|| Mutex::new(String::new()))
        .lock()
        .unwrap()
        .clone()
}

pub fn fmt_clock(secs: f64) -> String {
    format!("{}:{:02}", (secs as i64).max(0) / 60, (secs as i64).max(0) % 60)
}

fn get_repeat() -> &'static str {
    let yes = |v: Option<serde_json::Value>| matches!(
        v.as_ref().and_then(|x| x.as_str()),
        Some("inf") | Some("yes") | Some("always")
    ) || v.as_ref().and_then(|x| x.as_bool()).unwrap_or(false);
    if yes(player::get("loop-playlist")) {
        return "all";
    }
    if yes(player::get("loop-file")) {
        return "track";
    }
    "off"
}

pub fn status_lines(cfg: &SirenConfig) -> Vec<String> {
    let mut lines = Vec::new();
    if !player::alive() {
        lines.push("status: idle (no mpv)".into());
        return lines;
    }
    let status = if player::get_bool("pause", false) {
        "paused"
    } else {
        "playing"
    };
    lines.push(format!("status: {status}"));
    lines.push(format!("track:  {}", {
        let l = now_label();
        if l.is_empty() { "— silence —".into() } else { l }
    }));
    let (pos, dur) = (player::get_f64("time-pos"), player::get_f64("duration"));
    if dur > 0.0 {
        lines.push(format!("time:   {} / {}", fmt_clock(pos), fmt_clock(dur)));
    }
    let vol = player::get("volume")
        .and_then(|v| v.as_i64())
        .unwrap_or(cfg.default_volume as i64);
    lines.push(format!("vol:    {vol}%"));
    lines.push(format!(
        "mode:   shuffle {} · repeat {}",
        if player::get_bool("shuffle", false) {
            "on"
        } else {
            "off"
        },
        get_repeat()
    ));
    lines
}

// ---- queue CLI helpers (messages mirror Python) ----

pub fn cli_add(cfg: &SirenConfig, query: &str, prepend: bool) -> i32 {
    let files = crate::library::resolve_play_args(
        cfg,
        &query.split_whitespace().map(|s| s.to_string()).collect::<Vec<_>>(),
    );
    if files.is_empty() {
        println!("No tracks found matching query.");
        return 1;
    }
    let mut q = queue();
    for f in &files {
        q.add(&f.to_string_lossy(), prepend);
    }
    drop(q);
    if prepend {
        maybe_resync_queue();
    } else if driven() {
        maybe_resync_queue();
    }
    println!(
        "{} {} track(s) to queue.",
        if prepend { "Prepended" } else { "Added" },
        files.len()
    );
    save_queue();
    0
}

pub fn cli_list() -> i32 {
    let q = queue();
    if q.items.is_empty() {
        println!("(queue empty)");
        return 0;
    }
    for (i, it) in q.items.iter().enumerate() {
        let mark = if i == 0 { "▶" } else { " " };
        let d = if it.display.is_empty() {
            display_of(&it.path)
        } else {
            it.display.clone()
        };
        println!("  {mark} {:3}. {d}", i + 1);
    }
    0
}

pub fn cli_play() -> i32 {
    let removed: Vec<QueueItem> = {
        let mut q = queue();
        if q.items.is_empty() {
            println!("Queue empty.");
            return 1;
        }
        let mut kept = Vec::new();
        let mut gone = Vec::new();
        for it in q.items.drain(..) {
            if Path::new(&it.path).exists() {
                kept.push(it);
            } else {
                gone.push(it);
            }
        }
        q.items = kept;
        gone
    };
    for it in &removed {
        println!("Removed missing: {}", basename(&it.path));
    }
    if !removed.is_empty() {
        maybe_resync_queue();
        save_queue();
    }
    let q = queue();
    if q.items.is_empty() {
        println!("Queue empty after validation.");
        return 1;
    }
    drop(q);
    if play_queue_from(0, false) { 0 } else { 1 }
}

pub fn cli_queue_next() -> i32 {
    let q = queue();
    if q.items.is_empty() {
        println!("Queue empty.");
        return 1;
    }
    let cur = player::now_path();
    let idx = q.items.iter().position(|it| it.path == cur);
    drop(q);
    match idx {
        Some(i) => {
            let n = queue().items.len();
            if play_queue_from((i + 1) % n, true) { 0 } else { 1 }
        }
        None => {
            if play_queue_from(0, false) { 0 } else { 1 }
        }
    }
}

pub fn cli_remove(index_1based: &str) -> i32 {
    match index_1based.parse::<usize>() {
        Ok(n) if n >= 1 => {
            let mut q = queue();
            match q.remove(n - 1) {
                Some(it) => {
                    let d = if it.display.is_empty() {
                        display_of(&it.path)
                    } else {
                        it.display
                    };
                    drop(q);
                    maybe_resync_queue();
                    save_queue();
                    println!("Removed: {d}");
                    0
                }
                None => {
                    println!("Invalid index.");
                    1
                }
            }
        }
        _ => {
            println!("Usage: siren queue remove <index>");
            1
        }
    }
}

pub fn cli_move(a: &str, b: &str) -> i32 {
    let parse = |s: &str| s.parse::<usize>().ok().filter(|n| *n >= 1);
    match (parse(a), parse(b)) {
        (Some(f), Some(t)) => {
            let mut q = queue();
            if f - 1 < q.items.len() && t - 1 < q.items.len() {
                let it = q.items.remove(f - 1);
                q.items.insert(t - 1, it);
                drop(q);
                maybe_resync_queue();
                save_queue();
                println!("Moved.");
                0
            } else {
                println!("Invalid indices.");
                1
            }
        }
        _ => {
            println!("Usage: siren queue move <from> <to>");
            1
        }
    }
}

pub fn cli_clear() -> i32 {
    queue().clear();
    maybe_resync_queue();
    save_queue();
    println!("Queue cleared.");
    0
}

/// Replace the whole queue (mirrors Python `QUEUE.items = tracks`).
pub fn replace(items: Vec<QueueItem>) {
    queue().items = items;
    save_queue();
}

/// Snapshot for playlist save.
pub fn snapshot() -> Vec<QueueItem> {
    queue().items.clone()
}

/// Quiet single-path add (playlist load path).
pub fn cli_add_path(path: &str, prepend: bool) {
    queue().add(path, prepend);
}

/// Quiet clear (playlist load path — no resync, no print).
pub fn cli_clear_quiet() {
    queue().clear();
}
