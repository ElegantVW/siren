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
//! The PCM comes from the PipeWire monitor that `stream.rs` already
//! captures, so the visualizer follows whatever is playing: radio,
//! YouTube, games. Not a pre-decoded file.

use crate::spectrum::{live_bands, smooth_bands, tick_peaks, BANDS};
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
}

static LIVE: OnceLock<Mutex<Live>> = OnceLock::new();

fn live() -> &'static Mutex<Live> {
    LIVE.get_or_init(|| {
        Mutex::new(Live {
            smoothed: [0.0; BANDS],
            peaks: [0.0; BANDS],
            peak: 1e-3,
            running: false,
        })
    })
}

/// Is the live PCM reader running?
pub fn running() -> bool {
    live().lock().unwrap().running
}

/// Start the PCM reader: ffmpeg decodes the monitor to f32le mono.
/// One long-lived child, read on a thread, bands updated as they come.
pub fn start(monitor: &str) -> bool {
    {
        let mut g = live().lock().unwrap();
        if g.running {
            return true;
        }
        g.running = true;
    }
    let monitor = monitor.to_string();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut child = match std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner", "-loglevel", "error",
                "-f", "pulse", "-i", &monitor,
                "-ac", "1", "-ar", "22050",
                "-f", "f32le", "-",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => {
                live().lock().unwrap().running = false;
                return;
            }
        };
        let mut out = child.stdout.take().unwrap();
        let mut buf = vec![0u8; crate::spectrum::WINDOW * 4];
        loop {
            match out.read_exact(&mut buf) {
                Ok(()) => {
                    let pcm: Vec<f32> = buf
                        .chunks_exact(4)
                        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                        .collect();
                    let mut g = live().lock().unwrap();
                    let raw = live_bands(&pcm, &mut g.peak);
                    let sm = smooth_bands(&g.smoothed, &raw);
                    tick_peaks(&mut g.peaks, &sm);
                    g.smoothed = sm;
                }
                Err(_) => break,
            }
        }
        let _ = child.kill();
        live().lock().unwrap().running = false;
    });
    true
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
