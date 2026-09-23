//! Siren config — mirrors Python `SirenConfig` (`faeOS/bin/siren`).
//! File: `~/.config/siren/config.json`. Unknown keys are preserved on
//! load but never written back (same as Python: only known keys saved).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    PathBuf::from(home).join(".config/siren/config.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SirenConfig {
    #[serde(default = "d_volume")]
    pub default_volume: i32,
    #[serde(default = "d_roots")]
    pub library_roots: Vec<String>,
    #[serde(default = "d_true")]
    pub fuzzy_search: bool,
    #[serde(default = "d_true")]
    pub waves: bool,
    #[serde(default = "d_true")]
    pub gapless: bool,
    #[serde(default)]
    pub normalize: bool,
    #[serde(default = "d_true")]
    pub cache_meta: bool,
    #[serde(default = "d_bands")]
    pub wave_bands: i32,
    #[serde(default = "d_true")]
    pub mouse: bool,
    /// Audio routing: "local" (mpv) or "heos" (network speaker)
    #[serde(default = "d_output")]
    pub audio_output: String,
    /// Preferred HEOS speaker (name fragment)
    #[serde(default = "d_speaker")]
    pub audio_speaker: String,
}

fn d_volume() -> i32 {
    75
}
fn d_roots() -> Vec<String> {
    vec!["~/Music".into()]
}
fn d_true() -> bool {
    true
}
fn d_bands() -> i32 {
    16
}
fn d_output() -> String {
    "local".into()
}
fn d_speaker() -> String {
    "Vanguarda Office".into()
}

impl Default for SirenConfig {
    fn default() -> Self {
        Self {
            default_volume: 75,
            library_roots: d_roots(),
            fuzzy_search: true,
            waves: true,
            gapless: true,
            normalize: false,
            cache_meta: true,
            wave_bands: 16,
            mouse: true,
            audio_output: d_output(),
            audio_speaker: d_speaker(),
        }
    }
}

impl SirenConfig {
    pub fn load() -> Self {
        let mut cfg = Self::default();
        let path = config_path();
        let raw = std::fs::read_to_string(&path).unwrap_or_default();
        if raw.trim().is_empty() {
            return cfg;
        }
        let v: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => return cfg, // corrupt file → defaults (Python behavior)
        };
        if let Some(n) = v.get("default_volume").and_then(|x| x.as_i64()) {
            if (0..=150).contains(&n) {
                cfg.default_volume = n as i32;
            }
        }
        if let Some(arr) = v.get("library_roots").and_then(|x| x.as_array()) {
            let roots: Vec<String> = arr
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect();
            if !roots.is_empty() {
                cfg.library_roots = roots;
            }
        }
        for key in [
            "fuzzy_search",
            "waves",
            "gapless",
            "normalize",
            "cache_meta",
            "mouse",
        ] {
            if let Some(b) = v.get(key).and_then(|x| x.as_bool()) {
                match key {
                    "fuzzy_search" => cfg.fuzzy_search = b,
                    "waves" => cfg.waves = b,
                    "gapless" => cfg.gapless = b,
                    "normalize" => cfg.normalize = b,
                    "cache_meta" => cfg.cache_meta = b,
                    "mouse" => cfg.mouse = b,
                    _ => {}
                }
            }
        }
        if let Some(n) = v.get("wave_bands").and_then(|x| x.as_i64()) {
            if [8, 16, 32, 64].contains(&n) {
                cfg.wave_bands = n as i32;
            }
        }
        if let Some(s) = v.get("audio_output").and_then(|x| x.as_str()) {
            match s.to_lowercase().as_str() {
                "local" | "heos" => cfg.audio_output = s.to_lowercase(),
                _ => {}
            }
        }
        if let Some(s) = v.get("audio_speaker").and_then(|x| x.as_str()) {
            if !s.trim().is_empty() {
                cfg.audio_speaker = s.to_string();
            }
        }
        cfg
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path).context("config replace")?;
        Ok(())
    }

    /// Set one key from CLI text. Returns the display value. Mirrors
    /// Python `cli_set_config` error strings.
    pub fn set(&mut self, key: &str, val: &str) -> Result<String, String> {
        match key.to_lowercase().as_str() {
            "default_volume" => {
                let n: i32 = val
                    .trim()
                    .parse()
                    .map_err(|_| "expects an integer (0-150)".to_string())?;
                if !(0..=150).contains(&n) {
                    return Err("expects 0-150".to_string());
                }
                self.default_volume = n;
                Ok(n.to_string())
            }
            "library_roots" => {
                let parts: Vec<String> = val
                    .split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect();
                if parts.is_empty() {
                    return Err("expects comma-separated paths".to_string());
                }
                self.library_roots = parts.clone();
                Ok(parts.join(", "))
            }
            "fuzzy_search" | "waves" | "gapless" | "normalize" | "cache_meta"
            | "mouse" => {
                let b = matches!(
                    val.trim().to_lowercase().as_str(),
                    "1" | "yes" | "true" | "on"
                );
                match key.to_lowercase().as_str() {
                    "fuzzy_search" => self.fuzzy_search = b,
                    "waves" => self.waves = b,
                    "gapless" => self.gapless = b,
                    "normalize" => self.normalize = b,
                    "cache_meta" => self.cache_meta = b,
                    "mouse" => self.mouse = b,
                    _ => {}
                }
                Ok(if b { "on".into() } else { "off".into() })
            }
            // accepted for compat; the strip renders a fixed 64-bar field
            "wave_bands" => match val.trim() {
                "8" | "16" | "32" | "64" => {
                    self.wave_bands = val.trim().parse().unwrap();
                    Ok(val.trim().to_string())
                }
                _ => Err("expects 8, 16, 32 or 64".to_string()),
            },
            "audio_output" => match val.trim().to_lowercase().as_str() {
                "local" | "heos" => {
                    self.audio_output = val.trim().to_lowercase();
                    Ok(self.audio_output.clone())
                }
                _ => Err("expects local or heos".to_string()),
            },
            "audio_speaker" => {
                let v = val.trim().to_string();
                if v.is_empty() {
                    return Err("expects a speaker name".to_string());
                }
                self.audio_speaker = v.clone();
                Ok(v)
            }
            _ => Err(format!("unknown key: {key}")),
        }
    }

    pub fn get(&self, key: &str) -> Option<String> {
        match key.to_lowercase().as_str() {
            "default_volume" => Some(self.default_volume.to_string()),
            "library_roots" => Some(self.library_roots.join(", ")),
            "fuzzy_search" => Some(onoff(self.fuzzy_search)),
            "waves" => Some(onoff(self.waves)),
            "gapless" => Some(onoff(self.gapless)),
            "normalize" => Some(onoff(self.normalize)),
            "cache_meta" => Some(onoff(self.cache_meta)),
            "wave_bands" => Some(self.wave_bands.to_string()),
            "mouse" => Some(onoff(self.mouse)),
            "audio_output" => Some(self.audio_output.clone()),
            "audio_speaker" => Some(self.audio_speaker.clone()),
            _ => None,
        }
    }

    pub fn keys() -> &'static [&'static str] {
        &[
            "default_volume",
            "library_roots",
            "fuzzy_search",
            "waves",
            "gapless",
            "normalize",
            "cache_meta",
            "wave_bands",
            "mouse",
            "audio_output",
            "audio_speaker",
        ]
    }
}

fn onoff(b: bool) -> String {
    if b { "on".into() } else { "off".into() }
}
