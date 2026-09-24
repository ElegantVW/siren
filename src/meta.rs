//! Tag display via system `ffprobe` (zero new deps) with memory + disk
//! cache (`~/.cache/siren/meta.json`, keyed by path, mtime in the value).
//!
//! `display()` never blocks on a subprocess: misses return the stem and
//! warm in a background thread (cap 2); the next TUI tick shows tags.
//! CLI list/add uses `display_sync` so a one-shot process actually saves.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

#[derive(Clone)]
struct Entry {
    mtime: i64,
    artist: String,
    title: String,
}

static MEM: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
static PENDING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static DISK_LOADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static INFLIGHT: AtomicUsize = AtomicUsize::new(0);
const MAX_WORKERS: usize = 2;

fn mem() -> &'static Mutex<HashMap<String, Entry>> {
    MEM.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pending() -> std::sync::MutexGuard<'static, HashSet<String>> {
    PENDING
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap()
}

fn disk_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    PathBuf::from(format!("{home}/.cache/siren/meta.json"))
}

fn mtime_ns(path: &Path) -> i64 {
    path.metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(-1)
}

fn load_disk() {
    if DISK_LOADED.swap(true, Ordering::SeqCst) {
        return;
    }
    let raw = match std::fs::read_to_string(disk_path()) {
        Ok(s) => s,
        Err(_) => return,
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return,
    };
    let Some(obj) = v.as_object() else { return };
    let mut m = mem().lock().unwrap();
    for (k, e) in obj {
        // Rust shape {m,a,t}. Python fallback {m,d} has no a/t — skip so we re-probe.
        if e.get("a").is_none() && e.get("t").is_none() {
            continue;
        }
        let artist = e.get("a").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let title = e.get("t").and_then(|x| x.as_str()).unwrap_or("").to_string();
        // old rust keyed path:mtime — peel the suffix if the prefix is a real file
        let (path, mt_from_key) = match k.rsplit_once(':') {
            Some((p, rest)) if Path::new(p).is_file() && rest.parse::<i64>().is_ok() => {
                (p.to_string(), rest.parse::<i64>().ok())
            }
            _ => (k.clone(), None),
        };
        let mt = e
            .get("m")
            .and_then(|x| x.as_i64())
            .filter(|&n| n > 0)
            .or(mt_from_key)
            .unwrap_or(-1);
        // stem-as-title with no artist is leftover python `d`; don't cache it
        if artist.is_empty() {
            let stem = stem_of(Path::new(&path));
            if title.is_empty() || title == stem {
                continue;
            }
        }
        if mt < 0 {
            continue;
        }
        m.insert(
            path,
            Entry {
                mtime: mt,
                artist,
                title,
            },
        );
    }
}

fn save_disk() {
    let snapshot: Vec<(String, Entry)> = mem()
        .lock()
        .unwrap()
        .iter()
        .map(|(k, e)| (k.clone(), e.clone()))
        .collect();
    let mut obj = serde_json::Map::new();
    for (k, e) in snapshot {
        obj.insert(
            k,
            serde_json::json!({"m": e.mtime, "a": e.artist, "t": e.title}),
        );
    }
    let p = disk_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_string(&obj).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, p);
    }
}

fn stem_of(path: &Path) -> String {
    let p = path.to_string_lossy();
    let p = p.strip_prefix("file://").unwrap_or(&p);
    Path::new(p)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| p.to_string())
}

fn ffprobe_tags(path: &Path) -> Option<(String, String)> {
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            &path.to_string_lossy(),
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let tags = v.get("format")?.get("tags")?;
    let get = |keys: &[&str]| -> String {
        for key in keys {
            if let Some(s) = tags.get(*key).and_then(|x| x.as_str()) {
                if !s.trim().is_empty() {
                    return s.trim().to_string();
                }
            }
        }
        String::new()
    };
    let artist = get(&["artist", "ARTIST", "TPE1", "album_artist", "ALBUMARTIST"]);
    let title = get(&["title", "TITLE", "TIT2"]);
    if artist.is_empty() && title.is_empty() {
        return None;
    }
    Some((artist, title))
}

fn lookup(path: &Path) -> Option<(String, String)> {
    load_disk();
    let key = path.to_string_lossy().into_owned();
    let mt = mtime_ns(path);
    let m = mem().lock().unwrap();
    m.get(&key).and_then(|e| {
        if e.mtime == mt {
            Some((e.artist.clone(), e.title.clone()))
        } else {
            None
        }
    })
}

fn store(path: &Path, artist: String, title: String) {
    let key = path.to_string_lossy().into_owned();
    mem().lock().unwrap().insert(
        key,
        Entry {
            mtime: mtime_ns(path),
            artist,
            title,
        },
    );
    save_disk();
}

fn try_begin_worker() -> bool {
    loop {
        let n = INFLIGHT.load(Ordering::SeqCst);
        if n >= MAX_WORKERS {
            return false;
        }
        if INFLIGHT
            .compare_exchange(n, n + 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return true;
        }
    }
}

/// Enqueue a background probe if this path isn't cached yet.
pub fn warm(path: &Path) {
    if lookup(path).is_some() {
        return;
    }
    let key = path.to_string_lossy().into_owned();
    if pending().contains(&key) {
        return;
    }
    if !try_begin_worker() {
        return;
    }
    if !pending().insert(key) {
        INFLIGHT.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    let owned = path.to_path_buf();
    std::thread::spawn(move || {
        let hit = ffprobe_tags(&owned).unwrap_or_default();
        store(&owned, hit.0, hit.1);
        pending().remove(&owned.to_string_lossy().into_owned());
        INFLIGHT.fetch_sub(1, Ordering::SeqCst);
    });
}

fn format_tags(artist: &str, title: &str, path: &Path) -> String {
    match (artist.is_empty(), title.is_empty()) {
        (true, true) => stem_of(path),
        (true, false) => title.to_string(),
        (false, true) => artist.to_string(),
        (false, false) => format!("{artist} - {title}"),
    }
}

fn as_path(path: &str) -> &Path {
    Path::new(path.strip_prefix("file://").unwrap_or(path))
}

/// Cache-only (no spawn). Used by fuzzy resolve so CLI doesn't fork-bomb.
pub fn display_cached(path: &str) -> String {
    let pb = as_path(path);
    match lookup(pb) {
        Some((a, t)) => format_tags(&a, &t, pb),
        None => stem_of(pb),
    }
}

/// "Artist - Title" or stem fallback. Never blocks; warms misses (cap 2).
pub fn display(path: &str) -> String {
    let pb = as_path(path);
    match lookup(pb) {
        Some((a, t)) => format_tags(&a, &t, pb),
        None => {
            warm(pb);
            stem_of(pb)
        }
    }
}

/// Block on ffprobe for this one path. CLI one-shots need this so the
/// process doesn't exit before a worker can save.
pub fn display_sync(path: &str) -> String {
    let pb = as_path(path);
    if let Some((a, t)) = lookup(pb) {
        return format_tags(&a, &t, pb);
    }
    let (a, t) = ffprobe_tags(pb).unwrap_or_default();
    store(pb, a.clone(), t.clone());
    format_tags(&a, &t, pb)
}

/// (artist, title) for records; empty when unknown. Never blocks.
pub fn tags_for(path: &str) -> (String, String) {
    let pb = as_path(path);
    match lookup(pb) {
        Some(hit) => hit,
        None => {
            warm(pb);
            Default::default()
        }
    }
}

/// Blocking tags (CLI add). Empty strings when the file has none.
pub fn tags_for_sync(path: &str) -> (String, String) {
    let pb = as_path(path);
    if let Some(hit) = lookup(pb) {
        return hit;
    }
    let hit = ffprobe_tags(pb).unwrap_or_default();
    store(pb, hit.0.clone(), hit.1.clone());
    hit
}
