//! Radio — community stations via radio-browser.info (keyless).
//!
//! HTTP via system `curl`, like trove. Countries cached a week on disk.
//! Lists sort alphabetically (case-insensitive) client-side — server
//! order is not trusted.

use serde::{Deserialize, Serialize};

const UA: &str = "siren-radio/1.0 (community radio client)";
const BASES: &[&str] = &[
    "https://de1.api.radio-browser.info/json",
    "https://de2.api.radio-browser.info/json",
];

pub const PAGE_SIZE: usize = 20;
pub const PREFETCH_WITHIN: usize = 6;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Station {
    pub uuid: String,
    pub name: String,
    pub url: String,
    pub codec: String,
    pub bitrate: u64,
    pub tags: String,
    pub country: String,
}

#[derive(Debug, Clone, Default)]
pub struct Country {
    pub name: String,
    pub count: u64,
}

fn curl_text(url: &str) -> Result<String, String> {
    // de1 first, de2 fallback on failure
    let mut err = String::new();
    let out = std::process::Command::new("curl")
        .args([
            "-fsSL", "--max-time", "25", "-A", UA, "--retry", "1",
            "--retry-delay", "1", url,
        ])
        .output()
        .map_err(|e| format!("curl missing/failed: {e}"))?;
    if !out.status.success() {
        err = format!("http {}", out.status);
        // retry other mirrors on failure
        if url.contains(BASES[0]) {
            let alt = url.replacen(BASES[0], BASES[1], 1);
            let out2 = std::process::Command::new("curl")
                .args([
                    "-fsSL", "--max-time", "25", "-A", UA, "--retry", "1",
                    "--retry-delay", "1", &alt,
                ])
                .output()
                .map_err(|e| format!("curl missing/failed: {e}"))?;
            if !out2.status.success() {
                return Err(format!("radio api down ({err})"));
            }
            return String::from_utf8(out2.stdout).map_err(|e| format!("utf8: {e}"));
        }
        return Err(err);
    }
    String::from_utf8(out.stdout).map_err(|e| format!("utf8: {e}"))
}

fn cache_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    std::path::PathBuf::from(format!("{home}/.cache/siren"))
}

fn state_path() -> std::path::PathBuf {
    let base = std::env::var("SIREN_CONFIG_DIR").unwrap_or_else(|_| {
        format!(
            "{}/.config/siren",
            std::env::var("HOME").unwrap_or_else(|_| "/root".into())
        )
    });
    std::path::PathBuf::from(base).join("radio.json")
}

fn by_name(mut v: Vec<Station>) -> Vec<Station> {
    v.sort_by_key(|s| s.name.to_lowercase());
    v
}

fn to_station(v: &serde_json::Value) -> Option<Station> {
    let url = v
        .get("url_resolved")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| v.get("url").and_then(|x| x.as_str()))
        .unwrap_or("");
    if url.is_empty() || (!url.starts_with("http://") && !url.starts_with("https://")) {
        return None;
    }
    // prefer stations that checked out OK (missing field = keep)
    if let Some(ok) = v.get("lastcheckok").and_then(|x| x.as_i64()) {
        if ok == 0 {
            return None;
        }
    }
    Some(Station {
        uuid: v.get("stationuuid").and_then(|x| x.as_str()).unwrap_or("").into(),
        name: v.get("name").and_then(|x| x.as_str()).unwrap_or(url).trim().into(),
        url: url.into(),
        codec: v.get("codec").and_then(|x| x.as_str()).unwrap_or("").into(),
        bitrate: v.get("bitrate").and_then(|x| x.as_u64()).unwrap_or(0),
        tags: v.get("tags").and_then(|x| x.as_str()).unwrap_or("").into(),
        country: v.get("country").and_then(|x| x.as_str()).unwrap_or("").into(),
    })
}

fn parse_stations(text: &str) -> Vec<Station> {
    let v: serde_json::Value = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
    let arr = v.as_array().cloned().unwrap_or_default();
    by_name(arr.iter().filter_map(to_station).collect())
}

/// Countries (name + station count), alphabetical, cached 7 days.
pub fn countries() -> Result<Vec<Country>, String> {
    let cp = cache_dir().join("radio-countries.json");
    if let Ok(raw) = std::fs::read_to_string(&cp) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            let fresh = v
                .get("ts")
                .and_then(|x| x.as_u64())
                .map(|ts| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs().saturating_sub(ts) < 7 * 86400)
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if fresh {
                let mut out: Vec<Country> = v
                    .get("countries")
                    .and_then(|x| x.as_array())
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|c| {
                        let name = c.get("name")?.as_str()?.trim().to_string();
                        if name.is_empty() {
                            return None;
                        }
                        Some(Country {
                            name,
                            count: c.get("count").and_then(|x| x.as_u64()).unwrap_or(0),
                        })
                    })
                    .collect();
                out.sort_by_key(|c| c.name.to_lowercase());
                if !out.is_empty() {
                    return Ok(out);
                }
            }
        }
    }
    let text = curl_text(&format!("{}/countries?order=stationcount&reverse=true", BASES[0]))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("json: {e}"))?;
    let mut out: Vec<Country> = v
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            let name = c.get("name")?.as_str()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(Country {
                name,
                count: c.get("stationcount").and_then(|x| x.as_u64()).unwrap_or(0),
            })
        })
        .collect();
    out.sort_by_key(|c| c.name.to_lowercase());
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let saved = serde_json::json!({
        "ts": ts,
        "countries": out.iter().map(|c| serde_json::json!({"name": c.name, "count": c.count})).collect::<Vec<_>>(),
    });
    let _ = std::fs::create_dir_all(cache_dir());
    let tmp = cp.with_extension("json.tmp");
    if std::fs::write(&tmp, saved.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, cp);
    }
    Ok(out)
}

fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else if b == b' ' {
            out.push_str("%20");
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Stations for a country, alphabetical page.
pub fn by_country(country: &str, limit: usize, offset: usize) -> Result<Vec<Station>, String> {
    let text = curl_text(&format!(
        "{}/stations/bycountry/{}?order=name&limit={limit}&offset={offset}",
        BASES[0],
        enc(country),
    ))?;
    Ok(parse_stations(&text))
}

/// Free-text station search, alphabetical page. Multi-word queries fall
/// back to per-token search merged + deduped (the API's `name=` is one
/// phrase, so "laut lofi" would otherwise miss "[laut.fm] lofi").
pub fn search(query: &str, limit: usize, offset: usize) -> Result<Vec<Station>, String> {
    let one = |q: &str, lim: usize, off: usize| -> Result<Vec<Station>, String> {
        let text = curl_text(&format!(
            "{}/stations/search?name={}&order=name&limit={lim}&offset={off}",
            BASES[0],
            enc(q),
        ))?;
        Ok(parse_stations(&text))
    };
    let first = one(query, limit, offset)?;
    if !first.is_empty() || !query.trim().contains(' ') || offset > 0 {
        return Ok(first);
    }
    let mut merged: Vec<Station> = Vec::new();
    for tok in query.split_whitespace() {
        match one(tok, limit, 0) {
            Ok(chunk) => {
                for s in chunk {
                    if !merged.iter().any(|m: &Station| {
                        (!m.uuid.is_empty() && m.uuid == s.uuid) || m.url == s.url
                    }) {
                        merged.push(s);
                    }
                }
            }
            Err(e) => return Err(e),
        }
    }
    merged.sort_by_key(|s| s.name.to_lowercase());
    merged.truncate(limit);
    Ok(merged)
}

pub fn to_queue_item(s: &Station) -> crate::queue::QueueItem {
    let artist = if s.country.is_empty() {
        "Radio".into()
    } else {
        format!("Radio · {}", s.country)
    };
    crate::queue::QueueItem {
        path: s.url.clone(),
        display: s.name.clone(),
        title: s.name.clone(),
        artist,
        duration: 0.0,
    }
}

// ---- favorites + last country ----

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RadioState {
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    favs: Vec<Station>,
}

fn load_state() -> RadioState {
    let raw = std::fs::read_to_string(state_path()).unwrap_or_default();
    serde_json::from_str(&raw).unwrap_or_default()
}

fn save_state(st: &RadioState) {
    let p = state_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_string_pretty(st).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, p);
    }
}

pub fn last_country() -> Option<String> {
    load_state().country
}

pub fn set_country(name: &str) {
    let mut st = load_state();
    st.country = Some(name.to_string());
    save_state(&st);
}

pub fn favs() -> Vec<Station> {
    let mut f = load_state().favs;
    f.sort_by_key(|s| s.name.to_lowercase());
    f
}

/// Toggle favorite by station uuid. Returns true when now a fav.
pub fn toggle_fav(s: &Station) -> bool {
    let mut st = load_state();
    if st.favs.iter().any(|f| f.uuid == s.uuid && !s.uuid.is_empty()) {
        st.favs.retain(|f| f.uuid != s.uuid);
        save_state(&st);
        false
    } else if st.favs.iter().any(|f| f.url == s.url) {
        st.favs.retain(|f| f.url != s.url);
        save_state(&st);
        false
    } else {
        st.favs.push(s.clone());
        save_state(&st);
        true
    }
}

pub fn about_text() -> Vec<String> {
    vec![
        "siren radio — community stations".into(),
        "(radio-browser.info, keyless)".into(),
        "".into(),
        "  siren radio countries         list countries (a-z)".into(),
        "  siren radio stations Portugal stations for a country".into(),
        "  siren radio search lofi       search by name".into(),
        "  siren radio play <words>      play top match (output channel)".into(),
        "  siren radio fav               list favorites".into(),
        "  siren radio fav <words>       toggle favorite".into(),
        "".into(),
        "TUI: Tab to radio, Enter picks a country, Enter plays.".into(),
    ]
}

pub fn is_fav(s: &Station) -> bool {
    let st = load_state();
    st.favs.iter().any(|f| {
        (!s.uuid.is_empty() && f.uuid == s.uuid) || f.url == s.url
    })
}
