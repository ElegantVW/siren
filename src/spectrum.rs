//! Real spectrum analysis — our own dependency, std only.
//!
//! Pipeline: system `ffmpeg` decodes to mono f32 PCM (writing an MP3
//! decoder ourselves would be madness; shells-to-system-tools is the
//! house pattern, like mpv/curl) → hand-rolled radix-2 FFT + Hann
//! window → log-spaced bands. No crates beyond std.
//!
//! Layout: fixed 64 bars (`▁▂▃▄▅▆▇█`), no cycling.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const SAMPLE_RATE: u32 = 22050;
pub const WINDOW: usize = 2048; // ~93ms per FFT
pub const HOP: usize = 1024; // 50% overlap
pub const BANDS: usize = 64;
pub const FMIN: f32 = 50.0;
pub const FMAX: f32 = 11025.0;

#[derive(Clone, Copy)]
struct C32 {
    re: f32,
    im: f32,
}

impl C32 {
    fn add(self, o: C32) -> C32 {
        C32 { re: self.re + o.re, im: self.im + o.im }
    }
    fn sub(self, o: C32) -> C32 {
        C32 { re: self.re - o.re, im: self.im - o.im }
    }
    fn mul(self, o: C32) -> C32 {
        C32 {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
    fn mag(self) -> f32 {
        (self.re * self.re + self.im * self.im).sqrt()
    }
}

/// In-place iterative radix-2 FFT. `buf.len()` must be a power of two.
fn fft(buf: &mut [C32]) {
    let n = buf.len();
    debug_assert!(n.is_power_of_two());
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j &= !bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            buf.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        let wlen = C32 { re: ang.cos(), im: ang.sin() };
        let mut i = 0;
        while i < n {
            let mut w = C32 { re: 1.0, im: 0.0 };
            for k in 0..len / 2 {
                let u = buf[i + k];
                let v = buf[i + k + len / 2].mul(w);
                buf[i + k] = u.add(v);
                buf[i + k + len / 2] = u.sub(v);
                w = w.mul(wlen);
            }
            i += len;
        }
        len <<= 1;
    }
}

fn hann(n: usize, i: usize) -> f32 {
    0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos())
}

/// One window of samples → 64 log-spaced magnitudes (raw, unnormalized).
fn frame_bands(samples: &[f32]) -> [f32; BANDS] {
    let mut buf = [C32 { re: 0.0, im: 0.0 }; WINDOW];
    for i in 0..WINDOW {
        buf[i].re = samples.get(i).copied().unwrap_or(0.0) * hann(WINDOW, i);
    }
    fft(&mut buf);
    let mut out = [0.0f32; BANDS];
    let ratio = FMAX / FMIN;
    for b in 0..BANDS {
        let f_lo = FMIN * ratio.powf(b as f32 / BANDS as f32);
        let f_hi = FMIN * ratio.powf((b + 1) as f32 / BANDS as f32);
        let k_lo = ((f_lo * WINDOW as f32 / SAMPLE_RATE as f32) as usize).max(1);
        let k_hi = ((f_hi * WINDOW as f32 / SAMPLE_RATE as f32) as usize + 1).min(WINDOW / 2);
        let mut m: f32 = 0.0;
        for k in k_lo..k_hi.max(k_lo + 1) {
            m = m.max(buf[k].mag());
        }
        out[b] = m / WINDOW as f32;
    }
    out
}

#[derive(Clone)]
pub struct TrackSpec {
    pub frames: Vec<[f32; BANDS]>,
    /// seconds per frame
    pub frame_dur: f64,
    pub duration: f64,
}

impl TrackSpec {
    /// Bands at `secs` (clamped). Empty when no frames.
    pub fn at(&self, secs: f64) -> [f32; BANDS] {
        if self.frames.is_empty() {
            return [0.0; BANDS];
        }
        let i = (secs / self.frame_dur).clamp(0.0, (self.frames.len() - 1) as f64) as usize;
        self.frames[i]
    }
}

fn cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    PathBuf::from(home).join(".cache/siren/spectrum")
}

fn cache_key(path: &Path) -> Option<String> {
    let meta = path.metadata().ok()?;
    let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    use std::hash::{Hash, Hasher};
    path.to_string_lossy().hash(&mut h);
    mtime.hash(&mut h);
    meta.len().hash(&mut h);
    Some(format!("{:016x}", h.finish()))
}

fn cache_path(key: &str) -> PathBuf {
    cache_dir().join(format!("{key}.spec"))
}

fn write_cache(key: &str, spec: &TrackSpec) {
    let mut buf = Vec::with_capacity(16 + spec.frames.len() * BANDS * 4);
    buf.extend_from_slice(b"SPEC0001");
    buf.extend_from_slice(&(spec.frames.len() as u32).to_le_bytes());
    buf.extend_from_slice(&spec.frame_dur.to_le_bytes());
    for f in &spec.frames {
        for b in f {
            buf.extend_from_slice(&b.to_le_bytes());
        }
    }
    let p = cache_path(key);
    if std::fs::create_dir_all(cache_dir()).is_ok() {
        let _ = std::fs::write(p, buf);
    }
}

fn read_cache(key: &str) -> Option<TrackSpec> {
    let raw = std::fs::read(cache_path(key)).ok()?;
    if raw.len() < 16 || &raw[..8] != b"SPEC0001" {
        return None;
    }
    let n = u32::from_le_bytes(raw[8..12].try_into().ok()?) as usize;
    let frame_dur = f64::from_le_bytes(raw[12..20].try_into().ok()?);
    if raw.len() != 20 + n * BANDS * 4 {
        return None;
    }
    let mut frames = Vec::with_capacity(n);
    for i in 0..n {
        let mut band = [0.0f32; BANDS];
        for b in 0..BANDS {
            let o = 20 + (i * BANDS + b) * 4;
            band[b] = f32::from_le_bytes(raw[o..o + 4].try_into().ok()?);
        }
        frames.push(band);
    }
    let duration = n as f64 * frame_dur;
    Some(TrackSpec { frames, frame_dur, duration })
}

fn decode_pcm(path: &Path) -> Result<Vec<f32>, String> {
    let out = std::process::Command::new("ffmpeg")
        .args([
            "-v", "error", "-i",
            &path.to_string_lossy(),
            "-f", "f32le", "-ac", "1", "-ar", &SAMPLE_RATE.to_string(),
            "-map", "a", "-",
        ])
        .output()
        .map_err(|e| format!("ffmpeg missing/failed: {e}"))?;
    if !out.status.success() {
        return Err("ffmpeg decode failed".into());
    }
    let bytes = out.stdout;
    let mut pcm = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        pcm.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    if pcm.is_empty() {
        return Err("no audio".into());
    }
    Ok(pcm)
}

fn analyze(pcm: &[f32]) -> TrackSpec {
    let frame_dur = HOP as f64 / SAMPLE_RATE as f64;
    let mut frames = Vec::new();
    let mut pos = 0;
    while pos + WINDOW <= pcm.len() {
        frames.push(frame_bands(&pcm[pos..pos + WINDOW]));
        pos += HOP;
    }
    if frames.is_empty() && !pcm.is_empty() {
        frames.push(frame_bands(pcm));
    }
    // per-track normalize (sqrt curve for visible mids)
    let peak = frames
        .iter()
        .flat_map(|f| f.iter())
        .copied()
        .fold(0.0f32, f32::max)
        .max(1e-9);
    for f in frames.iter_mut() {
        for b in f.iter_mut() {
            *b = (*b / peak).sqrt().clamp(0.0, 1.0);
        }
    }
    let duration = frames.len() as f64 * frame_dur;
    TrackSpec { frames, frame_dur, duration }
}

static MEM: OnceLock<Mutex<HashMap<String, TrackSpec>>> = OnceLock::new();
static PENDING: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();

fn mem() -> &'static Mutex<HashMap<String, TrackSpec>> {
    MEM.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cache-hit only. Never decodes (call `request_analyze` first).
pub fn spectrum_for(path: &Path) -> Option<TrackSpec> {
    let key = cache_key(path)?;
    if let Some(s) = mem().lock().unwrap().get(&key) {
        return Some(s.clone());
    }
    if let Some(s) = read_cache(&key) {
        // keep memory bounded: last 2 tracks
        let mut m = mem().lock().unwrap();
        while m.len() >= 2 {
            if let Some(k) = m.keys().next().cloned() {
                m.remove(&k);
            }
        }
        m.insert(key, s.clone());
        return Some(s);
    }
    None
}

/// Decode+analyze in a background thread (no-op if cached/pending).
pub fn request_analyze(path: &Path) {
    let key = match cache_key(path) {
        Some(k) => k,
        None => return,
    };
    if mem().lock().unwrap().contains_key(&key) || cache_path(&key).exists() {
        return;
    }
    {
        let mut p = PENDING.get_or_init(|| Mutex::new(std::collections::HashSet::new())).lock().unwrap();
        if !p.insert(key.clone()) {
            return;
        }
    }
    let owned = path.to_path_buf();
    std::thread::spawn(move || {
        if let Ok(pcm) = decode_pcm(&owned) {
            let spec = analyze(&pcm);
            write_cache(&key, &spec);
            mem().lock().unwrap().insert(key.clone(), spec);
        }
        PENDING.get().map(|p| p.lock().unwrap().remove(&key));
    });
}

/// Bar glyphs for 0..1 (7 levels).
pub fn bar_glyph(v: f32) -> char {
    const G: [char; 8] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇'];
    G[(v.clamp(0.0, 1.0) * 7.0).round() as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 440Hz sine must peak in the right bin (bin = f*W/SR ≈ 40.9).
    #[test]
    fn fft_finds_tone() {
        let mut buf = [C32 { re: 0.0, im: 0.0 }; WINDOW];
        for i in 0..WINDOW {
            buf[i].re = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / SAMPLE_RATE as f32).sin()
                * hann(WINDOW, i);
        }
        fft(&mut buf);
        let mut best = 0;
        let mut best_m = 0.0;
        for k in 1..WINDOW / 2 {
            let m = buf[k].mag();
            if m > best_m {
                best_m = m;
                best = k;
            }
        }
        assert!((38..=44).contains(&best), "peak at bin {best}, want ~41");
        // leakage check: peak towers over the average
        let avg: f32 = (1..WINDOW / 2).map(|k| buf[k].mag()).sum::<f32>() / (WINDOW / 2 - 1) as f32;
        assert!(best_m > avg * 10.0, "peak {best_m} vs avg {avg}");
    }

    #[test]
    fn bands_cover_tone() {
        let n = SAMPLE_RATE as usize; // 1s of 440Hz
        let pcm: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / n as f32).sin())
            .collect();
        let spec = analyze(&pcm);
        assert!(!spec.frames.is_empty());
        // 440Hz = band ~26 of 64 log bands (50Hz..11kHz); energy concentrates there
        let f0 = spec.frames[spec.frames.len() / 2];
        let mid: f32 = f0[20..32].iter().sum();
        let hi: f32 = f0[48..].iter().sum();
        assert!(mid > hi * 3.0, "mid {mid} vs high {hi}");
    }
}
