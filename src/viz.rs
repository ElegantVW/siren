//! Viz — real-time spectrum visualizer.
//!
//! Renders the live audio (Siren_Master monitor) as a braille bar
//! chart, in two orientations:
//!   - horizontal: bands left→right, bars grow up   (wide/short boxes)
//!   - vertical:   bands top→bottom, bars grow right (tall/narrow boxes)
//!
//! Braille (U+2800) gives a 2×4 dot matrix per cell, so a 4-row strip
//! still shows 16 amplitude levels — the reason block glyphs looked
//! chunky is they only give one level per row.
//!
//! PCM from one source at a time. If that source stays silent, viz
//! steps to the next candidate (local monitors first, HTTP last).
//! One ffmpeg (and optional curl). Not a file FFT.

use crate::spectrum::{live_bands, smooth_bands, tick_peaks, BANDS};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

const DOT1: u8 = 0b0000_0001;
const DOT2: u8 = 0b0000_0010;
const DOT3: u8 = 0b0000_0100;
const DOT4: u8 = 0b0000_1000;
const DOT5: u8 = 0b0001_0000;
const DOT6: u8 = 0b0010_0000;
const DOT7: u8 = 0b0100_0000;
const DOT8: u8 = 0b1000_0000;

/// Shared live state between the audio thread and the UI.
struct Live {
    smoothed: [f32; BANDS],
    peaks: [f32; BANDS],
    peak: f32,
    running: bool,
    monitor: String,
    sources: Vec<String>,
    ffmpeg_pid: Option<u32>,
    curl_pid: Option<u32>,
    gen: u64,
}

static LIVE: OnceLock<Mutex<Live>> = OnceLock::new();

fn live() -> &'static Mutex<Live> {
    LIVE.get_or_init(|| {
        Mutex::new(Live {
            smoothed: [0.0; BANDS],
            peaks: [0.0; BANDS],
            peak: 1e-3,
            running: false,
            monitor: String::new(),
            sources: Vec::new(),
            ffmpeg_pid: None,
            curl_pid: None,
            gen: 0,
        })
    })
}

/// Is the live PCM reader running?
pub fn running() -> bool {
    live().lock().unwrap().running
}

fn kill_pid(pid: u32) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Stop the PCM reader. Follow's encoder is a different process — this
/// only kills the viz ffmpeg (and curl when tapping a HEOS URL).
pub fn stop() {
    let (ff, curl) = {
        let mut g = live().lock().unwrap();
        g.gen = g.gen.wrapping_add(1);
        g.running = false;
        g.monitor.clear();
        (g.ffmpeg_pid.take(), g.curl_pid.take())
    };
    if let Some(pid) = ff {
        kill_pid(pid);
    }
    if let Some(pid) = curl {
        kill_pid(pid);
    }
}

/// Hop is loud enough to count as "this source has audio".
pub(crate) fn hop_loud(hop: &[f32]) -> bool {
    hop.iter().any(|x| x.abs() >= 0.01)
}

pub(crate) fn is_network_src(s: &str) -> bool {
    s.contains("://")
}

/// Fail-fast on a dead Pulse monitor; give HTTP a second to connect.
pub(crate) fn probe_hops(network: bool) -> u32 {
    if network { 86 } else { 24 }
}

/// After a lock, wait longer before abandoning (a pause in the song).
pub(crate) fn hold_hops() -> u32 {
    172
}

pub(crate) fn next_source_idx(idx: usize, n: usize) -> usize {
    if n == 0 {
        0
    } else {
        (idx + 1) % n
    }
}

fn dedup_sources(src: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for s in src {
        let t = s.trim();
        if t.is_empty() {
            continue;
        }
        if !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// Start the PCM reader on one source. Same name + alive → no-op.
pub fn start(monitor: &str) -> bool {
    start_hunt(&[monitor.to_string()])
}

/// Walk `sources` until one has PCM. One ffmpeg at a time. HTTP last
/// in the caller's list. After a silent round, pause so we do not
/// respawn curl in a loop.
pub fn start_hunt(sources: &[String]) -> bool {
    let sources = dedup_sources(sources);
    if sources.is_empty() {
        return false;
    }
    {
        let g = live().lock().unwrap();
        if g.running && g.sources == sources {
            return true;
        }
    }
    stop();
    let gen = {
        let mut g = live().lock().unwrap();
        g.running = true;
        g.sources = sources.clone();
        g.monitor = sources[0].clone();
        g.smoothed = [0.0; BANDS];
        g.peaks = [0.0; BANDS];
        g.peak = 1e-3;
        g.gen = g.gen.wrapping_add(1);
        g.gen
    };
    std::thread::spawn(move || hunt_loop(gen, sources));
    true
}

struct Pcm {
    ffmpeg: std::process::Child,
    curl: Option<std::process::Child>,
    stdout: std::process::ChildStdout,
}

fn reap_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn close_pcm(mut pcm: Pcm) {
    reap_child(&mut pcm.ffmpeg);
    if let Some(mut c) = pcm.curl {
        reap_child(&mut c);
    }
}

fn open_pcm(monitor: &str) -> Option<Pcm> {
    let mut curl_child = None;
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-fflags",
        "nobuffer",
        "-flags",
        "low_delay",
    ]);
    if is_network_src(monitor) {
        // Icecast AAC needs more than 32 bytes to probe. Pulse can
        // stay tiny because `-f pulse` already names the format.
        cmd.args([
            "-probesize",
            "262144",
            "-analyzeduration",
            "1000000",
        ]);
        let mut curl = Command::new("curl")
            .args([
                "-sS",
                "-N",
                "-L",
                "--doh-url",
                "https://1.1.1.1/dns-query",
                "--connect-timeout",
                "2",
                monitor,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
            .ok()?;
        match curl.stdout.take() {
            Some(stdout) => {
                cmd.arg("-i").arg("pipe:0").stdin(stdout);
                curl_child = Some(curl);
            }
            None => {
                reap_child(&mut curl);
                return None;
            }
        }
    } else {
        cmd.args([
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-f",
            "pulse",
            "-fragment_size",
            "1920",
            "-i",
            monitor,
        ]);
    }
    let mut child = match cmd
        .args([
            "-flush_packets",
            "1",
            "-ac",
            "1",
            "-ar",
            "22050",
            "-f",
            "f32le",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            if let Some(mut c) = curl_child {
                reap_child(&mut c);
            }
            return None;
        }
    };
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            reap_child(&mut child);
            if let Some(mut c) = curl_child {
                reap_child(&mut c);
            }
            return None;
        }
    };
    Some(Pcm {
        ffmpeg: child,
        curl: curl_child,
        stdout,
    })
}

fn hunt_loop(gen: u64, sources: Vec<String>) {
    use std::io::Read;
    let n = sources.len();
    let mut idx = 0usize;
    loop {
        {
            let g = live().lock().unwrap();
            if g.gen != gen {
                return;
            }
        }
        let src = sources[idx].clone();
        {
            let mut g = live().lock().unwrap();
            if g.gen != gen {
                return;
            }
            g.monitor = src.clone();
        }
        let mut loud = false;
        if let Some(mut pcm) = open_pcm(&src) {
            {
                let mut g = live().lock().unwrap();
                if g.gen != gen {
                    close_pcm(pcm);
                    return;
                }
                g.ffmpeg_pid = Some(pcm.ffmpeg.id());
                g.curl_pid = pcm.curl.as_ref().map(|c| c.id());
            }
            let net = is_network_src(&src);
            let mut silent = 0u32;
            let mut win = vec![0.0f32; crate::spectrum::WINDOW];
            let mut hop_bytes = vec![0u8; crate::spectrum::HOP * 4];
            loop {
                match pcm.stdout.read_exact(&mut hop_bytes) {
                    Ok(()) => {
                        let hop: Vec<f32> = hop_bytes
                            .chunks_exact(4)
                            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                            .collect();
                        slide_window(&mut win, &hop);
                        let mut g = live().lock().unwrap();
                        if g.gen != gen {
                            break;
                        }
                        if hop_loud(&hop) {
                            silent = 0;
                            loud = true;
                        } else {
                            silent = silent.saturating_add(1);
                        }
                        let raw = live_bands(&win, &mut g.peak);
                        let sm = smooth_bands(&g.smoothed, &raw);
                        tick_peaks(&mut g.peaks, &sm);
                        g.smoothed = sm;
                        drop(g);
                        let limit = if loud {
                            hold_hops()
                        } else {
                            probe_hops(net)
                        };
                        if silent >= limit {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            close_pcm(pcm);
        }
        {
            let mut g = live().lock().unwrap();
            if g.gen != gen {
                return;
            }
            g.ffmpeg_pid = None;
            g.curl_pid = None;
        }
        let next = next_source_idx(idx, n);
        if next == 0 && !loud {
            // Full silent round — do not hammer curl. Recheck in a bit.
            for _ in 0..15 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if live().lock().unwrap().gen != gen {
                    return;
                }
            }
        }
        idx = next;
    }
}

/// Slide `hop` new samples into `win`, dropping the oldest. Keeps the
/// window at WINDOW samples so frequency resolution is unchanged while
/// the update rate becomes HOP-sized steps.
fn slide_window(win: &mut [f32], hop: &[f32]) {
    let n = win.len();
    let k = hop.len().min(n);
    win.copy_within(k.., 0);
    win[n - k..].copy_from_slice(&hop[hop.len() - k..]);
}

/// Snapshot the current smoothed bands and peak caps.
pub fn snapshot() -> ([f32; BANDS], [f32; BANDS]) {
    let g = live().lock().unwrap();
    (g.smoothed, g.peaks)
}

/// Pack a boolean dot grid into braille cells.
/// grid[row][col] -> char per 2×4 block. Rows padded to /4, cols to /2.
fn pack_braille(grid: &[Vec<bool>]) -> Vec<String> {
    let rows = grid.len();
    let cols = grid.first().map(|r| r.len()).unwrap_or(0);
    let mut out = Vec::new();
    let cell_rows = (rows + 3) / 4;
    let cell_cols = (cols + 1) / 2;
    for cr in 0..cell_rows {
        let mut line = String::new();
        for cc in 0..cell_cols {
            let get = |r: isize, c: isize| -> bool {
                if r < 0 || c < 0 { return false; }
                grid.get(r as usize).map(|row| row.get(c as usize).copied().unwrap_or(false)).unwrap_or(false)
            };
            let r = (cr * 4) as isize;
            let c = (cc * 2) as isize;
            let mut bits: u8 = 0;
            if get(r,     c    ) { bits |= DOT1; }
            if get(r + 1, c    ) { bits |= DOT2; }
            if get(r + 2, c    ) { bits |= DOT3; }
            if get(r,     c + 1) { bits |= DOT4; }
            if get(r + 1, c + 1) { bits |= DOT5; }
            if get(r + 2, c + 1) { bits |= DOT6; }
            if get(r + 3, c    ) { bits |= DOT7; }
            if get(r + 3, c + 1) { bits |= DOT8; }
            line.push(char::from_u32(0x2800 + bits as u32).unwrap_or(' '));
        }
        out.push(line);
    }
    out
}

/// Horizontal bars: bands left→right, bars grow UP from the bottom.
/// `w_cells` wide (each cell = 2 dot columns), `h_cells` tall (4 dot rows).
pub fn render_horizontal(bands: &[f32; BANDS], peaks: &[f32; BANDS], w_cells: usize, h_cells: usize) -> Vec<String> {
    let dot_rows = h_cells * 4;
    let dot_cols = w_cells * 2;
    let mut grid = vec![vec![false; dot_cols]; dot_rows];
    for c in 0..dot_cols {
        let band = (c as usize * BANDS / dot_cols).min(BANDS - 1);
        let v = bands[band].clamp(0.0, 1.0);
        let p = peaks[band].clamp(0.0, 1.0);
        let fill = ((v * dot_rows as f32).round() as usize).min(dot_rows);
        let cap = ((p * dot_rows as f32).round() as isize).min(dot_rows as isize - 1).max(0) as usize;
        for r in (dot_rows - fill)..dot_rows {
            grid[r][c] = true;
        }
        // peak cap: one dot row at the peak, above the fill
        let cap_row = dot_rows - 1 - cap;
        if cap_row < dot_rows && !grid[cap_row][c] {
            grid[cap_row][c] = true;
        }
    }
    pack_braille(&grid)
}

/// Vertical bars: bands top→bottom, bars grow RIGHT.
/// `w_cells` wide (2 dot cols each), `h_cells` tall (4 dot rows each).
pub fn render_vertical(bands: &[f32; BANDS], peaks: &[f32; BANDS], w_cells: usize, h_cells: usize) -> Vec<String> {
    let dot_rows = h_cells * 4;
    let dot_cols = w_cells * 2;
    let mut grid = vec![vec![false; dot_cols]; dot_rows];
    for r in 0..dot_rows {
        let band = (r as usize * BANDS / dot_rows).min(BANDS - 1);
        let v = bands[band].clamp(0.0, 1.0);
        let p = peaks[band].clamp(0.0, 1.0);
        let fill = ((v * dot_cols as f32).round() as usize).min(dot_cols);
        let cap = ((p * dot_cols as f32).round() as isize).min(dot_cols as isize - 1).max(0) as usize;
        for c in 0..fill {
            grid[r][c] = true;
        }
        let cap_col = cap.min(dot_cols - 1);
        if !grid[r][cap_col] {
            grid[r][cap_col] = true;
        }
    }
    pack_braille(&grid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zeros() -> ([f32; BANDS], [f32; BANDS]) { ([0.0; BANDS], [0.0; BANDS]) }

    #[test]
    fn braille_pack_shape() {
        let grid = vec![vec![false; 4]; 8];
        let out = pack_braille(&grid);
        // 8 rows / 4 = 2 cell-rows; 4 cols / 2 = 2 cell-cols
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].chars().count(), 2);
        // all empty -> braille blank
        assert!(out[0].chars().all(|c| c == '\u{2800}'));
    }

    #[test]
    fn braille_pack_full_is_all_dots() {
        let grid = vec![vec![true; 4]; 8];
        let out = pack_braille(&grid);
        assert_eq!(out[0].chars().next().unwrap(), '\u{28FF}');
    }

    #[test]
    fn horizontal_has_right_dims() {
        let (b, p) = zeros();
        let out = render_horizontal(&b, &p, 20, 4);
        assert_eq!(out.len(), 4, "4 cell-rows");
        assert_eq!(out[0].chars().count(), 20, "20 cell-cols");
    }

    #[test]
    fn vertical_has_right_dims() {
        let (b, p) = zeros();
        let out = render_vertical(&b, &p, 10, 8);
        assert_eq!(out.len(), 8);
        assert_eq!(out[0].chars().count(), 10);
    }

    #[test]
    fn zero_bands_is_blank() {
        let (b, p) = zeros();
        let out = render_horizontal(&b, &p, 8, 3);
        // peak cap draws one dot even at zero — still not a panic
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn full_bands_fill_the_grid() {
        let b = [1.0f32; BANDS];
        let p = [1.0f32; BANDS];
        let out = render_horizontal(&b, &p, 4, 3);
        // every cell should be non-blank (bars fill upward)
        assert!(out.iter().all(|l| l.chars().all(|c| c != '\u{2800}')),
            "full bands must light every dot");
    }

    #[test]
    fn smoothing_prefers_attack_over_release() {
        let prev = [0.0f32; BANDS];
        let mut cur = [0.0f32; BANDS];
        cur[0] = 1.0;
        let up = smooth_bands(&prev, &cur);
        assert!(up[0] > 0.5, "attack should be fast: {}", up[0]);
        let mut dec = [1.0f32; BANDS];
        let cur2 = [0.0f32; BANDS];
        let down = smooth_bands(&dec, &cur2);
        assert!(down[0] > 0.5, "release should be slow: {}", down[0]);
    }

    #[test]
    fn peaks_tick_down() {
        let mut peaks = [0.5f32; BANDS];
        let cur = [0.0f32; BANDS];
        tick_peaks(&mut peaks, &cur);
        assert!(peaks[0] < 0.5, "peak should fall when band drops");
    }

    #[test]
    fn start_retargets_when_monitor_changes() {
        let src = std::fs::read_to_string("src/viz.rs").expect("viz.rs");
        let start = src.find("pub fn start_hunt").expect("start_hunt");
        let end = src[start..]
            .find("\n/// Slide")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(body.contains("stop()"), "new list stops the old ffmpeg");
        assert!(
            body.contains("g.sources == sources"),
            "same hunt list is a no-op"
        );
        assert!(
            body.contains("://"),
            "HEOS radio URL uses ffmpeg -i, not pulse"
        );
        assert!(
            body.contains("doh-url") && body.contains("curl"),
            "station URL goes through curl DoH (system DNS SERVFAILs some hosts)"
        );
        assert!(
            body.contains("nobuffer")
                && body.contains("low_delay")
                && body.contains("probesize")
                && body.contains("fragment_size")
                && body.contains("flush_packets"),
            "viz ffmpeg uses the same low-delay flags as the follow encoder"
        );
        assert!(
            body.contains("262144") && body.contains("1000000"),
            "HTTP probe is large enough to detect Icecast AAC"
        );
        assert!(
            body.contains("\"-N\"") || body.contains("-N"),
            "curl -N so Icecast is not held in stdout buffer"
        );
        assert!(
            src.contains("fn reap_child") && body.contains("close_pcm"),
            "killed ffmpeg/curl are wait()ed so they do not zombie"
        );
        assert!(
            body.contains("open_pcm") && body.contains("idx = next"),
            "one reader walks sources; next after silence"
        );
        assert!(
            body.contains("from_millis(100)"),
            "silent round parks before respawning curl"
        );
        assert!(src.contains("pub fn stop"), "TUI exit / retarget kills viz ffmpeg");
        let stop = src.find("pub fn stop").expect("stop");
        let stop_end = src[stop..]
            .find("\n/// Start")
            .map(|i| stop + i)
            .unwrap_or(src.len());
        let stop_body = &src[stop..stop_end];
        assert!(
            stop_body.contains("kill_pid") || stop_body.contains("kill"),
            "stop must TERM the viz ffmpeg"
        );
        assert!(
            !stop_body.contains("siren-stream"),
            "viz stop leaves the follow encoder"
        );
    }

    #[test]
    fn hop_loud_skips_quiet() {
        assert!(!hop_loud(&[0.0; 8]));
        assert!(!hop_loud(&[0.009, -0.009]));
        assert!(hop_loud(&[0.0, 0.01]));
        assert!(hop_loud(&[-0.02]));
    }

    #[test]
    fn next_source_wraps() {
        assert_eq!(next_source_idx(0, 0), 0);
        assert_eq!(next_source_idx(0, 1), 0);
        assert_eq!(next_source_idx(0, 3), 1);
        assert_eq!(next_source_idx(2, 3), 0);
    }

    #[test]
    fn probe_is_faster_on_pulse_than_http() {
        assert!(probe_hops(false) < probe_hops(true));
        assert!(hold_hops() > probe_hops(true));
        assert!(is_network_src("http://x/s"));
        assert!(is_network_src("https://x/s"));
        assert!(!is_network_src("alsa_output.pci-0000_31_00.4.analog-stereo.monitor"));
    }

    #[test]
    fn dedup_keeps_order() {
        let v = dedup_sources(&[
            "a".into(),
            " a ".into(),
            String::new(),
            "b".into(),
            "a".into(),
        ]);
        assert_eq!(v, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn live_bands_normalizes() {
        let mut peak = 1e-3;
        // a real tone (sine), not DC — flat signal reads as silence
        let pcm: Vec<f32> = (0..crate::spectrum::WINDOW)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / crate::spectrum::SAMPLE_RATE as f32).sin() * 0.5)
            .collect();
        let b = live_bands(&pcm, &mut peak);
        let mx = b.iter().copied().fold(0.0f32, f32::max);
        assert!(mx > 0.9 && mx <= 1.0, "normalized to full scale, got {mx}");
    }
}

#[cfg(test)]
mod latency_tests {
    use super::*;

    /// HOP is the update step: 128 samples @ 22050Hz = ~5.8ms.
    #[test]
    fn hop_gives_fast_updates() {
        assert_eq!(crate::spectrum::HOP, 128);
        let step_ms = crate::spectrum::HOP as f64 * 1000.0 / crate::spectrum::SAMPLE_RATE as f64;
        assert!(step_ms < 8.0, "HOP step should be under 8ms, got {step_ms:.1}ms");
    }

    /// WINDOW still defines resolution: 2048 samples = ~92ms of audio.
    #[test]
    fn window_keeps_resolution() {
        assert_eq!(crate::spectrum::WINDOW, 2048, "window must stay 2048 for frequency resolution");
    }

    /// slide_window keeps the window length and shifts by hop.
    #[test]
    fn slide_window_shifts_and_keeps_len() {
        let mut win = vec![0.0f32; 8];
        let hop = vec![1.0f32, 2.0, 3.0, 4.0];
        slide_window(&mut win, &hop);
        assert_eq!(win.len(), 8);
        // tail holds the newest hop
        assert_eq!(&win[4..], &[1.0, 2.0, 3.0, 4.0]);
        // head is zeros (shifted-in silence)
        assert_eq!(&win[..4], &[0.0, 0.0, 0.0, 0.0]);
    }

    /// Subsequent slides accumulate without corrupting length.
    #[test]
    fn slide_window_multiple_steps() {
        let mut win = vec![0.0f32; 8];
        slide_window(&mut win, &[1.0; 4]);
        slide_window(&mut win, &[2.0; 4]);
        assert_eq!(win.len(), 8);
        assert_eq!(&win[4..], &[2.0; 4]);
        assert_eq!(&win[..4], &[1.0; 4]);
    }
}
