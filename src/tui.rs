//! Two-box TUI (Option A): top = content view, bottom = focused view's menu.
//! Tab cycles browser → queue → waves → audio. Waves stays on top.
//! Playback stays mpv-over-IPC; audio view + `c` drive the HEOS speaker.

use crate::{config::SirenConfig, heos, library, player, playlist, queue};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph},
    Frame,
};
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq)]
enum View {
    Browser,
    Queue,
    Waves,
    Audio,
}
const VIEWS: [View; 4] = [View::Browser, View::Queue, View::Waves, View::Audio];

impl View {
    fn title(&self) -> &'static str {
        match self {
            View::Browser => "browser",
            View::Queue => "queue",
            View::Waves => "waves",
            View::Audio => "audio",
        }
    }
    fn menu(&self) -> &'static str {
        match self {
            View::Browser => "enter play · a add · c cast · / filter · S save · L load · R rm",
            View::Queue => "enter play-from · d remove · c clear",
            View::Waves => "p pause · n/b next/prev · s shuffle · r repeat · W bands · +/- vol",
            View::Audio => "o output · s speaker · v refresh · t test-cast · m mute · +/- vol",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum InputMode {
    Filter,
    SavePl,
    LoadPl,
    RmPl,
}

struct AudioState {
    roster: Vec<heos::HeosPlayer>,
    dlna_ok: bool,
    last_poll: Instant,
}

struct App {
    cfg: SirenConfig,
    focus: usize,
    lib_cache: Vec<PathBuf>,
    lib_at: Instant,
    browser_sel: usize,
    queue_sel: usize,
    filter: String,
    input: Option<InputMode>,
    input_buf: String,
    msg: String,
    msg_at: Instant,
    audio: AudioState,
    spk_rx: Option<std::sync::mpsc::Receiver<(Vec<heos::HeosPlayer>, bool)>>,
    spk_pending: bool,
    spk_at: Instant,
    spk_force: bool,
    mpv_label: String,
    mpv_pos: f64,
    mpv_dur: f64,
    mpv_paused: bool,
    mpv_vol: i64,
}

impl App {
    fn new() -> Self {
        let cfg = SirenConfig::load();
        let lib = library::scan_library(&cfg);
        Self {
            cfg,
            focus: 0,
            lib_cache: lib,
            lib_at: Instant::now(),
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
            spk_rx: None,
            spk_pending: false,
            spk_at: Instant::now() - Duration::from_secs(99),
            spk_force: true,
            mpv_label: String::new(),
            mpv_pos: 0.0,
            mpv_dur: 0.0,
            mpv_paused: false,
            mpv_vol: 75,
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

    fn filtered(&mut self) -> Vec<PathBuf> {
        if self.filter.trim().is_empty() {
            return self.lib().to_vec();
        }
        // resolve re-scans internally; acceptable on filter keystrokes only
        library::resolve_library(&self.cfg, &self.filter)
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
        match heos::dlna_cast(&ip, pid, path) {
            Ok(()) => self.say(format!("▶ {} on {name}", path_name(path))),
            Err(e) => self.say(format!("cast failed: {e}")),
        }
    }

    fn browser_selected(&mut self) -> Option<PathBuf> {
        self.filtered().get(self.browser_sel).cloned()
    }
}

fn path_name(p: &PathBuf) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

fn stem_name(p: &PathBuf) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_name(p))
}

pub fn run() -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let mut app = App::new();
    app.poll_mpv();
    let res = event_loop(&mut terminal, &mut app);

    disable_raw_mode()?;
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
            if let Event::Key(key) = event::read()? {
                if handle_key(app, key.code, key.modifiers) {
                    return Ok(());
                }
                terminal.draw(|f| draw(f, app))?;
                last_tick = Instant::now();
            }
        } else if last_tick.elapsed() >= Duration::from_millis(500) {
            last_tick = Instant::now();
            app.poll_mpv();
            app.poll_speaker();
            terminal.draw(|f| draw(f, app))?;
        } else {
            // harvest background speaker results without a full redraw
            app.poll_speaker();
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
        KeyCode::Tab => {
            app.focus = (app.focus + 1) % VIEWS.len();
            if app.view() == View::Audio {
                app.force_speaker_poll();
            }
        }
        KeyCode::BackTab => {
            app.focus = (app.focus + VIEWS.len() - 1) % VIEWS.len();
        }
        KeyCode::Char('q') | KeyCode::Esc => return true,
        _ => match app.view() {
            View::Browser => handle_browser(app, code, mods),
            View::Queue => handle_queue(app, code, mods),
            View::Waves => handle_waves(app, code, mods),
            View::Audio => handle_audio(app, code, mods),
        },
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
                if queue::play_queue_from(0, false) {
                    app.say(format!("Playing playlist: {name}"));
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
    }
}

fn handle_browser(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    let n = app.filtered().len();
    match code {
        KeyCode::Char('j') | KeyCode::Down => {
            if n > 0 {
                app.browser_sel = (app.browser_sel + 1).min(n - 1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.browser_sel = app.browser_sel.saturating_sub(1);
        }
        KeyCode::Enter | KeyCode::Char('p') | KeyCode::Char(' ') => {
            if let Some(p) = app.browser_selected() {
                if app.is_heos() {
                    app.cast_path(&p);
                } else {
                    let s = p.to_string_lossy().into_owned();
                    queue::start_playlist(&app.cfg, &[s], false);
                }
            }
        }
        KeyCode::Char('o') => {
            if let Some(p) = app.browser_selected() {
                let s = p.to_string_lossy().into_owned();
                queue::start_playlist(&app.cfg, &[s], false);
            }
        }
        KeyCode::Char('a') => {
            if let Some(p) = app.browser_selected() {
                let s = p.to_string_lossy().into_owned();
                // in-TUI session queue
                let items = queue::snapshot();
                let mut v = items;
                v.push(queue::QueueItem {
                    path: s.clone(),
                    display: stem_name(&p),
                    title: String::new(),
                    artist: String::new(),
                    duration: 0.0,
                });
                queue::replace(v);
                app.say(format!("queued {}", path_name(&p)));
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
        KeyCode::Char('+') | KeyCode::Char('=') => app.adjust_volume(5),
        KeyCode::Char('-') | KeyCode::Char('_') => app.adjust_volume(-5),
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
            if queue::play_queue_from(app.queue_sel, false) {
                app.say("playing from queue");
            }
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
        KeyCode::Char('+') | KeyCode::Char('=') => app.adjust_volume(5),
        KeyCode::Char('-') | KeyCode::Char('_') => app.adjust_volume(-5),
        _ => {}
    }
}

fn mpv_toggle_pause(app: &mut App) {
    if app.is_heos() {
        if let Some((ip, pid, name)) = app.speaker_target() {
            let to = heos::toggle(&ip, pid);
            app.say(format!("{name} → {to}"));
        } else {
            app.say("no speaker found");
        }
    } else {
        player::send(&serde_json::json!(["cycle", "pause"]));
    }
}

fn handle_waves(app: &mut App, code: KeyCode, _mods: KeyModifiers) {
    match code {
        KeyCode::Char('p') | KeyCode::Char(' ') => mpv_toggle_pause(app),
        KeyCode::Char('n') => {
            if app.is_heos() {
                if let Some((ip, pid, _)) = app.speaker_target() {
                    heos::play_next(&ip, pid);
                }
            } else {
                queue::cmd_next();
            }
        }
        KeyCode::Char('b') => {
            if app.is_heos() {
                if let Some((ip, pid, _)) = app.speaker_target() {
                    heos::play_previous(&ip, pid);
                }
            } else {
                queue::cmd_prev();
            }
        }
        KeyCode::Char('s') => {
            let cur = player::get_bool("shuffle", false);
            player::send(&serde_json::json!(["set_property", "shuffle", !cur]));
            app.say(format!("shuffle {}", if !cur { "on" } else { "off" }));
        }
        KeyCode::Char('r') => {
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
        KeyCode::Char('W') => {
            let nxt = match app.cfg.wave_bands {
                16 => 32,
                32 => 8,
                _ => 16,
            };
            app.cfg.wave_bands = nxt;
            let _ = app.cfg.save();
            app.say(format!("bands {nxt}"));
        }
        KeyCode::Char('w') => {
            app.cfg.waves = !app.cfg.waves;
            let _ = app.cfg.save();
            app.say(format!("waves {}", if app.cfg.waves { "on" } else { "off" }));
        }
        KeyCode::Char('+') | KeyCode::Char('=') => app.adjust_volume(5),
        KeyCode::Char('-') | KeyCode::Char('_') => app.adjust_volume(-5),
        _ => {}
    }
}

fn handle_audio(app: &mut App, code: KeyCode, _mods: KeyModifiers) {
    match code {
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
                }
            } else {
                app.say("no speaker found");
            }
        }
        KeyCode::Char('+') | KeyCode::Char('=') => app.adjust_volume(5),
        KeyCode::Char('-') | KeyCode::Char('_') => app.adjust_volume(-5),
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
        .title(top_title)
        .border_style(Style::default().fg(Color::Magenta));
    let inner = top_block.inner(chunks[0]);
    f.render_widget(top_block, chunks[0]);
    match focus {
        View::Browser => draw_browser(f, app, inner),
        View::Queue => draw_queue(f, app, inner),
        View::Waves => draw_waves(f, app, inner),
        View::Audio => draw_audio(f, app, inner),
    }

    // persistent waves strip — visible at all times, all views
    let wave_block = Block::default()
        .borders(Borders::ALL)
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
                View::Browser => format!("{} tracks · tab cycle · q quit", app.filtered().len()),
                View::Queue => format!("{} queued · tab cycle · q quit", queue::snapshot().len()),
                View::Waves => {
                    if app.mpv_label.is_empty() {
                        "idle · tab cycle · q quit".into()
                    } else {
                        format!("{} · tab cycle · q quit", app.mpv_label)
                    }
                }
                View::Audio => format!("out:{} · spk:{} · tab cycle · q quit", app.cfg.audio_output, app.cfg.audio_speaker),
            };
            lines.push(Line::from(Span::styled(format!("  {hint}"), Style::default().fg(Color::DarkGray))));
        }
        (format!(" ✦ {} menu ✦ ", focus.title()), lines)
    };
    let menu_block = Block::default()
        .borders(Borders::ALL)
        .title(menu_title)
        .border_style(Style::default().fg(Color::DarkGray));
    let menu_inner = menu_block.inner(chunks[2]);
    f.render_widget(menu_block, chunks[2]);
    f.render_widget(Paragraph::new(menu_lines), menu_inner);
}

fn draw_browser(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let items = app.filtered();
    let rows: Vec<ListItem> = items
        .iter()
        .map(|p| ListItem::new(Line::from(Span::raw(format!("  {}", stem_name(p))))))
        .collect();
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
            let d = if it.display.is_empty() { stem_name(&PathBuf::from(&it.path)) } else { it.display.clone() };
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
fn waves_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if app.is_heos() {
        let spk = app
            .audio
            .roster
            .iter()
            .find(|p| {
                p.name.to_lowercase().contains(&app.cfg.audio_speaker.to_lowercase())
            })
            .or_else(|| app.audio.roster.first());
        match spk {
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
                lines.push(Line::from(Span::styled(
                    "  heos output · v in audio view",
                    Style::default().fg(Color::DarkGray),
                )));
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
    let ratio = if app.mpv_dur > 0.0 {
        (app.mpv_pos / app.mpv_dur).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let bar_w = 24;
    let filled = (ratio * bar_w as f64) as usize;
    let bar: String = "█".repeat(filled) + &"░".repeat(bar_w - filled);
    lines.push(Line::from(Span::raw(format!(
        "  {bar} {} / {}",
        queue::fmt_clock(app.mpv_pos),
        queue::fmt_clock(app.mpv_dur)
    ))));
    lines
}

fn draw_waves(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let mut lines = waves_lines(app);
    if !lines.is_empty() && !app.mpv_label.is_empty() {
        // volume bar (wave_bands wide, nod to the EQ) — view-only extra
        let vb = app.cfg.wave_bands.max(8) as usize;
        let vf = ((app.mpv_vol as usize * vb) / 150).min(vb);
        let vbar: String = "█".repeat(vf) + &"░".repeat(vb - vf);
        lines.push(Line::from(Span::raw(format!("  vol {vbar} {}%", app.mpv_vol))));
    }
    f.render_widget(Paragraph::new(lines), area);
    if app.mpv_dur > 0.0 && area.height > 6 {
        let gauge_area = ratatui::layout::Rect {
            x: area.x,
            y: area.y + area.height - 1,
            width: area.width,
            height: 1,
        };
        let ratio = (app.mpv_pos / app.mpv_dur).clamp(0.0, 1.0);
        f.render_widget(
            Gauge::default()
                .ratio(ratio)
                .gauge_style(Style::default().fg(Color::Magenta))
                .label(""),
            gauge_area,
        );
    }
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
