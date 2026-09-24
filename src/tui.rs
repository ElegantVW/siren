//! Two-box TUI (Option A): top = content view, bottom = focused view's menu.
//! Tab cycles browser → queue → waves → audio. Waves stays on top.
//! Playback stays mpv-over-IPC; audio view + `c` drive the HEOS speaker.

use crate::{config::SirenConfig, heos, library, player, playlist, queue, radio};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq)]
enum View {
    Browser,
    Queue,
    Audio,
    Trove,
    Radio,
}
const VIEWS: [View; 5] = [
    View::Browser,
    View::Queue,
    View::Audio,
    View::Trove,
    View::Radio,
];

impl View {
    fn title(&self) -> &'static str {
        match self {
            View::Browser => "browser",
            View::Queue => "queue",
            View::Audio => "audio",
            View::Trove => "trove",
            View::Radio => "radio",
        }
    }
    fn menu(&self) -> &'static str {
        match self {
            View::Browser => "enter open/play · a add · c cast · backspace up · / filter · S save · L load · R rm",
            View::Queue => "enter play-from · d remove · c clear",
            View::Audio => "o output · s speaker · S shuffle · r repeat · p pause · v refresh · t test · m mute",
            View::Trove => "s search · enter vers · d dl · j/k · m more · a all · f format",
            View::Radio => "enter play · d play · c country · s search · f fav · m more · j/k",
        }
    }
}

/// Pause-aware elapsed clock for speaker playback (firmware has no position API).
struct HeosClock {
    path: Option<PathBuf>,
    start_epoch: f64,
    paused_acc: f64,
    last_tick: Instant,
}

impl HeosClock {
    /// Seconds into the current cast, or None when unknown.
    fn tick(&mut self, playing: Option<bool>) -> Option<f64> {
        let now_ep = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        // re-read the cast record; reset on change
        if let Some((p, ts)) = heos::last_cast() {
            if self.path.as_ref() != Some(&p) || (self.start_epoch - ts).abs() > 0.5 {
                self.path = Some(p);
                self.start_epoch = ts;
                self.paused_acc = 0.0;
            }
        }
        let dt = self.last_tick.elapsed().as_secs_f64();
        self.last_tick = Instant::now();
        match playing {
            Some(false) => self.paused_acc += dt,
            _ => {}
        }
        if self.path.is_none() {
            return None;
        }
        Some((now_ep - self.start_epoch - self.paused_acc).max(0.0))
    }
}

#[derive(Clone)]
struct BrowserEntry {
    path: PathBuf,
    is_dir: bool,
    display: String,
}

#[derive(Clone, Copy, PartialEq)]
enum InputMode {
    Filter,
    SavePl,
    LoadPl,
    RmPl,
    TroveSearch,
    RadioSearch,
}

struct TroveState {
    query: String,
    kind: Option<String>,
    words: Vec<String>,
    docs: Vec<crate::trove::Doc>,
    total: u64,
    sel: usize,
    page: u32,
    searching: bool,
    search_rx: Option<std::sync::mpsc::Receiver<Result<(Vec<crate::trove::Doc>, u64), String>>>,
    loading_more: bool,
    more_rx: Option<std::sync::mpsc::Receiver<Result<(Vec<crate::trove::Doc>, u64), String>>>,
    exhausted: bool,
    dl_active: bool,
    dl_rx: Option<std::sync::mpsc::Receiver<DlMsg>>,
    log: Vec<String>,
    /// file currently showing live progress (replaced in place)
    dl_prog_file: Option<String>,
    /// session format: mp3|flac|ogg|wav|opus|m4a|all
    fmt: String,
    /// doc idx awaiting a format pick + its options
    fmt_for: Option<usize>,
    fmt_opts: Vec<String>,
    fmt_rx: Option<std::sync::mpsc::Receiver<(usize, Vec<String>)>>,
    fmt_pending: bool,
    /// first `a` arms (shows count), second consecutive `a` downloads all
    all_armed: bool,
}

struct AudioState {
    roster: Vec<heos::HeosPlayer>,
    dlna_ok: bool,
    last_poll: Instant,
}

struct RadioState {
    country: Option<String>,
    query: String,
    stations: Vec<radio::Station>,
    sel: usize,
    /// pages fetched (1-based; 0 = none yet)
    page: u32,
    loading: bool,
    rx: Option<std::sync::mpsc::Receiver<Result<Vec<radio::Station>, String>>>,
    loading_more: bool,
    more_rx: Option<std::sync::mpsc::Receiver<Result<Vec<radio::Station>, String>>>,
    exhausted: bool,
    /// country picker open (Enter with no country, or `c`)
    picking: bool,
    countries: Vec<radio::Country>,
    pick_sel: usize,
    countries_loading: bool,
    countries_rx: Option<std::sync::mpsc::Receiver<Result<Vec<radio::Country>, String>>>,
    favs: Vec<radio::Station>,
}

struct App {
    cfg: SirenConfig,
    focus: usize,
    lib_cache: Vec<PathBuf>,
    lib_at: Instant,
    /// directory browser state (empty filter): current dir + entries
    bcwd: PathBuf,
    bentries: Vec<BrowserEntry>,
    browser_sel: usize,
    queue_sel: usize,
    filter: String,
    input: Option<InputMode>,
    input_buf: String,
    msg: String,
    msg_at: Instant,
    audio: AudioState,
    trove: TroveState,
    radio: RadioState,
    spk_rx: Option<std::sync::mpsc::Receiver<(Vec<heos::HeosPlayer>, bool)>>,
    spk_pending: bool,
    spk_at: Instant,
    spk_force: bool,
    heos_clock: HeosClock,
    heos_elapsed: Option<f64>,
    mpv_label: String,
    mpv_pos: f64,
    mpv_dur: f64,
    mpv_paused: bool,
    mpv_vol: i64,
}

impl App {
    fn new() -> Self {
        let cfg = SirenConfig::load();
        queue::ensure_loaded();
        let lib = library::scan_library(&cfg);
        let first_root = library::library_roots(&cfg).into_iter().next().unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into()))
        });
        let mut app = Self {
            cfg,
            focus: 0,
            lib_cache: lib,
            lib_at: Instant::now(),
            bcwd: first_root,
            bentries: Vec::new(),
            browser_sel: 0,
            queue_sel: 0,
            filter: String::new(),
            input: None,
            input_buf: String::new(),
            msg: String::new(),
            msg_at: Instant::now(),
            audio: AudioState {
                roster: Vec::new(),
                dlna_ok: false,
                last_poll: Instant::now() - Duration::from_secs(99),
            },
            trove: TroveState {
                query: String::new(),
                kind: None,
                words: Vec::new(),
                docs: Vec::new(),
                total: 0,
                sel: 0,
                page: 0,
                searching: false,
                search_rx: None,
                loading_more: false,
                more_rx: None,
                exhausted: false,
                dl_active: false,
                dl_rx: None,
                log: Vec::new(),
                dl_prog_file: None,
                fmt: "mp3".into(),
                fmt_for: None,
                fmt_opts: Vec::new(),
                fmt_rx: None,
                fmt_pending: false,
                all_armed: false,
            },
            radio: RadioState {
                country: radio::last_country(),
                query: String::new(),
                stations: Vec::new(),
                sel: 0,
                page: 0,
                loading: false,
                rx: None,
                loading_more: false,
                more_rx: None,
                exhausted: false,
                picking: false,
                countries: Vec::new(),
                pick_sel: 0,
                countries_loading: false,
                countries_rx: None,
                favs: radio::favs(),
            },
            spk_rx: None,
            spk_pending: false,
            spk_at: Instant::now() - Duration::from_secs(99),
            spk_force: true,
            heos_clock: HeosClock {
                path: None,
                start_epoch: 0.0,
                paused_acc: 0.0,
                last_tick: Instant::now(),
            },
            heos_elapsed: None,
            mpv_label: String::new(),
            mpv_pos: 0.0,
            mpv_dur: 0.0,
            mpv_paused: false,
            mpv_vol: 75,
        };
        app.rebuild_browser();
        app
    }

    /// Rebuild directory entries for `bcwd` (dirs first, then audio files).
    fn rebuild_browser(&mut self) {
        let mut dirs: Vec<BrowserEntry> = Vec::new();
        let mut files: Vec<BrowserEntry> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.bcwd) {
            let mut names: Vec<PathBuf> =
                rd.filter_map(|e| e.ok().map(|x| x.path())).collect();
            names.sort();
            for p in names {
                let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                if name.starts_with('.') {
                    continue;
                }
                if p.is_dir() {
                    dirs.push(BrowserEntry {
                        path: p,
                        is_dir: true,
                        display: format!("{name}/"),
                    });
                } else if p.is_file() {
                    let lo = p.to_string_lossy().to_lowercase();
                    if crate::library::AUDIO_EXT.iter().any(|e| lo.ends_with(e)) {
                        crate::meta::warm(&p);
                        let display = crate::meta::display(&p.to_string_lossy());
                        files.push(BrowserEntry {
                            path: p,
                            is_dir: false,
                            display,
                        });
                    }
                }
            }
        }
        dirs.extend(files);
        self.bentries = dirs;
        // clamp selection into the new list
        if !self.bentries.is_empty() {
            self.browser_sel = self.browser_sel.min(self.bentries.len() - 1);
        } else {
            self.browser_sel = 0;
        }
    }

    /// Cached library (rescan at most every 5s — never per frame).
    fn lib(&mut self) -> &[PathBuf] {
        if self.lib_at.elapsed() >= Duration::from_secs(5) {
            self.lib_cache = library::scan_library(&self.cfg);
            self.lib_at = Instant::now();
        }
        &self.lib_cache
    }

    fn view(&self) -> View {
        VIEWS[self.focus]
    }
    fn is_heos(&self) -> bool {
        self.cfg.audio_output == "heos"
    }
    fn say(&mut self, m: impl Into<String>) {
        self.msg = m.into();
        self.msg_at = Instant::now();
    }

    /// Files for search mode; in dir mode use `bentries` (includes dirs).
    fn filtered(&mut self) -> Vec<PathBuf> {
        if self.filter.trim().is_empty() {
            return self
                .bentries
                .iter()
                .filter(|e| !e.is_dir)
                .map(|e| e.path.clone())
                .collect();
        }
        // resolve re-scans internally; acceptable on filter keystrokes only
        library::resolve_library(&self.cfg, &self.filter)
    }

    /// Short printable cwd (~/Music/trove/…).
    fn cwd_display(&self) -> String {
        let home = std::env::var("HOME").unwrap_or_default();
        let s = self.bcwd.to_string_lossy().into_owned();
        if !home.is_empty() {
            if let Some(rest) = s.strip_prefix(&home) {
                return format!("~{rest}");
            }
        }
        s
    }

    fn poll_mpv(&mut self) {
        if !player::alive() {
            self.mpv_label.clear();
            self.mpv_pos = 0.0;
            self.mpv_dur = 0.0;
            self.mpv_paused = false;
            return;
        }
        self.mpv_label = queue::now_label();
        self.mpv_pos = player::get_f64("time-pos");
        self.mpv_dur = player::get_f64("duration");
        self.mpv_paused = player::get_bool("pause", false);
        self.mpv_vol = player::get("volume").and_then(|v| v.as_i64()).unwrap_or(self.cfg.default_volume as i64);
    }

    /// Speaker poll runs in a background thread — never blocks draw/input.
    /// Cadence 10s, or forced (view enter, `s`/`v` keys).
    fn poll_speaker(&mut self) {
        // harvest finished poll
        if self.spk_pending {
            if let Some(rx) = self.spk_rx.as_ref() {
                match rx.try_recv() {
                    Ok((roster, dlna_ok)) => {
                        self.audio.roster = roster;
                        self.audio.dlna_ok = dlna_ok;
                        self.spk_pending = false;
                        self.spk_rx = None;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.spk_pending = false;
                        self.spk_rx = None;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
            } else {
                self.spk_pending = false;
            }
        }
        let stale = self.spk_at.elapsed() >= Duration::from_secs(10);
        if !self.spk_pending && (stale || self.spk_force) {
            self.spk_force = false;
            self.spk_at = Instant::now();
            let (tx, rx) = std::sync::mpsc::channel();
            self.spk_rx = Some(rx);
            self.spk_pending = true;
            std::thread::spawn(move || {
                let mut roster = heos::roster();
                heos::enrich(&mut roster);
                let dlna_ok = std::net::TcpStream::connect_timeout(
                    &"192.168.8.186:8200".parse().unwrap(),
                    Duration::from_millis(400),
                )
                .is_ok();
                let _ = tx.send((roster, dlna_ok));
            });
        }
    }

    fn force_speaker_poll(&mut self) {
        self.spk_force = true;
    }

    fn speaker_target(&mut self) -> Option<(String, i64, String)> {
        let hint = self.cfg.audio_speaker.clone();
        heos::resolve(Some(&hint)).map(|(ip, pid, p)| (ip, pid, p.name))
    }

    fn adjust_volume(&mut self, delta: i64) {
        if self.is_heos() {
            if let Some((ip, pid, name)) = self.speaker_target() {
                let cur = heos::rpc(&ip, &[format!("heos://player/get_volume?pid={pid}")]);
                let mut lvl = 20;
                for o in &cur {
                    let msg = o.get("heos").and_then(|h| h.get("message")).and_then(|m| m.as_str()).unwrap_or("");
                    for kv in msg.split('&') {
                        if let Some((k, v)) = kv.split_once('=') {
                            if k == "level" {
                                if let Ok(n) = v.parse() {
                                    lvl = n;
                                }
                            }
                        }
                    }
                }
                let next = (lvl + delta as i32).clamp(0, 100);
                if heos::set_volume(&ip, pid, next) {
                    patch_roster(self, None, Some(next)); // optimistic
                    self.say(format!("{name} volume → {next}%"));
                } else {
                    self.say("volume failed");
                }
            } else {
                self.say("no speaker found");
            }
        } else {
            let next = (self.mpv_vol + delta).clamp(0, 150);
            player::send(&serde_json::json!(["set_property", "volume", next]));
            self.mpv_vol = next;
            self.say(format!("volume → {next}%"));
        }
    }

    fn cast_path(&mut self, path: &PathBuf) {
        let target = match self.speaker_target() {
            Some(t) => t,
            None => {
                self.say("no speaker found — try v (refresh)");
                return;
            }
        };
        let (ip, pid, name) = target;
        self.say(format!("casting {} → {name}…", path_name(path)));
        match heos::dlna_cast(&ip, pid, path, 1) {
            Ok(()) => {
                heos::note_cast(path);
                // reset the elapsed clock + optimistic play state
                self.heos_clock.path = Some(path.clone());
                self.heos_clock.start_epoch = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                self.heos_clock.paused_acc = 0.0;
                patch_roster(self, Some("play"), None);
                self.say(format!("▶ {} on {name}", path_name(path)))
            }
            Err(e) => self.say(format!("cast failed: {e}")),
        }
    }

    fn browser_selected(&mut self) -> Option<PathBuf> {
        if self.filter.trim().is_empty() {
            // dir mode: dirs navigate, only files select
            return self
                .bentries
                .get(self.browser_sel)
                .filter(|e| !e.is_dir)
                .map(|e| e.path.clone());
        }
        self.filtered().get(self.browser_sel).cloned()
    }
}

fn path_name(p: &PathBuf) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

fn stem_name(p: &PathBuf) -> String {
    // tags (cached, never blocks) with stem fallback
    crate::meta::display(&p.to_string_lossy())
}

pub fn run() -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    // mouse wheel + click (Python parity, gated on mouse=true)
    let mouse_on = SirenConfig::load().mouse;
    if mouse_on {
        let _ = execute!(stdout, EnableMouseCapture);
    }
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let mut app = App::new();
    app.poll_mpv();
    let res = event_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
    if mouse_on {
        let _ = execute!(terminal.backend_mut(), DisableMouseCapture);
    }
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    res
}

fn event_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> anyhow::Result<()> {
    // draw on input + 2Hz tick (was: every 100ms regardless)
    let mut last_tick = Instant::now() - Duration::from_secs(99);
    terminal.draw(|f| draw(f, app))?;
    loop {
        if event::poll(Duration::from_millis(500))? {
            match event::read()? {
                Event::Key(key) => {
                    if handle_key(app, key.code, key.modifiers) {
                        return Ok(());
                    }
                    terminal.draw(|f| draw(f, app))?;
                    last_tick = Instant::now();
                }
                Event::Mouse(me) => {
                    if handle_mouse(app, me) {
                        terminal.draw(|f| draw(f, app))?;
                        last_tick = Instant::now();
                    }
                }
                _ => {}
            }
        } else if last_tick.elapsed() >= Duration::from_millis(500) {
            last_tick = Instant::now();
            app.poll_mpv();
            app.poll_speaker();
            // speaker playing-state for the elapsed clock (roster may lag;
            // actions patch it optimistically, so this stays truthful)
            let playing = speaker_entry(app).and_then(|p| p.state).map(|s| s == "play");
            app.heos_elapsed = app.heos_clock.tick(playing);
            poll_trove(app);
            poll_radio(app);
            terminal.draw(|f| draw(f, app))?;
        } else {
            // harvest background results without a full redraw
            app.poll_speaker();
            poll_trove(app);
            poll_radio(app);
        }
    }
}

/// True = quit.
fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) -> bool {
    // input mode eats everything except Esc/Enter
    if let Some(mode) = app.input {
        match code {
            KeyCode::Esc => {
                app.input = None;
                app.input_buf.clear();
            }
            KeyCode::Enter => {
                let val = std::mem::take(&mut app.input_buf);
                app.input = None;
                apply_input(app, mode, &val);
            }
            KeyCode::Backspace => {
                app.input_buf.pop();
            }
            KeyCode::Char(c) => {
                if mods.contains(KeyModifiers::CONTROL) && (c == 'u' || c == 'U') {
                    app.input_buf.clear();
                } else {
                    app.input_buf.push(c);
                }
            }
            _ => {}
        }
        return false;
    }

    match code {
        // global transport (all views; waves tab is gone)
        KeyCode::Char('n') => {
            transport_next(app);
            return false;
        }
        KeyCode::Char('b') => {
            transport_prev(app);
            return false;
        }
        KeyCode::Char('+') | KeyCode::Char('=') => {
            app.adjust_volume(5);
            return false;
        }
        KeyCode::Char('-') | KeyCode::Char('_') => {
            app.adjust_volume(-5);
            return false;
        }
        KeyCode::Tab => {
            app.focus = (app.focus + 1) % VIEWS.len();
            if app.view() == View::Audio {
                app.force_speaker_poll();
            }
            if app.view() == View::Radio {
                app.radio.favs = radio::favs();
                if app.radio.stations.is_empty()
                    && !app.radio.loading
                    && (app.radio.country.is_some() || !app.radio.query.trim().is_empty())
                {
                    radio_fetch(app, 1);
                }
            }
        }
        KeyCode::BackTab => {
            app.focus = (app.focus + VIEWS.len() - 1) % VIEWS.len();
        }
        KeyCode::Char('q') => return true,
        KeyCode::Esc => {
            // trove format row eats Esc first (cancel row, don't quit)
            if app.view() == View::Trove && app.trove.fmt_for.is_some() {
                app.trove.fmt_for = None;
                app.trove.fmt_opts.clear();
                return false;
            }
            // radio country picker eats Esc first (cancel picker, don't quit)
            if app.view() == View::Radio && app.radio.picking {
                app.radio.picking = false;
                return false;
            }
            // browser filter: Esc clears search, back to directory
            if app.view() == View::Browser && !app.filter.is_empty() {
                app.filter.clear();
                app.browser_sel = 0;
                return false;
            }
            return true;
        }
        _ => match app.view() {
            View::Browser => handle_browser(app, code, mods),
            View::Queue => handle_queue(app, code, mods),
            View::Audio => handle_audio(app, code, mods),
            View::Trove => handle_trove(app, code, mods),
            View::Radio => handle_radio(app, code, mods),
        },
    }
    false
}

/// Mouse: wheel scrolls the focused list; click selects, double-click acts.
/// Returns true when the view changed (needs redraw).
fn handle_mouse(app: &mut App, me: crossterm::event::MouseEvent) -> bool {
    if !app.cfg.mouse {
        return false;
    }
    match me.kind {
        MouseEventKind::ScrollUp => {
            scroll_by(app, -1);
            true
        }
        MouseEventKind::ScrollDown => {
            scroll_by(app, 1);
            true
        }
        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
            click_at(app, me.row)
        }
        _ => false,
    }
}

fn scroll_by(app: &mut App, delta: isize) {
    match app.view() {
        View::Browser => {
            let n = browser_row_count(app);
            if n == 0 {
                return;
            }
            let s = app.browser_sel as isize + delta;
            app.browser_sel = s.clamp(0, n as isize - 1) as usize;
        }
        View::Queue => {
            let n = queue::snapshot().len();
            if n == 0 {
                return;
            }
            let s = app.queue_sel as isize + delta;
            app.queue_sel = s.clamp(0, n as isize - 1) as usize;
        }
        View::Trove => {
            let n = app.trove.docs.len();
            if n == 0 {
                return;
            }
            let s = app.trove.sel as isize + delta;
            app.trove.sel = s.clamp(0, n as isize - 1) as usize;
        }
        View::Radio => {
            if app.radio.picking {
                let n = app.radio.countries.len();
                if n == 0 {
                    return;
                }
                let s = app.radio.pick_sel as isize + delta;
                app.radio.pick_sel = s.clamp(0, n as isize - 1) as usize;
                return;
            }
            let n = radio_len(app);
            if n == 0 {
                return;
            }
            let s = app.radio.sel as isize + delta;
            app.radio.sel = s.clamp(0, n as isize - 1) as usize;
        }
        _ => {}
    }
}

static LAST_CLICK: std::sync::LazyLock<std::sync::Mutex<(Instant, u16, u8)>> =
    std::sync::LazyLock::new(|| {
        std::sync::Mutex::new((Instant::now() - Duration::from_secs(99), u16::MAX, u8::MAX))
    });

/// Map a terminal row to a list index (content starts at row 1).
/// Double-click (same row <500ms) activates like Enter.
fn click_at(app: &mut App, row: u16) -> bool {
    let h = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24);
    if row < 1 || row >= h.saturating_sub(11) {
        return false; // waves strip / menu — not lists
    }
    let now = Instant::now();
    let view_no = app.focus as u8;
    let mut last = LAST_CLICK.lock().unwrap();
    let double = last.1 == row && last.2 == view_no && now.duration_since(last.0) < Duration::from_millis(500);
    *last = (now, row, view_no);
    drop(last);
    match app.view() {
        View::Browser => {
            let n = browser_row_count(app);
            let idx = (row as usize).saturating_sub(1);
            if idx < n {
                app.browser_sel = idx;
                if double {
                    browser_activate(app);
                }
                return true;
            }
        }
        View::Queue => {
            let n = queue::snapshot().len();
            let idx = (row as usize).saturating_sub(1);
            if idx < n {
                app.queue_sel = idx;
                if double {
                    play_queue_output(app, idx);
                }
                return true;
            }
        }
        View::Trove => {
            // 2 rows per doc + optional title row
            let title_off = if app.trove.query.is_empty() && app.trove.docs.is_empty() { 0 } else { 1 };
            let rel = (row as usize).saturating_sub(1);
            if rel >= title_off {
                let idx = (rel - title_off) / 2;
                if idx < app.trove.docs.len() {
                    app.trove.sel = idx;
                    if double {
                        trove_ask_format(app, idx);
                    }
                    return true;
                }
            }
        }
        View::Radio => {
            // 1 row per entry, no title row
            let idx = (row as usize).saturating_sub(1);
            if app.radio.picking {
                if idx < app.radio.countries.len() {
                    app.radio.pick_sel = idx;
                    if double {
                        if let Some(c) = app.radio.countries.get(idx).cloned() {
                            app.radio.picking = false;
                            app.radio.country = Some(c.name.clone());
                            app.radio.query.clear();
                            app.radio.stations.clear();
                            app.radio.sel = 0;
                            app.radio.page = 0;
                            app.radio.exhausted = false;
                            radio::set_country(&c.name);
                            app.say(format!("radio → {}", c.name));
                            radio_fetch(app, 1);
                        }
                    }
                    return true;
                }
                return false;
            }
            if idx < radio_len(app) {
                app.radio.sel = idx;
                if double {
                    if let Some((st, _)) = radio_row(app, idx) {
                        radio_play(app, &st);
                    }
                }
                return true;
            }
        }
        _ => {}
    }
    false
}

fn apply_input(app: &mut App, mode: InputMode, val: &str) {
    match mode {
        InputMode::Filter => {
            app.filter = val.to_string();
            app.browser_sel = 0;
        }
        InputMode::SavePl => {
            if val.trim().is_empty() {
                app.say("need a name");
                return;
            }
            let items = queue::snapshot();
            if playlist::save(val.trim(), &items) {
                app.say(format!("Saved playlist: {}", val.trim()));
            } else {
                app.say("save failed");
            }
        }
        InputMode::LoadPl => {
            let name = playlist::find(val.trim()).unwrap_or_else(|| val.trim().to_string());
            let tracks = playlist::load(&name);
            if tracks.is_empty() {
                app.say(format!("Playlist not found: {}", val.trim()));
            } else {
                queue::replace(tracks);
                match crate::output::play_from(&app.cfg, 0) {
                    Ok(_) => {
                        if app.is_heos() {
                            patch_roster(app, Some("play"), None);
                        }
                        app.say(format!("Playing playlist: {name}"));
                    }
                    Err(e) => app.say(e),
                }
            }
        }
        InputMode::RmPl => {
            if playlist::delete(val.trim()) {
                app.say(format!("Deleted playlist: {}", val.trim()));
            } else {
                app.say(format!("Playlist not found: {}", val.trim()));
            }
        }
        InputMode::RadioSearch => {
            let q = val.trim().to_string();
            app.radio.query = q;
            app.radio.stations.clear();
            app.radio.sel = 0;
            app.radio.page = 0;
            app.radio.exhausted = false;
            app.radio.favs = radio::favs();
            if app.radio.query.is_empty() {
                // empty search → back to the country list
                if app.radio.country.is_some() {
                    radio_fetch(app, 1);
                }
                return;
            }
            radio_fetch(app, 1);
        }
        InputMode::TroveSearch => {
            let q = val.trim().to_string();
            if q.is_empty() {
                return;
            }
            let mut words: Vec<String> =
                q.split_whitespace().map(|s| s.to_string()).collect();
            let mut kind: Option<String> = None;
            if !words.is_empty() && crate::trove::is_kind_token(&words[0]) {
                kind = Some(words.remove(0));
            }
            app.trove.query = q.clone();
            app.trove.kind = kind.clone();
            app.trove.words = words.clone();
            app.trove.docs.clear();
            app.trove.sel = 0;
            app.trove.page = 0;
            app.trove.exhausted = false;
            app.trove.loading_more = false;
            app.trove.more_rx = None;
            app.trove.all_armed = false;
            app.trove.fmt_for = None;
            app.trove.fmt_opts.clear();
            app.trove.fmt_pending = false;
            app.trove.fmt_rx = None;
            app.trove.searching = true;
            let (tx, rx) = std::sync::mpsc::channel();
            app.trove.search_rx = Some(rx);
            std::thread::spawn(move || {
                let res = crate::trove::search_page(
                    kind.as_deref(),
                    &words,
                    crate::trove::PAGE_SIZE,
                    1,
                );
                let _ = tx.send(res);
            });
        }
    }
}

fn trove_page_short(got: usize, total: u64) -> bool {
    if got < crate::trove::PAGE_SIZE as usize {
        return true;
    }
    total > 0 && got as u64 >= total
}

fn trove_maybe_prefetch(app: &mut App) {
    if app.trove.searching || app.trove.loading_more || app.trove.exhausted {
        return;
    }
    if app.trove.docs.is_empty() || app.trove.query.is_empty() {
        return;
    }
    let n = app.trove.docs.len();
    if n > 0 && app.trove.total > 0 && n as u64 >= app.trove.total {
        app.trove.exhausted = true;
        return;
    }
    if app.trove.sel + crate::trove::PREFETCH_WITHIN < n {
        return;
    }
    trove_fetch_more(app);
}

fn trove_fetch_more(app: &mut App) {
    if app.trove.searching || app.trove.loading_more || app.trove.exhausted {
        return;
    }
    if app.trove.query.is_empty() {
        return;
    }
    let kind = app.trove.kind.clone();
    let words = app.trove.words.clone();
    let page = app.trove.page + 1;
    app.trove.loading_more = true;
    let (tx, rx) = std::sync::mpsc::channel();
    app.trove.more_rx = Some(rx);
    std::thread::spawn(move || {
        let res = crate::trove::search_page(
            kind.as_deref(),
            &words,
            crate::trove::PAGE_SIZE,
            page,
        );
        let _ = tx.send(res);
    });
}

/// Harvest finished trove search/download threads (non-blocking).
fn poll_trove(app: &mut App) {
    if app.trove.searching {
        if let Some(rx) = app.trove.search_rx.as_ref() {
            match rx.try_recv() {
                Ok(Ok((docs, total))) => {
                    app.trove.docs = docs;
                    app.trove.total = total;
                    app.trove.sel = 0;
                    app.trove.page = 1;
                    app.trove.all_armed = false;
                    app.trove.searching = false;
                    app.trove.search_rx = None;
                    app.trove.exhausted = trove_page_short(app.trove.docs.len(), total);
                    app.say(format!(
                        "{} hits ({} total)",
                        app.trove.docs.len(),
                        app.trove.total
                    ));
                }
                Ok(Err(e)) => {
                    app.trove.searching = false;
                    app.trove.search_rx = None;
                    app.say(format!("search failed: {e}"));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    app.trove.searching = false;
                    app.trove.search_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        } else {
            app.trove.searching = false;
        }
    }
    if app.trove.loading_more {
        if let Some(rx) = app.trove.more_rx.as_ref() {
            match rx.try_recv() {
                Ok(Ok((chunk, total))) => {
                    let added = chunk.len();
                    if total > 0 {
                        app.trove.total = total;
                    }
                    crate::trove::append_unique(&mut app.trove.docs, chunk);
                    app.trove.page = app.trove.page.saturating_add(1);
                    app.trove.loading_more = false;
                    app.trove.more_rx = None;
                    if added < crate::trove::PAGE_SIZE as usize
                        || (app.trove.total > 0 && app.trove.docs.len() as u64 >= app.trove.total)
                    {
                        app.trove.exhausted = true;
                    }
                    if added > 0 {
                        app.say(format!(
                            "{} shown ({} total)",
                            app.trove.docs.len(),
                            app.trove.total
                        ));
                    }
                }
                Ok(Err(e)) => {
                    app.trove.loading_more = false;
                    app.trove.more_rx = None;
                    app.trove.exhausted = true;
                    app.say(format!("more failed: {e}"));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    app.trove.loading_more = false;
                    app.trove.more_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        } else {
            app.trove.loading_more = false;
        }
    }
    trove_maybe_prefetch(app);
    if app.trove.dl_active {
        if let Some(rx) = app.trove.dl_rx.as_ref() {
            loop {
                match rx.try_recv() {
                    Ok(DlMsg::Progress { file, i, n, frac }) => {
                        let pct = match frac {
                            Some(f) => format!("{:>3.0}%", f * 100.0),
                            None => "  ?".into(),
                        };
                        let line = format!("↓ {i}/{n} {file}  {pct}");
                        // same file → replace live line in place; else push
                        if app.trove.dl_prog_file.as_deref() == Some(file.as_str()) {
                            app.trove.log.pop();
                        } else {
                            app.trove.dl_prog_file = Some(file);
                        }
                        app.trove.log.push(line);
                        while app.trove.log.len() > 6 {
                            app.trove.log.remove(0);
                        }
                    }
                    Ok(DlMsg::Line(l)) => {
                        // completion line supersedes its live line
                        app.trove.dl_prog_file = None;
                        app.trove.log.push(l);
                        while app.trove.log.len() > 6 {
                            app.trove.log.remove(0);
                        }
                    }
                    Ok(DlMsg::Finished) => {
                        app.trove.dl_active = false;
                        app.trove.dl_rx = None;
                        app.trove.dl_prog_file = None;
                        app.say("download finished — see trove log");
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        app.trove.dl_active = false;
                        app.trove.dl_rx = None;
                        app.trove.dl_prog_file = None;
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                }
            }
        } else {
            app.trove.dl_active = false;
        }
    }
    if app.trove.fmt_pending {
        if let Some(rx) = app.trove.fmt_rx.as_ref() {
            match rx.try_recv() {
                Ok((idx, opts)) => {
                    app.trove.fmt_for = Some(idx);
                    app.trove.fmt_opts = opts;
                    app.trove.fmt_pending = false;
                    app.trove.fmt_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    app.trove.fmt_pending = false;
                    app.trove.fmt_rx = None;
                    app.trove.fmt_for = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        } else {
            app.trove.fmt_pending = false;
        }
    }
}

/// Download-thread → TUI messages.
enum DlMsg {
    /// live per-file progress (replaces the previous progress line)
    Progress { file: String, i: usize, n: usize, frac: Option<f64> },
    /// finished line (completion / failure / done summary)
    Line(String),
    /// worker done
    Finished,
}

fn trove_download(app: &mut App, idxs: Vec<usize>, format: Option<String>) {
    if app.trove.dl_active {
        app.say("download already running");
        return;
    }
    let docs: Vec<crate::trove::Doc> = idxs
        .into_iter()
        .filter_map(|i| app.trove.docs.get(i).cloned())
        .collect();
    if docs.is_empty() {
        return;
    }
    let fmt = format.or_else(|| {
        if app.trove.fmt == "all" {
            None
        } else {
            Some(app.trove.fmt.clone())
        }
    });
    app.trove.dl_active = true;
    app.trove.log.push(format!(
        "↓ {} item(s) [{}]…",
        docs.len(),
        fmt.as_deref().unwrap_or("all")
    ));
    let (tx, rx) = std::sync::mpsc::channel();
    app.trove.dl_rx = Some(rx);
    app.trove.dl_prog_file = None;
    std::thread::spawn(move || {
        for d in &docs {
            let tx2 = tx.clone();
            let ident = d.identifier.clone();
            let mut cb = move |i: usize, n: usize, name: &str, good: bool| {
                let _ = tx2.send(DlMsg::Line(format!(
                    "↓ {i}/{n} {}{}",
                    name,
                    if good { "" } else { " FAILED" }
                )));
            };
            let tx3 = tx.clone();
            let mut live = move |i: usize, n: usize, name: &str, frac: Option<f64>| {
                let _ = tx3.send(DlMsg::Progress {
                    file: name.to_string(),
                    i,
                    n,
                    frac,
                });
            };
            match crate::trove::do_download_inner(
                &d.identifier,
                &d.mediatype,
                true,
                fmt.as_deref(),
                Some(&mut cb as &mut dyn FnMut(usize, usize, &str, bool)),
                Some(&mut live
                    as &mut dyn FnMut(usize, usize, &str, Option<f64>)),
            ) {
                Ok((ok, n, dir)) => {
                    let _ = tx.send(DlMsg::Line(format!(
                        "Done {ok}/{n} {} → {}",
                        ident,
                        dir.display()
                    )));
                }
                Err(e) => {
                    let _ = tx.send(DlMsg::Line(format!("{ident}: {e}")));
                }
            }
        }
        let _ = tx.send(DlMsg::Finished);
    });
}

/// Ask what versions a doc offers (background metadata fetch).
fn trove_ask_format(app: &mut App, idx: usize) {
    let d = match app.trove.docs.get(idx).cloned() {
        Some(d) => d,
        None => return,
    };
    app.trove.fmt_for = Some(idx);
    app.trove.fmt_opts.clear();
    app.trove.fmt_pending = true;
    let (tx, rx) = std::sync::mpsc::channel();
    app.trove.fmt_rx = Some(rx);
    std::thread::spawn(move || {
        let mut opts = crate::trove::available_formats(&d.identifier, &d.mediatype).unwrap_or_default();
        opts.push("all".into());
        let _ = tx.send((idx, opts));
    });
}

const SESSION_FMTS: &[&str] = &["mp3", "flac", "ogg", "wav", "opus", "m4a", "all"];

fn handle_trove(app: &mut App, code: KeyCode, _mods: KeyModifiers) {
    let n = app.trove.docs.len();
    // version picker is modal: digits pick a version, Esc cancels,
    // everything else is swallowed so 1-9 never double as download keys
    if app.trove.fmt_for.is_some() {
        match code {
            KeyCode::Esc => {
                app.trove.fmt_for = None;
                app.trove.fmt_opts.clear();
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                let k = c.to_digit(10).unwrap() as usize;
                if k >= 1 && k <= app.trove.fmt_opts.len() {
                    let f = app.trove.fmt_opts[k - 1].clone();
                    let idx = app.trove.fmt_for.take().unwrap();
                    app.trove.fmt_opts.clear();
                    trove_download(app, vec![idx], Some(f));
                }
            }
            _ => {}
        }
        return;
    }
    // second consecutive `a` confirms download-all; anything else disarms
    if !matches!(code, KeyCode::Char('a')) {
        app.trove.all_armed = false;
    }
    match code {
        KeyCode::Char('j') | KeyCode::Down => {
            if n > 0 {
                app.trove.sel = (app.trove.sel + 1).min(n - 1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.trove.sel = app.trove.sel.saturating_sub(1);
        }
        KeyCode::Char('s') => {
            app.input = Some(InputMode::TroveSearch);
            app.input_buf.clear();
        }
        KeyCode::Char('m') => {
            if app.trove.exhausted {
                app.say("end of results");
            } else {
                trove_fetch_more(app);
            }
        }
        KeyCode::Enter => {
            if app.trove.sel < n {
                trove_ask_format(app, app.trove.sel);
            }
        }
        // instant download of the cursor row with the session format
        KeyCode::Char('d') => {
            if app.trove.sel < n {
                trove_download(app, vec![app.trove.sel], None);
            }
        }
        KeyCode::Char('a') => {
            if n == 0 {
                return;
            }
            if app.trove.all_armed {
                app.trove.all_armed = false;
                trove_download(app, (0..n).collect(), None);
            } else {
                app.trove.all_armed = true;
                app.say(format!(
                    "press a again to download {n} items [{}]",
                    app.trove.fmt
                ));
            }
        }
        KeyCode::Char('f') => {
            let cur = SESSION_FMTS.iter().position(|f| *f == app.trove.fmt).unwrap_or(0);
            app.trove.fmt = SESSION_FMTS[(cur + 1) % SESSION_FMTS.len()].to_string();
            app.say(format!("trove format → {}", app.trove.fmt));
        }
        _ => {}
    }
}

/// Radio rows: favorites pinned on top (★), then browse results with
/// fav dupes removed. One shared list so cursor, click and draw agree.
fn radio_visible(app: &App) -> Vec<(radio::Station, bool)> {
    let mut out: Vec<(radio::Station, bool)> = Vec::new();
    for s in &app.radio.favs {
        out.push((s.clone(), true));
    }
    for s in &app.radio.stations {
        if app.radio.favs.iter().any(|x| {
            (!x.uuid.is_empty() && x.uuid == s.uuid) || x.url == s.url
        }) {
            continue;
        }
        out.push((s.clone(), false));
    }
    out
}

fn radio_len(app: &App) -> usize {
    radio_visible(app).len()
}

fn radio_row(app: &App, idx: usize) -> Option<(radio::Station, bool)> {
    radio_visible(app).into_iter().nth(idx)
}

/// Fetch page `page` (1-based) for the current country/query in background.
fn radio_fetch(app: &mut App, page: u32) {
    if app.radio.loading || app.radio.loading_more {
        return;
    }
    let country = app.radio.country.clone();
    let query = app.radio.query.clone();
    if query.trim().is_empty() && country.is_none() {
        return;
    }
    let offset = ((page.max(1) - 1) as usize) * radio::PAGE_SIZE;
    if page <= 1 {
        app.radio.loading = true;
        let (tx, rx) = std::sync::mpsc::channel();
        app.radio.rx = Some(rx);
        std::thread::spawn(move || {
            let res = if query.trim().is_empty() {
                radio::by_country(&country.unwrap_or_default(), radio::PAGE_SIZE, offset)
            } else {
                radio::search(&query, radio::PAGE_SIZE, offset)
            };
            let _ = tx.send(res);
        });
    } else {
        app.radio.loading_more = true;
        let (tx, rx) = std::sync::mpsc::channel();
        app.radio.more_rx = Some(rx);
        std::thread::spawn(move || {
            let res = if query.trim().is_empty() {
                radio::by_country(&country.unwrap_or_default(), radio::PAGE_SIZE, offset)
            } else {
                radio::search(&query, radio::PAGE_SIZE, offset)
            };
            let _ = tx.send(res);
        });
    }
}

fn radio_open_picker(app: &mut App) {
    app.radio.picking = true;
    app.radio.pick_sel = 0;
    if !app.radio.countries.is_empty() || app.radio.countries_loading {
        return;
    }
    app.radio.countries_loading = true;
    let (tx, rx) = std::sync::mpsc::channel();
    app.radio.countries_rx = Some(rx);
    std::thread::spawn(move || {
        let _ = tx.send(radio::countries());
    });
}

/// Stage a station on the queue and play it through the one output.
fn radio_play(app: &mut App, st: &radio::Station) {
    queue::replace(vec![radio::to_queue_item(st)]);
    match crate::output::play_from(&app.cfg, 0) {
        Ok(m) => {
            if app.is_heos() {
                patch_roster(app, Some("play"), None);
            }
            app.say(format!("{} · {m}", st.name));
        }
        Err(e) => app.say(e),
    }
}

/// Harvest finished radio threads + prefetch near the end (non-blocking).
fn poll_radio(app: &mut App) {
    if app.radio.countries_loading {
        if let Some(rx) = app.radio.countries_rx.as_ref() {
            match rx.try_recv() {
                Ok(Ok(list)) => {
                    app.radio.countries = list;
                    app.radio.countries_loading = false;
                    app.radio.countries_rx = None;
                }
                Ok(Err(e)) => {
                    app.radio.countries_loading = false;
                    app.radio.countries_rx = None;
                    app.say(format!("countries failed: {e}"));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    app.radio.countries_loading = false;
                    app.radio.countries_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        } else {
            app.radio.countries_loading = false;
        }
    }
    if app.radio.loading {
        if let Some(rx) = app.radio.rx.as_ref() {
            match rx.try_recv() {
                Ok(Ok(chunk)) => {
                    app.radio.exhausted = chunk.len() < radio::PAGE_SIZE;
                    app.radio.stations = chunk;
                    app.radio.sel = 0;
                    app.radio.page = 1;
                    app.radio.loading = false;
                    app.radio.rx = None;
                }
                Ok(Err(e)) => {
                    app.radio.loading = false;
                    app.radio.rx = None;
                    app.say(format!("radio failed: {e}"));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    app.radio.loading = false;
                    app.radio.rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        } else {
            app.radio.loading = false;
        }
    }
    if app.radio.loading_more {
        if let Some(rx) = app.radio.more_rx.as_ref() {
            match rx.try_recv() {
                Ok(Ok(chunk)) => {
                    let short = chunk.len() < radio::PAGE_SIZE;
                    let mut added = 0;
                    for s in chunk {
                        if !app.radio.stations.iter().any(|x| {
                            (!x.uuid.is_empty() && x.uuid == s.uuid) || x.url == s.url
                        }) {
                            app.radio.stations.push(s);
                            added += 1;
                        }
                    }
                    app.radio.page = app.radio.page.saturating_add(1);
                    app.radio.loading_more = false;
                    app.radio.more_rx = None;
                    if short || added == 0 {
                        app.radio.exhausted = true;
                    }
                }
                Ok(Err(e)) => {
                    app.radio.loading_more = false;
                    app.radio.more_rx = None;
                    app.radio.exhausted = true;
                    app.say(format!("more failed: {e}"));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    app.radio.loading_more = false;
                    app.radio.more_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        } else {
            app.radio.loading_more = false;
        }
    }
    // prefetch before the cursor hits the end (favs pin the top;
    // only browse results page)
    if !app.radio.loading
        && !app.radio.loading_more
        && !app.radio.exhausted
        && !app.radio.picking
        && !app.radio.stations.is_empty()
    {
        let n = radio_len(app);
        if app.radio.sel + radio::PREFETCH_WITHIN >= n {
            radio_fetch(app, app.radio.page + 1);
        }
    }
}

fn handle_radio(app: &mut App, code: KeyCode, _mods: KeyModifiers) {
    // country picker is modal: Enter picks, Esc cancels
    if app.radio.picking {
        match code {
            KeyCode::Esc => {
                app.radio.picking = false;
                return;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !app.radio.countries.is_empty() {
                    app.radio.pick_sel =
                        (app.radio.pick_sel + 1).min(app.radio.countries.len() - 1);
                }
                return;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                app.radio.pick_sel = app.radio.pick_sel.saturating_sub(1);
                return;
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(c) = app.radio.countries.get(app.radio.pick_sel).cloned() {
                    app.radio.picking = false;
                    app.radio.country = Some(c.name.clone());
                    app.radio.query.clear();
                    app.radio.stations.clear();
                    app.radio.sel = 0;
                    app.radio.page = 0;
                    app.radio.exhausted = false;
                    radio::set_country(&c.name);
                    app.say(format!("radio → {}", c.name));
                    radio_fetch(app, 1);
                }
                return;
            }
            _ => return,
        }
    }
    let n = radio_len(app);
    match code {
        KeyCode::Char('j') | KeyCode::Down => {
            if n > 0 {
                app.radio.sel = (app.radio.sel + 1).min(n - 1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.radio.sel = app.radio.sel.saturating_sub(1);
        }
        KeyCode::Enter | KeyCode::Char('d') | KeyCode::Char(' ') => {
            if app.radio.country.is_none() && app.radio.query.trim().is_empty() {
                radio_open_picker(app);
                return;
            }
            if let Some((st, _)) = radio_row(app, app.radio.sel) {
                radio_play(app, &st);
            }
        }
        KeyCode::Char('c') => radio_open_picker(app),
        KeyCode::Char('s') => {
            app.input = Some(InputMode::RadioSearch);
            app.input_buf.clear();
        }
        KeyCode::Char('f') => {
            if let Some((st, _)) = radio_row(app, app.radio.sel) {
                if radio::toggle_fav(&st) {
                    app.say(format!("fav: {}", st.name));
                } else {
                    app.say(format!("unfaved: {}", st.name));
                }
                app.radio.favs = radio::favs();
            }
        }
        KeyCode::Char('m') => {
            if app.radio.exhausted {
                app.say("end of results");
            } else {
                radio_fetch(app, app.radio.page + 1);
            }
        }
        _ => {}
    }
}

/// Enter / double-click: open dir, or play the selected file.
fn browser_activate(app: &mut App) {
    let searching = !app.filter.trim().is_empty();
    if searching {
        if let Some(p) = app.browser_selected() {
            if app.is_heos() {
                app.cast_path(&p);
            } else {
                let s = p.to_string_lossy().into_owned();
                queue::start_playlist(&app.cfg, &[s], false);
            }
        }
        return;
    }
    match app.bentries.get(app.browser_sel).cloned() {
        Some(e) if e.is_dir => {
            app.bcwd = e.path;
            app.browser_sel = 0;
            app.rebuild_browser();
        }
        Some(e) => {
            if app.is_heos() {
                app.cast_path(&e.path);
            } else {
                let s = e.path.to_string_lossy().into_owned();
                queue::start_playlist(&app.cfg, &[s], false);
            }
        }
        None => {}
    }
}

/// Rows in the browser view (search hits or cwd entries).
fn browser_row_count(app: &mut App) -> usize {
    if app.filter.trim().is_empty() {
        app.bentries.len()
    } else {
        app.filtered().len()
    }
}

/// Queue one file in the TUI session queue.
fn tui_queue_add(app: &mut App, p: &PathBuf) {
    let s = p.to_string_lossy().into_owned();
    let mut v = queue::snapshot();
    let (artist, title) = crate::meta::tags_for(&s);
    v.push(queue::QueueItem {
        path: s.clone(),
        display: stem_name(p),
        title,
        artist,
        duration: 0.0,
    });
    queue::replace(v);
    app.say(format!("queued {}", path_name(p)));
}

/// Queue every audio file under a directory (recursive, sorted).
fn tui_queue_add_dir(app: &mut App, dir: &PathBuf) {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut stack = vec![dir.clone()];
    while let Some(d) = stack.pop() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&d)
            .map(|rd| rd.filter_map(|e| e.ok().map(|x| x.path())).collect())
            .unwrap_or_default();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                stack.push(p);
            } else if p.is_file() {
                let lo = p.to_string_lossy().to_lowercase();
                if crate::library::AUDIO_EXT.iter().any(|e| lo.ends_with(e)) {
                    files.push(p);
                }
            }
        }
    }
    files.sort();
    let mut v = queue::snapshot();
    for p in &files {
        let s = p.to_string_lossy().into_owned();
        if v.iter().any(|it| it.path == s) {
            continue;
        }
        let (artist, title) = crate::meta::tags_for(&s);
        v.push(queue::QueueItem {
            path: s,
            display: stem_name(p),
            title,
            artist,
            duration: 0.0,
        });
    }
    let n = files.len();
    queue::replace(v);
    app.say(format!("queued {n} track(s) from {}", path_name(dir)));
}

fn handle_browser(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    let searching = !app.filter.trim().is_empty();
    let n = browser_row_count(app);
    match code {
        KeyCode::Char('j') | KeyCode::Down => {
            if n > 0 {
                app.browser_sel = (app.browser_sel + 1).min(n - 1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.browser_sel = app.browser_sel.saturating_sub(1);
        }
        KeyCode::Backspace => {
            if searching {
                return;
            }
            // up one level (stop at filesystem root)
            if let Some(parent) = app.bcwd.parent() {
                if parent != app.bcwd {
                    app.bcwd = parent.to_path_buf();
                    app.browser_sel = 0;
                    app.rebuild_browser();
                }
            }
        }
        KeyCode::Enter | KeyCode::Char('p') | KeyCode::Char(' ') => {
            browser_activate(app);
        }
        KeyCode::Char('o') => {
            if let Some(p) = app.browser_selected() {
                let s = p.to_string_lossy().into_owned();
                queue::start_playlist(&app.cfg, &[s], false);
            }
        }
        KeyCode::Char('a') => {
            if searching {
                if let Some(p) = app.browser_selected() {
                    tui_queue_add(app, &p);
                }
                return;
            }
            match app.bentries.get(app.browser_sel).cloned() {
                Some(e) if e.is_dir => tui_queue_add_dir(app, &e.path),
                Some(e) => tui_queue_add(app, &e.path),
                None => {}
            }
        }
        KeyCode::Char('c') => {
            if let Some(p) = app.browser_selected() {
                app.cast_path(&p);
            }
        }
        KeyCode::Char('/') => {
            app.input = Some(InputMode::Filter);
            app.input_buf.clear();
        }
        KeyCode::Char('S') => {
            app.input = Some(InputMode::SavePl);
            app.input_buf.clear();
        }
        KeyCode::Char('L') => {
            app.input = Some(InputMode::LoadPl);
            app.input_buf.clear();
        }
        KeyCode::Char('R') => {
            app.input = Some(InputMode::RmPl);
            app.input_buf.clear();
        }
        _ => {
            let _ = mods;
        }
    }
}

fn handle_queue(app: &mut App, code: KeyCode, _mods: KeyModifiers) {
    let n = queue::snapshot().len();
    match code {
        KeyCode::Char('j') | KeyCode::Down => {
            if n > 0 {
                app.queue_sel = (app.queue_sel + 1).min(n - 1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.queue_sel = app.queue_sel.saturating_sub(1);
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            let sel = app.queue_sel;
            play_queue_output(app, sel);
        }
        KeyCode::Char('d') => {
            let items = queue::snapshot();
            if let Some(it) = items.get(app.queue_sel) {
                let d = if it.display.is_empty() { stem_name(&PathBuf::from(&it.path)) } else { it.display.clone() };
                let mut v = items;
                if app.queue_sel < v.len() {
                    v.remove(app.queue_sel);
                }
                queue::replace(v);
                queue::maybe_resync_queue();
                app.say(format!("Removed: {d}"));
                app.queue_sel = app.queue_sel.saturating_sub(1);
            }
        }
        KeyCode::Char('c') => {
            queue::replace(Vec::new());
            queue::maybe_resync_queue();
            app.say("Queue cleared.");
        }
        KeyCode::Char('S') => {
            app.input = Some(InputMode::SavePl);
            app.input_buf.clear();
        }
        KeyCode::Char('L') => {
            app.input = Some(InputMode::LoadPl);
            app.input_buf.clear();
        }
        KeyCode::Char('R') => {
            app.input = Some(InputMode::RmPl);
            app.input_buf.clear();
        }
        _ => {}
    }
}

fn mpv_toggle_pause(app: &mut App) {
    if app.is_heos() {
        if let Some((ip, pid, name)) = app.speaker_target() {
            let to = heos::toggle(&ip, pid);
            patch_roster(app, Some(to), None); // optimistic: show now
            app.say(format!("{name} → {to}"));
        } else {
            app.say("no speaker found");
        }
    } else {
        player::send(&serde_json::json!(["cycle", "pause"]));
    }
}

/// Transport following `audio_output` (shared by global keys).
fn transport_next(app: &mut App) {
    if app.is_heos() {
        if let Some((ip, pid, _)) = app.speaker_target() {
            heos::play_next(&ip, pid);
        }
    } else {
        queue::cmd_next();
    }
}

fn transport_prev(app: &mut App) {
    if app.is_heos() {
        if let Some((ip, pid, _)) = app.speaker_target() {
            heos::play_previous(&ip, pid);
        }
    } else {
        queue::cmd_prev();
    }
}

fn toggle_shuffle(app: &mut App) {
    let cur = player::get_bool("shuffle", false);
    player::send(&serde_json::json!(["set_property", "shuffle", !cur]));
    app.say(format!("shuffle {}", if !cur { "on" } else { "off" }));
}

fn cycle_repeat(app: &mut App) {
    // off → all → track → off
    let lp = player::get("loop-playlist").and_then(|v| v.as_str().map(|s| s.to_string()));
    let lf = player::get("loop-file").and_then(|v| v.as_str().map(|s| s.to_string()));
    let cur = if matches!(lp.as_deref(), Some("inf") | Some("yes")) {
        "all"
    } else if matches!(lf.as_deref(), Some("inf") | Some("yes")) {
        "track"
    } else {
        "off"
    };
    let nxt = match cur {
        "off" => "all",
        "all" => "track",
        _ => "off",
    };
    player::send(&serde_json::json!(["set_property", "loop-playlist", if nxt == "all" { "inf" } else { "no" }]));
    player::send(&serde_json::json!(["set_property", "loop-file", if nxt == "track" { "inf" } else { "no" }]));
    app.say(format!("repeat {nxt}"));
}

/// Play queue from `index` through the one output channel.
fn play_queue_output(app: &mut App, index: usize) {
    match crate::output::play_from(&app.cfg, index) {
        Ok(m) => {
            if app.is_heos() {
                patch_roster(app, Some("play"), None);
            }
            app.say(m);
        }
        Err(e) => app.say(e),
    }
}

/// Optimistic roster patch: show the commanded state NOW, poll confirms.
fn patch_roster(app: &mut App, state: Option<&str>, vol: Option<i32>) {
    let hint = app.cfg.audio_speaker.to_lowercase();
    for p in app.audio.roster.iter_mut() {
        if p.name.to_lowercase().contains(&hint) {
            if let Some(s) = state {
                p.state = Some(s.to_string());
            }
            if let Some(v) = vol {
                p.volume = Some(v);
            }
            return;
        }
    }
}

fn handle_audio(app: &mut App, code: KeyCode, _mods: KeyModifiers) {
    match code {
        KeyCode::Char('p') | KeyCode::Char(' ') => mpv_toggle_pause(app),
        KeyCode::Char('S') => toggle_shuffle(app),
        KeyCode::Char('r') => cycle_repeat(app),
        KeyCode::Char('o') => {
            app.cfg.audio_output = if app.is_heos() { "local".into() } else { "heos".into() };
            let _ = app.cfg.save();
            app.say(format!("output → {}", app.cfg.audio_output));
        }
        KeyCode::Char('s') => {
            app.force_speaker_poll();
            app.poll_speaker();
            if app.audio.roster.is_empty() {
                app.say("no speakers found");
                return;
            }
            let cur = app.audio.roster.iter().position(|p| {
                p.name.to_lowercase().contains(&app.cfg.audio_speaker.to_lowercase())
            }).unwrap_or(0);
            let next = &app.audio.roster[(cur + 1) % app.audio.roster.len()];
            app.cfg.audio_speaker = next.name.clone();
            let _ = app.cfg.save();
            app.say(format!("speaker → {}", next.name));
        }
        KeyCode::Char('v') => {
            app.force_speaker_poll();
            app.poll_speaker();
            app.say(format!("{} speaker(s), dlna {}", app.audio.roster.len(), if app.audio.dlna_ok { "ok" } else { "down" }));
        }
        KeyCode::Char('t') => {
            // test cast: browser selection, else queue top, else now-playing, else lib top
            let pick = app.browser_selected().or_else(|| {
                queue::snapshot().first().map(|it| PathBuf::from(&it.path))
            }).or_else(|| {
                let p = player::now_path();
                if p.is_empty() { None } else { Some(PathBuf::from(p)) }
            }).or_else(|| app.lib().first().cloned());
            match pick {
                Some(p) => app.cast_path(&p),
                None => app.say("nothing to cast"),
            }
        }
        KeyCode::Char('m') => {
            if let Some((ip, pid, name)) = app.speaker_target() {
                // read mute, flip
                let mut muted = false;
                for o in heos::rpc(&ip, &[format!("heos://player/get_mute?pid={pid}")]) {
                    let msg = o.get("heos").and_then(|h| h.get("message")).and_then(|m| m.as_str()).unwrap_or("");
                    if msg.contains("state=on") {
                        muted = true;
                    }
                }
                if heos::set_mute(&ip, pid, !muted) {
                    app.say(format!("{name} mute → {}", if !muted { "on" } else { "off" }));
                    app.force_speaker_poll();
                }
            } else {
                app.say("no speaker found");
            }
        }
        _ => {}
    }
}

// ---- drawing ----

fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(4), // persistent waves strip
            Constraint::Length(7),
        ])
        .split(f.area());

    // top box
    let focus = app.view();
    let out_tag = if app.is_heos() { "heos" } else { "local" };
    let top_title = format!(" ✦ siren · {} · out:{out_tag} ✦ ", focus.title());
    let top_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(top_title)
        .border_style(Style::default().fg(Color::Magenta));
    let inner = top_block.inner(chunks[0]);
    f.render_widget(top_block, chunks[0]);
    match focus {
        View::Browser => draw_browser(f, app, inner),
        View::Queue => draw_queue(f, app, inner),
        View::Audio => draw_audio(f, app, inner),
        View::Trove => draw_trove(f, app, inner),
        View::Radio => draw_radio(f, app, inner),
    }

    // persistent waves strip — visible at all times, all views
    let wave_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" ✦ waves ✦ ")
        .border_style(Style::default().fg(Color::DarkGray));
    let wave_inner = wave_block.inner(chunks[1]);
    f.render_widget(wave_block, chunks[1]);
    f.render_widget(Paragraph::new(waves_lines(app)), wave_inner);

    // bottom box: context menu (or input)
    let (menu_title, menu_lines) = if let Some(mode) = app.input {
        let prompt = match mode {
            InputMode::Filter => "filter",
            InputMode::SavePl => "save playlist as",
            InputMode::LoadPl => "load playlist",
            InputMode::RmPl => "remove playlist",
            InputMode::TroveSearch => "trove search (music|live|ccmixter + words)",
            InputMode::RadioSearch => "radio search (station name)",
        };
        (
            format!(" ✦ {prompt} ✦ "),
            vec![Line::from(vec![
                Span::raw("  "),
                Span::styled(app.input_buf.clone() + "█", Style::default().fg(Color::White)),
            ])],
        )
    } else {
        let mut lines = vec![Line::from(Span::styled(
            format!("  {}", focus.menu()),
            Style::default().fg(Color::Gray),
        ))];
        if !app.msg.is_empty() && app.msg_at.elapsed() < Duration::from_secs(6) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(app.msg.clone(), Style::default().fg(Color::Cyan)),
            ]));
        } else {
            let hint = match focus {
                View::Browser => {
                    if app.filter.trim().is_empty() {
                        format!("{} · {} entries · tab cycle · q quit", app.cwd_display(), app.bentries.len())
                    } else {
                        format!("{} tracks · tab cycle · q quit", app.filtered().len())
                    }
                }
                View::Queue => format!("{} queued · tab cycle · q quit", queue::snapshot().len()),
                View::Audio => format!("out:{} · spk:{} · tab cycle · q quit", app.cfg.audio_output, app.cfg.audio_speaker),
                View::Trove => {
                    let st = if app.trove.searching {
                        "searching…".to_string()
                    } else if app.trove.loading_more {
                        format!("{}+… / {}", app.trove.docs.len(), app.trove.total)
                    } else if app.trove.dl_active {
                        "downloading…".to_string()
                    } else if app.trove.docs.is_empty() {
                        "s to search".to_string()
                    } else if app.trove.exhausted {
                        format!("{} / {} · end", app.trove.docs.len(), app.trove.total)
                    } else {
                        format!("{} / {}", app.trove.docs.len(), app.trove.total)
                    };
                    format!("{st} · fmt:{} · tab cycle · q quit", app.trove.fmt)
                }
                View::Radio => {
                    let where_ = if app.radio.query.trim().is_empty() {
                        app.radio.country.clone().unwrap_or_else(|| "no country".into())
                    } else {
                        format!("search: {}", app.radio.query)
                    };
                    let st = if app.radio.loading {
                        "loading…".to_string()
                    } else if app.radio.loading_more {
                        format!("{}+…", radio_len(app))
                    } else if radio_len(app) == 0 {
                        "enter picks a country".to_string()
                    } else if app.radio.exhausted {
                        format!("{} · end", radio_len(app))
                    } else {
                        format!("{}", radio_len(app))
                    };
                    format!("{where_} · {st} · favs:{} · tab cycle · q quit", app.radio.favs.len())
                }
            };
            lines.push(Line::from(Span::styled(format!("  {hint}"), Style::default().fg(Color::DarkGray))));
        }
        (format!(" ✦ {} menu ✦ ", focus.title()), lines)
    };
    let menu_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(menu_title)
        .border_style(Style::default().fg(Color::DarkGray));
    let menu_inner = menu_block.inner(chunks[2]);
    f.render_widget(menu_block, chunks[2]);
    f.render_widget(Paragraph::new(menu_lines), menu_inner);
}

fn draw_browser(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    // search mode: flat fuzzy hits. dir mode: cwd entries (dirs + files).
    let rows: Vec<ListItem> = if app.filter.trim().is_empty() {
        app.bentries
            .iter()
            .map(|e| {
                let (mark, style) = if e.is_dir {
                    ("▸ ", Style::default().fg(Color::Cyan))
                } else {
                    ("  ", Style::default())
                };
                let label = if e.is_dir {
                    e.display.clone()
                } else {
                    crate::meta::display(&e.path.to_string_lossy())
                };
                ListItem::new(Line::from(vec![
                    Span::raw(mark),
                    Span::styled(label, style),
                ]))
            })
            .collect()
    } else {
        let items = app.filtered();
        items
            .iter()
            .map(|p| ListItem::new(Line::from(Span::raw(format!("  {}", stem_name(p))))))
            .collect()
    };
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(app.browser_sel.min(rows.len() - 1)));
    }
    let list = List::new(rows)
        .highlight_style(Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))
        .highlight_symbol("❯ ");
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_queue(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let items = queue::snapshot();
    let rows: Vec<ListItem> = items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            // stored labels win (streams keep station names); files re-probe
            let d = if it.display.is_empty() {
                crate::meta::display(&it.path)
            } else {
                it.display.clone()
            };
            let mark = if i == 0 { "▶ " } else { "  " };
            ListItem::new(Line::from(Span::raw(format!("{mark}{:3}. {d}", i + 1))))
        })
        .collect();
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(app.queue_sel.min(rows.len() - 1)));
    }
    let list = List::new(if rows.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  (queue empty)",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        rows
    })
    .highlight_style(Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))
    .highlight_symbol("❯ ");
    f.render_stateful_widget(list, area, &mut state);
}

/// Waves body — shared by the persistent strip and the waves view.
/// Follows `audio_output`: speaker state when heos, mpv when local.
/// Speaker entry matching the configured speaker (or first seen).
fn speaker_entry(app: &App) -> Option<heos::HeosPlayer> {
    app.audio
        .roster
        .iter()
        .find(|p| p.name.to_lowercase().contains(&app.cfg.audio_speaker.to_lowercase()))
        .or_else(|| app.audio.roster.first())
        .cloned()
}

/// Real 64-bar EQ row for a track at `secs` ("analyzing…" until decoded).
/// Live streams have no local file to decode — say so instead of spinning.
fn eq_row(app: &App, path: &PathBuf, secs: f64) -> String {
    if crate::meta::is_url(&path.to_string_lossy()) {
        return "· live stream ·".into();
    }
    crate::spectrum::request_analyze(path);
    match crate::spectrum::spectrum_for(path) {
        Some(spec) => spec.at(secs).iter().map(|v| crate::spectrum::bar_glyph(*v)).collect(),
        None => "analyzing…".into(),
    }
}

/// Waves strip body — always visible. Real spectrum (mpv time-pos local,
/// pause-aware elapsed clock on speaker); label follows `audio_output`.
fn waves_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if app.is_heos() {
        match speaker_entry(app) {
            Some(p) => {
                let playing = p.state.as_deref() == Some("play");
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        if playing { "▶ " } else { "❚❚ " },
                        Style::default().fg(Color::Green),
                    ),
                    Span::raw(format!(
                        "{} — {}{}",
                        p.name,
                        p.state.as_deref().unwrap_or("?"),
                        p.volume.map(|v| format!("  {v}%")).unwrap_or_default()
                    )),
                ]));
                let bars = match (&app.heos_clock.path, app.heos_elapsed) {
                    (Some(path), Some(secs)) => eq_row(app, path, secs),
                    _ => "analyzing…".into(),
                };
                lines.push(Line::from(Span::raw(format!("  {bars}"))));
            }
            None => {
                lines.push(Line::from(Span::raw(format!("  heos → {}", app.cfg.audio_speaker))));
                lines.push(Line::from(Span::styled(
                    "  no speaker seen — v in audio view",
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        return lines;
    }
    if app.mpv_label.is_empty() && !player::alive() {
        lines.push(Line::from(Span::styled(
            "  idle — no mpv",
            Style::default().fg(Color::DarkGray),
        )));
        return lines;
    }
    let label = if app.mpv_label.is_empty() {
        "— silence —".to_string()
    } else {
        app.mpv_label.clone()
    };
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            if app.mpv_paused { "❚❚ " } else { "▶ " },
            Style::default().fg(Color::Green),
        ),
        Span::raw(label),
    ]));
    let p = player::now_path();
    let bars = if p.is_empty() { "—".into() } else { eq_row(app, &PathBuf::from(&p), app.mpv_pos) };
    lines.push(Line::from(Span::raw(format!("  {bars}"))));
    lines
}

fn draw_trove(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let mut rows: Vec<ListItem> = Vec::new();
    if app.trove.searching {
        rows.push(ListItem::new(Line::from(Span::styled(
            "  searching archive.org…",
            Style::default().fg(Color::DarkGray),
        ))));
    } else if app.trove.docs.is_empty() {
        rows.push(ListItem::new(Line::from(Span::styled(
            if app.trove.query.is_empty() {
                "  press s to search free & legal music"
            } else {
                "  no results — s to search again"
            },
            Style::default().fg(Color::DarkGray),
        ))));
    }
    for (i, d) in app.trove.docs.iter().enumerate() {
        let (l1, l2) = crate::trove::doc_lines(i + 1, d);
        rows.push(ListItem::new(Line::from(Span::raw(format!("  {l1}")))));
        let _ = l2;
        rows.push(ListItem::new(Line::from(Span::styled(
            format!("      {l2}"),
            Style::default().fg(Color::DarkGray),
        ))));
    }
    if app.trove.loading_more {
        rows.push(ListItem::new(Line::from(Span::styled(
            "  loading more…",
            Style::default().fg(Color::DarkGray),
        ))));
    } else if app.trove.exhausted && !app.trove.docs.is_empty() {
        rows.push(ListItem::new(Line::from(Span::styled(
            "  — end —",
            Style::default().fg(Color::DarkGray),
        ))));
    }
    if let Some(idx) = app.trove.fmt_for {
        if let Some(d) = app.trove.docs.get(idx) {
            let opts: Vec<String> = app
                .trove
                .fmt_opts
                .iter()
                .enumerate()
                .map(|(i, o)| format!("{} {o}", i + 1))
                .collect();
            let what = if opts.is_empty() {
                "reading versions…".to_string()
            } else {
                opts.join(" · ")
            };
            rows.push(ListItem::new(Line::from(vec![
                Span::raw("  version? "),
                Span::styled(
                    format!("{} — {what}", d.identifier),
                    Style::default().fg(Color::Yellow),
                ),
            ])));
        }
    }
    // chronological tail: oldest first, newest at the bottom
    let tail: Vec<&String> = app.trove.log.iter().rev().take(3).collect();
    for l in tail.into_iter().rev() {
        rows.push(ListItem::new(Line::from(Span::styled(
            format!("  {l}"),
            Style::default().fg(Color::Cyan),
        ))));
    }
    // selection tracks result rows (2 rows per doc)
    let mut state = ListState::default();
    if !app.trove.docs.is_empty() {
        let row = 1 + (app.trove.sel.min(app.trove.docs.len() - 1)) * 2;
        state.select(Some(row.min(rows.len().saturating_sub(1))));
    }
    let title = if app.trove.query.is_empty() {
        String::new()
    } else {
        let extra = if app.trove.loading_more {
            " · loading more…"
        } else if app.trove.exhausted {
            " · end"
        } else {
            ""
        };
        format!(
            "  {} / {} for “{}”{extra}",
            app.trove.docs.len(),
            app.trove.total,
            app.trove.query
        )
    };
    if !title.is_empty() {
        rows.insert(
            0,
            ListItem::new(Line::from(Span::styled(title, Style::default().fg(Color::Gray)))),
        );
    }
    let list = List::new(rows)
        .highlight_style(Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))
        .highlight_symbol("❯ ");
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_radio(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let mut rows: Vec<ListItem> = Vec::new();
    if app.radio.picking {
        // modal country list, alphabetical
        if app.radio.countries_loading && app.radio.countries.is_empty() {
            rows.push(ListItem::new(Line::from(Span::styled(
                "  reading countries…",
                Style::default().fg(Color::DarkGray),
            ))));
        }
        for c in &app.radio.countries {
            rows.push(ListItem::new(Line::from(Span::raw(format!(
                "  {} ({})",
                c.name, c.count
            )))));
        }
        let mut state = ListState::default();
        if !rows.is_empty() {
            state.select(Some(app.radio.pick_sel.min(rows.len() - 1)));
        }
        let list = List::new(rows)
            .highlight_style(Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))
            .highlight_symbol("❯ ");
        f.render_stateful_widget(list, area, &mut state);
        return;
    }
    if app.radio.loading && app.radio.stations.is_empty() && app.radio.favs.is_empty() {
        rows.push(ListItem::new(Line::from(Span::styled(
            "  tuning…",
            Style::default().fg(Color::DarkGray),
        ))));
    }
    if radio_len(app) == 0 && !app.radio.loading {
        rows.push(ListItem::new(Line::from(Span::styled(
            "  press enter to pick a country",
            Style::default().fg(Color::DarkGray),
        ))));
    }
    for (s, fav) in radio_visible(app) {
        if fav {
            rows.push(ListItem::new(Line::from(vec![
                Span::raw("★ "),
                Span::styled(s.name.clone(), Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!("  [{}]", s.country),
                    Style::default().fg(Color::DarkGray),
                ),
            ])));
            continue;
        }
        let bit = if s.bitrate > 0 {
            format!(" {}k", s.bitrate)
        } else {
            String::new()
        };
        rows.push(ListItem::new(Line::from(vec![
            Span::raw("  "),
            Span::raw(format!("{}{}  [{}]", s.name, bit, s.codec)),
        ])));
    }
    if app.radio.loading_more {
        rows.push(ListItem::new(Line::from(Span::styled(
            "  loading more…",
            Style::default().fg(Color::DarkGray),
        ))));
    } else if app.radio.exhausted && radio_len(app) > 0 {
        rows.push(ListItem::new(Line::from(Span::styled(
            "  — end —",
            Style::default().fg(Color::DarkGray),
        ))));
    }
    let mut state = ListState::default();
    if radio_len(app) > 0 {
        state.select(Some(app.radio.sel.min(rows.len().saturating_sub(1))));
    }
    let list = List::new(rows)
        .highlight_style(Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))
        .highlight_symbol("❯ ");
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_audio(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::raw("  output   "),
        Span::styled(
            app.cfg.audio_output.clone(),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ),
        Span::styled("   (o toggle)", Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::from(vec![
        Span::raw("  speaker  "),
        Span::styled(
            app.cfg.audio_speaker.clone(),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled("   (s cycle · v refresh)", Style::default().fg(Color::DarkGray)),
    ]));
    if app.audio.roster.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no speakers seen — press v",
            Style::default().fg(Color::DarkGray),
        )));
    }
    for p in &app.audio.roster {
        let playing = p.state.as_deref() == Some("play");
        lines.push(Line::from(vec![
            Span::raw(format!("  {} ", if playing { "▶" } else { "·" })),
            Span::styled(
                p.name.clone(),
                Style::default().fg(if playing { Color::Green } else { Color::Gray }),
            ),
            Span::raw(format!(
                "  {}  {}{}",
                p.model,
                p.ip,
                p.volume.map(|v| format!("  {v}%")).unwrap_or_default()
            )),
        ]));
    }
    lines.push(Line::from(Span::styled(
        format!(
            "  dlna {}   (t test-cast · m mute · +/- vol)",
            if app.audio.dlna_ok { "ok" } else { "down" }
        ),
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(Paragraph::new(lines), area);
}
