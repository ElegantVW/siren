//! Trove — free & legal music (Internet Archive + ccMixter).
//!
//! HTTP via system `curl` — zero new Rust dependencies.
//! Music scope: audio kinds by default; `get` works for anything.
//!
//! Paging: `search`/`search_page` take (rows, page). TUI prefetches the
//! next page before the cursor hits the end; CLI has `[m] more`.

use serde_json::Value;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const UA: &str = "siren-trove/1.0 (legal archive.org + ccmixter client)";
const IA_SEARCH: &str = "https://archive.org/advancedsearch.php";
const CC_QUERY: &str = "https://ccmixter.org/api/query";

/// TUI page size. CLI `siren trove N …` still overrides (clamped 1–50).
pub const PAGE_SIZE: u32 = 20;
/// Prefetch the next page when the cursor is this close to the end.
pub const PREFETCH_WITHIN: usize = 6;

fn audio_dir() -> PathBuf {
    std::env::var("TROVE_AUDIO_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into()))
                .join("Music/trove")
        })
}

fn video_dir() -> PathBuf {
    std::env::var("TROVE_VIDEO_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into()))
                .join("Videos/trove")
        })
}

fn max_total() -> u64 {
    std::env::var("TROVE_MAX_TOTAL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn no_confirm() -> bool {
    matches!(
        std::env::var("TROVE_NO_CONFIRM").unwrap_or_default().as_str(),
        "1" | "yes" | "true" | "on"
    )
}

/// (mediatype, hint) — mirrors KIND_MAP in ia.py.
fn kind_map(kind: &str) -> Option<(&'static str, &'static str)> {
    Some(match kind.to_lowercase().as_str() {
        "music" => ("audio", "(subject:(music) OR collection:(opensource_audio) OR collection:(netlabels))"),
        "tracks" | "songs" => ("audio", "(subject:(music) OR collection:(opensource_audio))"),
        "album" => ("audio", "(subject:(album) OR subject:(music) OR collection:(netlabels))"),
        "podcast" | "podcasts" | "shows" => ("audio", "(subject:(podcast) OR collection:(podcasts) OR title:(podcast))"),
        "audiobook" | "audiobooks" => ("audio", "(collection:(librivoxaudio) OR subject:(audiobook) OR creator:(LibriVox))"),
        "live" | "etree" | "lma" | "jam" => ("etree", "collection:(etree)"),
        "movie" => ("movies", "(subject:(feature) OR subject:(film) OR collection:(feature_films))"),
        "movies" => ("movies", "(subject:(feature) OR collection:(feature_films))"),
        "films" => ("movies", "(subject:(film) OR collection:(feature_films))"),
        "series" => ("movies", "(subject:(series) OR subject:(television) OR subject:(episode))"),
        "video" => ("movies", ""),
        "documentary" | "documentaries" => ("movies", "(subject:(documentary) OR collection:(documentary))"),
        _ => return None,
    })
}

pub fn is_ccmixter_kind(s: &str) -> bool {
    matches!(
        s.to_lowercase().as_str(),
        "ccmixter" | "cc" | "remix" | "remixes" | "mixter"
    )
}

pub fn is_kind_token(s: &str) -> bool {
    let lo = s.to_lowercase();
    kind_map(&lo).is_some() || lo == "audio" || lo == "films" || is_ccmixter_kind(&lo)
}

pub fn is_ccmixter_ident(ident: &str) -> bool {
    ident.starts_with("ccmixter:")
}

pub fn build_query(kind: Option<&str>, terms: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(k) = kind {
        let kl = k.to_lowercase();
        if let Some((mt, hint)) = kind_map(&kl) {
            parts.push(format!("mediatype:({mt})"));
            if !hint.is_empty() {
                parts.push(hint.to_string());
            }
        } else if kl == "audio" {
            parts.push("mediatype:(audio)".into());
        } else if kl == "movies" || kl == "films" {
            parts.push("mediatype:(movies)".into());
        }
    }
    let mut words: Vec<String> = terms.to_vec();
    if let Some(k) = kind {
        if !is_kind_token(k) {
            words.insert(0, k.to_string());
        }
    }
    if !words.is_empty() {
        let esc = words.join(" ").replace('"', " ");
        parts.push(format!(
            "(title:({esc}) OR subject:({esc}) OR description:({esc}) OR creator:({esc}))"
        ));
    }
    if parts.is_empty() {
        parts.push("mediatype:(audio)".into());
    }
    parts.join(" AND ")
}

fn curl_text(url: &str) -> Result<String, String> {
    let out = Command::new("curl")
        .args([
            "-fsSL", "--max-time", "30", "-A", UA, "--retry", "2",
            "--retry-delay", "2", url,
        ])
        .output()
        .map_err(|e| format!("curl missing/failed: {e}"))?;
    if !out.status.success() {
        return Err(format!("http {}", out.status));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("utf8: {e}"))
}

fn first_str(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str())
            .take(2)
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    #[default]
    Archive,
    CcMixter,
}

#[derive(Debug, Clone, Default)]
pub struct Doc {
    pub identifier: String,
    pub title: String,
    pub creator: String,
    pub year: String,
    pub mediatype: String,
    pub downloads: String,
    pub source: Source,
}

fn to_doc(v: &Value) -> Doc {
    Doc {
        identifier: first_str(v, "identifier"),
        title: first_str(v, "title"),
        creator: first_str(v, "creator"),
        year: first_str(v, "year"),
        mediatype: first_str(v, "mediatype"),
        downloads: first_str(v, "downloads"),
        source: Source::Archive,
    }
}

/// Dispatch search: ccMixter kinds hit ccmixter.org, everything else IA.
pub fn search_page(
    kind: Option<&str>,
    terms: &[String],
    rows: u32,
    page: u32,
) -> Result<(Vec<Doc>, u64), String> {
    let rows = rows.clamp(1, 50);
    let page = page.max(1);
    if kind.map(is_ccmixter_kind).unwrap_or(false) {
        let q = terms.join(" ");
        let offset = (page - 1) * rows;
        return search_ccmixter(&q, rows, offset);
    }
    let query = build_query(kind, terms);
    search(&query, rows, page)
}

/// Search archive.org. Returns (docs, num_found).
pub fn search(query: &str, rows: u32, page: u32) -> Result<(Vec<Doc>, u64), String> {
    let rows = rows.clamp(1, 50).to_string();
    let enc = percent_encode(query);
    let mut url = format!(
        "{IA_SEARCH}?q={enc}&rows={rows}&page={page}&output=json&sort%5B%5D=downloads+desc"
    );
    for fl in ["identifier", "title", "creator", "year", "mediatype", "downloads"] {
        url.push_str(&format!("&fl%5B%5D={fl}"));
    }
    let text = curl_text(&url)?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("json: {e}"))?;
    let resp = v.get("response").cloned().unwrap_or(Value::Null);
    let docs = resp
        .get("docs")
        .and_then(|d| d.as_array())
        .map(|a| a.iter().map(to_doc).collect())
        .unwrap_or_default();
    let n = resp.get("numFound").and_then(|x| x.as_u64()).unwrap_or(0);
    Ok((docs, n))
}

fn search_ccmixter(q: &str, rows: u32, offset: u32) -> Result<(Vec<Doc>, u64), String> {
    let mut base = format!("{CC_QUERY}?datasource=uploads&search_type=any");
    if !q.trim().is_empty() {
        base.push_str("&search=");
        base.push_str(&percent_encode(q.trim()));
    }
    let total = {
        let text = curl_text(&format!("{base}&f=count"))?;
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        match v {
            Value::Number(n) => n.as_u64().unwrap_or(0),
            Value::Array(a) => a.first().and_then(|x| x.as_u64()).unwrap_or(0),
            _ => 0,
        }
    };
    // ccmixter chokes curl past ~100KB per response (limit 20+); fetch in
    // 10s and concat so one logical page is still PAGE_SIZE rows.
    let mut docs: Vec<Doc> = Vec::new();
    let mut off = offset;
    let end = offset + rows;
    while off < end {
        let lim = (end - off).min(10);
        let text = curl_text(&format!("{base}&f=json&limit={lim}&offset={off}"))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("json: {e}"))?;
        let arr = v.as_array().cloned().unwrap_or_default();
        let n = arr.len();
        docs.extend(arr.iter().filter_map(cc_to_doc));
        off += lim;
        if (n as u32) < lim {
            break;
        }
    }
    Ok((docs, total))
}

fn cc_to_doc(v: &Value) -> Option<Doc> {
    let id = v.get("upload_id")?.as_u64().or_else(|| {
        v.get("upload_id")
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
    })?;
    let title = first_str(v, "upload_name");
    let creator = {
        let real = first_str(v, "user_real_name");
        if real.is_empty() {
            first_str(v, "user_name")
        } else {
            real
        }
    };
    Some(Doc {
        identifier: format!("ccmixter:{id}"),
        title,
        creator,
        year: String::new(),
        mediatype: "ccmixter".into(),
        downloads: first_str(v, "upload_num_scores"),
        source: Source::CcMixter,
    })
}

/// Append `src` onto `dst`, skipping duplicate identifiers.
pub fn append_unique(dst: &mut Vec<Doc>, src: Vec<Doc>) {
    for d in src {
        if !dst.iter().any(|x| x.identifier == d.identifier) {
            dst.push(d);
        }
    }
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else if b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn item_meta(ident: &str) -> Result<Value, String> {
    let url = format!("https://archive.org/metadata/{}", percent_encode_path(ident));
    let text = curl_text(&url)?;
    serde_json::from_str(&text).map_err(|e| format!("json: {e}"))
}

fn percent_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct Target {
    pub url: String,
    pub name: String,
    pub dir: PathBuf,
    pub size: u64,
}

const AUDIO_EXT: &[&str] = &[".mp3", ".ogg", ".flac", ".m4a", ".wav", ".opus"];
const VIDEO_EXT: &[&str] = &[".mp4", ".mkv", ".webm", ".avi", ".ogv", ".mov"];

fn sanitize_ident(ident: &str) -> String {
    ident
        .chars()
        .map(|c| if c.is_alphanumeric() || ".-_".contains(c) { c } else { '_' })
        .take(80)
        .collect()
}

/// Preferred display order for format choice.
pub const FORMAT_ORDER: &[&str] = &["mp3", "flac", "ogg", "m4a", "wav", "opus"];

fn ext_of(name: &str) -> Option<String> {
    let lo = name.to_lowercase();
    AUDIO_EXT
        .iter()
        .chain(VIDEO_EXT.iter())
        .find(|e| lo.ends_with(**e))
        .map(|e| e.trim_start_matches('.').to_string())
}

/// Distinct audio formats an item offers, in preference order.
pub fn available_formats(ident: &str, mediatype: &str) -> Result<Vec<String>, String> {
    if is_ccmixter_ident(ident) {
        let targets = pick_ccmixter_targets(ident, None)?;
        let mut have: Vec<String> = Vec::new();
        for t in &targets {
            if let Some(e) = ext_of(&t.name) {
                if AUDIO_EXT.contains(&format!(".{e}").as_str()) && !have.contains(&e) {
                    have.push(e);
                }
            }
        }
        have.sort_by_key(|e| FORMAT_ORDER.iter().position(|o| o == e).unwrap_or(99));
        return Ok(have);
    }
    let meta = item_meta(ident)?;
    let files = meta.get("files").and_then(|f| f.as_array()).cloned().unwrap_or_default();
    let mut have: Vec<String> = Vec::new();
    for f in &files {
        if let Some(n) = f.get("name").and_then(|x| x.as_str()) {
            if let Some(e) = ext_of(n) {
                if AUDIO_EXT.contains(&format!(".{e}").as_str()) && !have.contains(&e) {
                    have.push(e);
                }
            }
        }
    }
    have.sort_by_key(|e| FORMAT_ORDER.iter().position(|o| o == e).unwrap_or(99));
    let _ = mediatype;
    Ok(have)
}

/// Everything the item offers (unfiltered) + its format list.
pub fn plan_download(ident: &str, mediatype: &str) -> Result<(Vec<Target>, Vec<String>), String> {
    let targets = pick_targets(ident, mediatype, None)?;
    let mut fmts: Vec<String> = Vec::new();
    for t in &targets {
        if let Some(e) = ext_of(&t.name) {
            if AUDIO_EXT.contains(&format!(".{e}").as_str()) && !fmts.contains(&e) {
                fmts.push(e);
            }
        }
    }
    fmts.sort_by_key(|e| FORMAT_ORDER.iter().position(|o| o == e).unwrap_or(99));
    Ok((targets, fmts))
}

fn pick_ccmixter_targets(ident: &str, format: Option<&str>) -> Result<Vec<Target>, String> {
    let id = ident.strip_prefix("ccmixter:").unwrap_or(ident);
    let text = curl_text(&format!("{CC_QUERY}?f=json&ids={id}"))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("json: {e}"))?;
    let item = v
        .as_array()
        .and_then(|a| a.first())
        .cloned()
        .ok_or_else(|| format!("ccmixter item not found: {id}"))?;
    let files = item
        .get("files")
        .and_then(|f| f.as_array())
        .cloned()
        .unwrap_or_default();
    let dest_dir = audio_dir().join(sanitize_ident(&format!("ccmixter-{id}")));
    let want = format.map(|s| s.to_lowercase());
    let mut targets = Vec::new();
    for f in &files {
        let name = f.get("file_name").and_then(|x| x.as_str()).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let url = f
            .get("download_url")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if url.is_empty() {
            continue;
        }
        let ext = ext_of(name).unwrap_or_default();
        let is_audio = AUDIO_EXT.contains(&format!(".{ext}").as_str());
        if let Some(w) = &want {
            if w != "all" && ext != *w {
                continue;
            }
        } else if !is_audio {
            continue;
        }
        let size = f.get("file_rawsize").and_then(|s| s.as_u64()).unwrap_or(0);
        let base = name.rsplit('/').next().unwrap_or(name).to_string();
        targets.push(Target {
            url,
            name: base,
            dir: dest_dir.clone(),
            size,
        });
        if targets.len() >= 40 {
            break;
        }
    }
    Ok(targets)
}

/// (url, filename, dest_dir, size) — video decided by mediatype.
/// `format`: Some(ext) keeps one file per song (stem-dedupe); None/"all" keeps everything.
pub fn pick_targets(ident: &str, mediatype: &str, format: Option<&str>) -> Result<Vec<Target>, String> {
    if is_ccmixter_ident(ident) {
        return pick_ccmixter_targets(ident, format);
    }
    let meta = item_meta(ident)?;
    let files = meta.get("files").and_then(|f| f.as_array()).cloned().unwrap_or_default();
    let mt = if mediatype.is_empty() {
        meta.get("mediatype").and_then(|m| m.as_str()).unwrap_or("").to_lowercase()
    } else {
        mediatype.to_lowercase()
    };
    let suffix_of = |n: &str| {
        let lo = n.to_lowercase();
        AUDIO_EXT.iter().find(|e| lo.ends_with(*e)).map(|e| e.to_string())
            .or_else(|| VIDEO_EXT.iter().find(|e| lo.ends_with(*e)).map(|e| e.to_string()))
    };
    let mut media: Vec<&Value> = files
        .iter()
        .filter(|f| {
            f.get("name")
                .and_then(|n| n.as_str())
                .map(|n| suffix_of(n).is_some())
                .unwrap_or(false)
        })
        .collect();
    if media.is_empty() {
        return Ok(Vec::new());
    }
    let mut is_video = mt.starts_with("movie");
    if !is_video {
        let has_audio = media.iter().any(|f| {
            f.get("name")
                .and_then(|n| n.as_str())
                .map(|n| AUDIO_EXT.iter().any(|e| n.to_lowercase().ends_with(e)))
                .unwrap_or(false)
        });
        if !has_audio {
            is_video = true;
        }
    }
    let dest_root = if is_video { video_dir() } else { audio_dir() };
    let dest_dir = dest_root.join(sanitize_ident(ident));
    if is_video {
        media.truncate(1);
    } else {
        media.truncate(40);
    }
    let want = format.map(|s| s.to_lowercase());
    let mut targets = Vec::new();
    let mut seen_stems: Vec<String> = Vec::new();
    for f in media {
        let name = f.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let ext = ext_of(name).unwrap_or_default();
        if let Some(w) = &want {
            if w != "all" && ext != *w {
                continue;
            }
        }
        // one file per song when a specific format is chosen
        if want.as_deref().is_some_and(|w| w != "all") {
            let stem: String = name
                .rsplit('/')
                .next()
                .unwrap_or(name)
                .chars()
                .take_while(|c| *c != '.')
                .collect::<String>()
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect();
            // strip trailing bitrate tags (64kb, vbr) so versions dedupe
            let stem = stem
                .trim_end_matches(|c: char| c.is_numeric() || c == 'k' || c == 'b')
                .trim_end_matches("_- ")
                .to_string();
            if seen_stems.contains(&stem) {
                continue;
            }
            seen_stems.push(stem);
        }
        let base = name.rsplit('/').next().unwrap_or(name).to_string();
        let url = format!(
            "https://archive.org/download/{}/{}",
            percent_encode_path(ident),
            percent_encode_path_keep_slash(name)
        );
        let size = f.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
        targets.push(Target { url, name: base, dir: dest_dir.clone(), size });
    }
    Ok(targets)
}

fn percent_encode_path_keep_slash(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn fmt_size(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.0} KB", n as f64 / 1024.0)
    } else if n < 1024 * 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    } else {
        format!("{:.2} GB", n as f64 / 1024.0 / 1024.0 / 1024.0)
    }
}

fn confirm(prompt: &str, default: bool) -> bool {
    if no_confirm() {
        return true;
    }
    eprint!("{prompt}{} ", if default { "[Y/n]" } else { "[y/N]" });
    let _ = io::stderr().flush();
    let mut ans = String::new();
    if io::stdin().read_line(&mut ans).is_err() {
        return false;
    }
    let ans = ans.trim().to_lowercase();
    if ans.is_empty() {
        return default;
    }
    ans.starts_with('y')
}

/// Download one file with resume (.part + Range via curl -C -).
/// Returns true on success. Ctrl-C kills curl; .part is kept.
/// `progress`: None → curl's own meter on stderr (CLI).
/// Some → curl runs silent (TUI-safe: no escape spam on the alt screen)
/// and the watcher reports 0..1 (None when size unknown) ~2Hz.
pub fn download_file(
    url: &str,
    dest: &std::path::Path,
    size: u64,
    progress: Option<&mut dyn FnMut(Option<f64>)>,
) -> bool {
    if let Err(e) = std::fs::create_dir_all(dest.parent().unwrap()) {
        eprintln!("  mkdir failed: {e}");
        return false;
    }
    if dest.exists() && dest.metadata().map(|m| m.len()).unwrap_or(0) > 0 {
        eprintln!("  skip (exists): {}", dest.file_name().unwrap_or_default().to_string_lossy());
        return true;
    }
    // "<name>.part" beside the destination (with_extension would eat the ext):
    let part = dest.with_file_name(format!(
        "{}.part",
        dest.file_name().unwrap_or_default().to_string_lossy()
    ));
    eprintln!(
        "  ↓ {}  {}",
        dest.file_name().unwrap_or_default().to_string_lossy(),
        if size > 0 { fmt_size(size) } else { "?".into() }
    );
    let silent = progress.is_some();
    let meter_args: &[&str] = if silent {
        // TUI/headless: no escape spam on the alt screen; progress via .part poll
        &["-fL", "--retry", "2", "--retry-delay", "2", "--max-time", "0", "-C", "-", "-sS"]
    } else {
        &["-fL", "--retry", "2", "--retry-delay", "2", "--max-time", "0", "-C", "-", "--progress-bar"]
    };
    let mut cmd = Command::new("curl");
    cmd.args(meter_args).arg("-A").arg(UA);
    // ccmixter hotlink-guards /content/* (403 without a same-origin Referer)
    if url.contains("ccmixter.org") {
        cmd.arg("-e").arg("https://ccmixter.org/");
    }
    let mut child = match cmd.arg("-o").arg(&part).arg(url).stderr(Stdio::inherit()).spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  curl spawn failed: {e}");
            return false;
        }
    };
    // poll .part while curl writes (~4Hz) when the owner wants progress
    if let Some(cb) = progress {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let cur = part.metadata().map(|m| m.len()).unwrap_or(0);
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    cb(if size > 0 {
                        Some((cur as f64 / size as f64).clamp(0.0, 1.0))
                    } else {
                        None
                    });
                }
                Err(_) => break,
            }
        }
    }
    let st = child.wait();
    match st {
        Ok(s) if s.success() => {
            if std::fs::rename(&part, dest).is_err() {
                eprintln!("  rename failed");
                return false;
            }
            true
        }
        _ => {
            // drop empty .part like Python; keep partial for resume
            if part.exists() && part.metadata().map(|m| m.len()).unwrap_or(1) == 0 {
                let _ = std::fs::remove_file(&part);
            }
            eprintln!("  download failed: {}", dest.file_name().unwrap_or_default().to_string_lossy());
            false
        }
    }
}

/// Headless download worker. Returns (ok, total, dest_dir).
/// `assume_yes` skips the size confirm (TUI thread — no stdin there).
/// `format`: Some(ext)|Some("all")|None(all). `progress` fires per file.
/// Errors are returned as strings (empty-targets / over-cap / metadata).
pub fn do_download_inner(
    ident: &str,
    mediatype: &str,
    assume_yes: bool,
    format: Option<&str>,
    mut progress: Option<&mut dyn FnMut(usize, usize, &str, bool)>,
    mut live: Option<&mut dyn FnMut(usize, usize, &str, Option<f64>)>,
) -> Result<(usize, usize, PathBuf), String> {
    let targets =
        pick_targets(ident, mediatype, format).map_err(|e| format!("metadata error: {e}"))?;
    if targets.is_empty() {
        return Err(format!(
            "No audio/video files found (metadata only?). Browse: https://archive.org/details/{ident}"
        ));
    }
    let total: u64 = targets.iter().map(|t| t.size).sum();
    let dir = targets[0].dir.clone();
    let cap = max_total();
    if cap > 0 && total > cap {
        return Err(format!(
            "refusing: {} exceeds TROVE_MAX_TOTAL={}",
            fmt_size(total),
            fmt_size(cap)
        ));
    }
    if !assume_yes && (targets.len() > 1 || total > 32 * 1024 * 1024) {
        let label = format!(
            "{} file(s) · {} → {}",
            targets.len(),
            fmt_size(total),
            dir.display()
        );
        if !confirm(&format!("Download {label}?"), false) {
            return Err("skipped.".into());
        }
    }
    let mut ok = 0;
    let n = targets.len();
    for (i, t) in targets.iter().enumerate() {
        // live % with file context for the owner's progress line
        let good = if let Some(live_cb) = live.as_mut() {
            let name = t.name.clone();
            let mut tick = |frac: Option<f64>| live_cb(i + 1, n, &name, frac);
            download_file(&t.url, &t.dir.join(&t.name), t.size, Some(&mut tick))
        } else {
            download_file(&t.url, &t.dir.join(&t.name), t.size, None)
        };
        if good {
            ok += 1;
        }
        if let Some(cb) = progress.as_mut() {
            cb(i + 1, targets.len(), &t.name, good);
        }
    }
    Ok((ok, targets.len(), dir))
}

pub fn do_download(ident: &str, mediatype: &str, title: &str, format: Option<&str>) {
    eprintln!("Fetching file list for {ident} …");
    match do_download_inner(ident, mediatype, false, format, None, None) {
        Ok((ok, n, dir)) => eprintln!("Done: {ok}/{n}  ({title} → {})", dir.display()),
        Err(e) => eprintln!("{e}"),
    }
}

/// Ask which of the available formats to take. Returns "all" or an ext.
pub fn prompt_format(formats: &[String]) -> String {
    if formats.len() <= 1 {
        return formats.first().cloned().unwrap_or_else(|| "all".into());
    }
    eprint!("format ({})? [{}] ", formats.join("/"), formats[0]);
    let _ = std::io::stderr().flush();
    let mut ans = String::new();
    if std::io::stdin().read_line(&mut ans).is_err() {
        return formats[0].clone();
    }
    let ans = ans.trim().to_lowercase();
    if ans.is_empty() {
        return formats[0].clone();
    }
    if ans == "all" || formats.contains(&ans) {
        return ans;
    }
    eprintln!("  unknown format — taking {}", formats[0]);
    formats[0].clone()
}

pub fn doc_lines(i: usize, d: &Doc) -> (String, String) {
    let title = if d.title.is_empty() { d.identifier.clone() } else { d.title.clone() };
    let meta = [
        d.creator.chars().take(40).collect::<String>(),
        d.year.clone(),
        d.mediatype.clone(),
        if d.downloads.is_empty() { String::new() } else { format!("↓{}", d.downloads) },
    ]
    .into_iter()
    .filter(|x| !x.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    (
        format!("{:>2}.  {}", i, title.chars().take(68).collect::<String>()),
        format!("    {meta}  {}", d.identifier),
    )
}

/// `siren trove …` — search archive.org, interactively download.
pub fn run_trove(
    n: u32,
    terms: &[String],
    kind: Option<&str>,
    format: Option<String>,
) -> i32 {
    let n = n.clamp(1, 50);
    let mut k = kind.map(|s| s.to_string());
    let mut words: Vec<String> = terms.to_vec();
    if k.is_none() && !words.is_empty() && is_kind_token(&words[0]) {
        k = Some(words.remove(0));
    }
    let mut page = 1u32;
    let mut docs: Vec<Doc> = Vec::new();
    let mut num_found = 0u64;
    loop {
        let where_ = if k.as_deref().map(is_ccmixter_kind).unwrap_or(false) {
            "ccmixter.org"
        } else {
            "archive.org"
        };
        if page == 1 {
            eprintln!("  searching {where_} …");
        } else {
            eprintln!("  more from {where_} (page {page}) …");
        }
        let t0 = std::time::Instant::now();
        match search_page(k.as_deref(), &words, n, page) {
            Ok((chunk, total)) => {
                num_found = total;
                let before = docs.len();
                append_unique(&mut docs, chunk);
                eprintln!(
                    "  got {} hits ({} total) in {:.1}s",
                    docs.len() - before,
                    num_found,
                    t0.elapsed().as_secs_f32()
                );
            }
            Err(e) => {
                eprintln!("  search failed: {e}");
                eprintln!("  tip: ether net checks the weave");
                return 1;
            }
        }
        let qlabel = if k.as_deref().map(is_ccmixter_kind).unwrap_or(false) {
            format!("ccmixter {}", words.join(" "))
        } else {
            build_query(k.as_deref(), &words)
        };
        let more = (num_found > 0 && (docs.len() as u64) < num_found)
            || (num_found == 0 && docs.len() >= (page * n) as usize);
        match interactive_list(&docs, num_found, &qlabel, format.clone(), more) {
            Loop::Quit => return 0,
            Loop::More => {
                page += 1;
                continue;
            }
            Loop::Again => {
                eprint!("new search words (or empty to keep): ");
                let _ = io::stderr().flush();
                let mut line = String::new();
                if io::stdin().read_line(&mut line).is_err() {
                    return 0;
                }
                let mut parts: Vec<String> = line.split_whitespace().map(|s| s.to_string()).collect();
                if !parts.is_empty() {
                    if is_kind_token(&parts[0]) {
                        k = Some(parts.remove(0));
                        words = parts;
                    } else {
                        words = parts;
                    }
                }
                page = 1;
                docs.clear();
                num_found = 0;
            }
            Loop::Done => return 0,
        }
    }
}

enum Loop {
    Quit,
    Again,
    More,
    Done,
}

fn interactive_list(
    docs: &[Doc],
    num_found: u64,
    query: &str,
    format: Option<String>,
    more: bool,
) -> Loop {
    println!();
    println!("  query:   {query}");
    println!("  matches: {num_found}  ·  showing {}", docs.len());
    println!();
    if docs.is_empty() {
        println!("  No results. Try different words.");
        return Loop::Again;
    }
    for (i, d) in docs.iter().enumerate() {
        let (l1, l2) = doc_lines(i + 1, d);
        println!("  {l1}");
        println!("  {l2}");
        println!();
    }
    if more {
        println!("  [1-N] download   [a] all listed   [m] more   [s] search again   [q] quit");
    } else {
        println!("  [1-N] download   [a] all listed   [s] search again   [q] quit");
    }
    println!();
    loop {
        eprint!("trove> ");
        let _ = io::stderr().flush();
        let mut choice = String::new();
        if io::stdin().read_line(&mut choice).is_err() {
            println!();
            return Loop::Quit;
        }
        match choice.trim().to_lowercase().as_str() {
            "q" | "quit" | "exit" => return Loop::Quit,
            "s" | "search" | "again" | "r" => return Loop::Again,
            "m" | "more" | "n" | "next" if more => return Loop::More,
            "a" | "all" => {
                for d in docs {
                    let f = format.clone().or_else(|| choose_format(&d.identifier, &d.mediatype));
                    do_download(&d.identifier, &d.mediatype, &d.title, f.as_deref());
                }
                println!("Done. [s] new search, [q] quit.");
                return Loop::Done;
            }
            c if c.parse::<usize>().is_ok() => {
                let n: usize = c.parse().unwrap_or(0);
                if 1 <= n && n <= docs.len() {
                    let d = &docs[n - 1];
                    let f = format.clone().or_else(|| choose_format(&d.identifier, &d.mediatype));
                    do_download(&d.identifier, &d.mediatype, &d.title, f.as_deref());
                    println!("Another number, [m] more, [s] search again, or [q] quit?");
                    continue;
                }
                println!("Pick a number, a, m, s, or q.");
            }
            _ => println!("Pick a number, a, m, s, or q."),
        }
    }
}

/// Pick a format for one item: flag wins, else prompt when >1 available.
fn choose_format(ident: &str, mediatype: &str) -> Option<String> {
    match plan_download(ident, mediatype) {
        Ok((_, fmts)) if fmts.len() > 1 => Some(prompt_format(&fmts)),
        Ok(_) => None, // single format (or none) — no question
        Err(e) => {
            eprintln!("{e}");
            None
        }
    }
}

/// `siren trove get <identifier>`
pub fn run_get(identifier: &str, format: Option<String>) -> i32 {
    let ident = identifier.trim();
    if ident.is_empty() {
        eprintln!("usage: siren trove get <identifier> [--format EXT|all]");
        return 2;
    }
    let f = format.or_else(|| choose_format(ident, ""));
    do_download(ident, "", ident, f.as_deref());
    0
}

pub fn about_text() -> Vec<String> {
    vec![
        "siren trove — free & legal media".into(),
        "(Internet Archive + ccMixter)".into(),
        "".into(),
        "  siren trove music lofi          search + pick (20, then [m] more)".into(),
        "  siren trove live grateful       Live Music Archive (etree)".into(),
        "  siren trove ccmixter lofi       ccMixter remixes".into(),
        "  siren trove podcast history     kind + terms".into(),
        "  siren trove get <identifier>    download one item".into(),
        "  --format mp3|flac|ogg|all      pick a version (else it asks)".into(),
        "  siren trove about".into(),
        "".into(),
        "downloads land in ~/Music/trove and ~/Videos/trove".into(),
    ]
}
