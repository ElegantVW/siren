//! Mixer — speaker, listening sinks, and per-app output control.
//!
//! Pulse lists run off the render thread. Never writes `Siren_Master`
//! (follow). HEOS speaker volume lives in the TUI via `heos::apply_volume`.

/// One live audio source.
#[derive(Debug, Clone, Default)]
pub struct SinkInput {
    pub id: u32,
    pub app: String,
    pub media: String,
    pub sink: u32,
    pub muted: bool,
    pub volume_pct: u32,
}

/// Parse `pactl list sink-inputs` into structured sources.
pub fn list_inputs() -> Vec<SinkInput> {
    let out = std::process::Command::new("pactl")
        .args(["list", "sink-inputs"])
        .output();
    let Ok(out) = out else { return Vec::new() };
    parse_inputs(&String::from_utf8_lossy(&out.stdout))
}

/// Pure parser — fully testable.
pub fn parse_inputs(text: &str) -> Vec<SinkInput> {
    let mut out = Vec::new();
    let mut cur = SinkInput::default();
    let mut in_block = false;
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Sink Input #") {
            if in_block {
                out.push(std::mem::take(&mut cur));
            }
            cur.id = rest.trim().parse().unwrap_or(0);
            in_block = true;
        } else if in_block {
            if let Some(v) = t.strip_prefix("Sink: ") {
                cur.sink = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = t.strip_prefix("Mute: ") {
                cur.muted = v.trim() == "yes";
            } else if t.starts_with("Volume: ") {
                // "Volume: front-left: 65536 / 100% / 0.00 dB, ..."
                if let Some(pct) = t.split('/').nth(1) {
                    cur.volume_pct = pct.trim().trim_end_matches('%').parse().unwrap_or(100);
                }
            } else if let Some(v) = t.strip_prefix("application.name = ") {
                cur.app = v.trim_matches('"').to_string();
            } else if let Some(v) = t.strip_prefix("media.name = ") {
                cur.media = v.trim_matches('"').to_string();
            } else if let Some(v) = t.strip_prefix("node.name = ") {
                if cur.app.is_empty() {
                    cur.app = v.trim_matches('"').to_string();
                }
            }
        }
    }
    if in_block {
        out.push(cur);
    }
    // drop the loopback itself — muting it would kill local monitoring,
    // and it's infrastructure, not a source
    out.retain(|s| !(s.app.contains("loopback") || s.media.contains("loopback")));
    out
}

/// Mute/unmute a sink-input by id.
pub fn set_mute(id: u32, mute: bool) -> bool {
    std::process::Command::new("pactl")
        .args([
            "set-sink-input-mute",
            &id.to_string(),
            if mute { "1" } else { "0" },
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Set a sink-input volume 0-100%.
pub fn set_volume(id: u32, pct: u32) -> bool {
    let pct = pct.clamp(0, 150);
    std::process::Command::new("pactl")
        .args(["set-sink-input-volume", &id.to_string(), &format!("{pct}%")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// One PipeWire/Pulse sink (the jack, a null sink, …).
#[derive(Debug, Clone, Default)]
pub struct Sink {
    pub name: String,
    pub description: String,
    pub muted: bool,
    pub volume_pct: u32,
}

/// Parse `pactl list sinks`.
pub fn list_sinks() -> Vec<Sink> {
    let out = std::process::Command::new("pactl")
        .args(["list", "sinks"])
        .output();
    let Ok(out) = out else { return Vec::new() };
    parse_sinks(&String::from_utf8_lossy(&out.stdout))
}

/// Pure parser — fully testable.
pub fn parse_sinks(text: &str) -> Vec<Sink> {
    let mut out = Vec::new();
    let mut cur = Sink::default();
    let mut in_block = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("Sink #") {
            if in_block {
                out.push(std::mem::take(&mut cur));
            }
            in_block = true;
        } else if in_block {
            if let Some(v) = t.strip_prefix("Name: ") {
                cur.name = v.trim().to_string();
            } else if let Some(v) = t.strip_prefix("Description: ") {
                cur.description = v.trim().to_string();
            } else if let Some(v) = t.strip_prefix("Mute: ") {
                cur.muted = v.trim() == "yes";
            } else if t.starts_with("Volume: ") {
                if let Some(pct) = t.split('/').nth(1) {
                    cur.volume_pct = pct.trim().trim_end_matches('%').parse().unwrap_or(100);
                }
            }
        }
    }
    if in_block {
        out.push(cur);
    }
    out
}

/// Headset analog — never Siren_Master (follow).
pub fn pick_phones(sinks: &[Sink]) -> Option<&Sink> {
    sinks
        .iter()
        .find(|s| !is_master_sink(&s.name) && sink_label(s) == "phones")
}

pub fn is_master_sink(name: &str) -> bool {
    name.contains("Siren_Master")
}

/// Listening Pulse sinks, analog first. Follow's null sink stays off the board.
pub fn output_sinks(sinks: &[Sink]) -> Vec<Sink> {
    let mut out: Vec<Sink> = sinks
        .iter()
        .filter(|s| !is_master_sink(&s.name))
        .cloned()
        .collect();
    out.sort_by_key(|s| if sink_label(s) == "phones" { 0u8 } else { 1 });
    out
}

pub fn sink_label(s: &Sink) -> String {
    let n = s.name.to_ascii_lowercase();
    let d = s.description.to_ascii_lowercase();
    if n.contains("analog") || d.contains("headphone") || d.contains("analog") {
        return "phones".into();
    }
    if n.contains("hdmi") || d.contains("hdmi") {
        return "hdmi".into();
    }
    if n.contains("bluez") || d.contains("bluetooth") {
        return "bt".into();
    }
    let short = s.name.rsplit('.').next().unwrap_or(s.name.as_str());
    if short.chars().count() <= 8 {
        short.to_string()
    } else {
        short.chars().take(8).collect()
    }
}

/// Two-line house swell used as the mute control (never the word "mute").
pub fn mute_mark(muted: bool) -> [&'static str; 2] {
    if muted {
        ["~ × ~", " ~ ~ "]
    } else {
        ["~~~~~", " ~ ~ "]
    }
}

pub fn set_sink_mute(name: &str, mute: bool) -> bool {
    if name.is_empty() || name.contains("Siren_Master") {
        return false;
    }
    std::process::Command::new("pactl")
        .args(["set-sink-mute", name, if mute { "1" } else { "0" }])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn set_sink_volume(name: &str, pct: u32) -> bool {
    if name.is_empty() || name.contains("Siren_Master") {
        return false;
    }
    let pct = pct.clamp(0, 150);
    std::process::Command::new("pactl")
        .args(["set-sink-volume", name, &format!("{pct}%")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Map a terminal row onto a vertical fader. Top of the bar is 100%, bottom is 0%.
pub fn fader_pct(y: u16, top: u16, h: u16) -> u32 {
    if h == 0 {
        return 0;
    }
    if y <= top {
        return 100;
    }
    let bottom = top.saturating_add(h.saturating_sub(1));
    if y >= bottom {
        return 0;
    }
    let span = h.saturating_sub(1) as u32;
    let rel = (y - top) as u32;
    100u32.saturating_sub((rel * 100) / span)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"Sink Input #466
	Driver: PipeWire
	Sink: 123
	Mute: no
	Volume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
	Properties:
		application.name = "mpv"
		media.name = "music-test.mp3 - mpv"
Sink Input #467
	Driver: PipeWire
	Sink: 123
	Mute: yes
	Volume: front-left: 32768 /  50% / -18.06 dB,   front-right: 32768 /  50% / -18.06 dB
	Properties:
		application.name = "zen-bin"
		media.name = "YouTube - mpv"
"#;

    #[test]
    fn missing_helper_is_empty() {
        assert!(parse_inputs("").is_empty());
        let src = std::fs::read_to_string("src/mixer.rs").expect("mixer.rs");
        assert!(
            src.contains("return Vec::new()"),
            "pactl missing must not crash the TUI"
        );
    }

    #[test]
    fn parses_two_inputs() {
        let v = parse_inputs(SAMPLE);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, 466);
        assert_eq!(v[0].app, "mpv");
        assert_eq!(v[0].media, "music-test.mp3 - mpv");
        assert_eq!(v[0].sink, 123);
        assert!(!v[0].muted);
        assert_eq!(v[0].volume_pct, 100);
        assert_eq!(v[1].id, 467);
        assert_eq!(v[1].app, "zen-bin");
        assert!(v[1].muted);
        assert_eq!(v[1].volume_pct, 50);
    }

    #[test]
    fn empty_input_gives_empty() {
        assert!(parse_inputs("").is_empty());
    }

    #[test]
    fn filters_loopback() {
        let text = "Sink Input #1\n\tSink: 123\n\tMute: no\n\tVolume: a / 100% / b\n\tProperties:\n\t\tapplication.name = \"loopback\"\n\t\tmedia.name = \"loopback\"\n";
        assert!(parse_inputs(text).is_empty());
    }

    #[test]
    fn missing_fields_default() {
        let v = parse_inputs("Sink Input #9\n");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, 9);
        assert_eq!(v[0].app, "");
        assert!(!v[0].muted);
    }

    const SINKS: &str = r#"Sink #31
	State: RUNNING
	Name: Siren_Master
	Description: Siren follow null
	Mute: no
	Volume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
Sink #4
	State: SUSPENDED
	Name: alsa_output.pci-0000_2f_00.1.hdmi-stereo
	Description: HDMI / DisplayPort
	Mute: no
	Volume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
Sink #57
	State: SUSPENDED
	Name: alsa_output.pci-0000_31_00.4.analog-stereo
	Description: HD Audio Controller Analog Stereo
	Mute: yes
	Volume: front-left: 26214 /  40% / -23.88 dB,   front-right: 26214 /  40% / -23.88 dB
"#;

    #[test]
    fn parses_sinks_and_picks_phones() {
        let v = parse_sinks(SINKS);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].name, "Siren_Master");
        let p = pick_phones(&v).expect("analog");
        assert!(p.name.contains("analog-stereo"));
        assert!(!p.name.contains("Siren"));
        let outs = output_sinks(&v);
        assert_eq!(outs.len(), 2);
        assert_eq!(sink_label(&outs[0]), "phones");
        assert_eq!(sink_label(&outs[1]), "hdmi");
        assert!(outs.iter().all(|s| !is_master_sink(&s.name)));
    }

    #[test]
    fn mute_mark_is_house_swell() {
        let live = mute_mark(false);
        let quiet = mute_mark(true);
        assert_eq!(live, ["~~~~~", " ~ ~ "]);
        assert_eq!(quiet, ["~ × ~", " ~ ~ "]);
        assert_eq!(live[0].chars().count(), 5);
        assert_eq!(quiet[0].chars().count(), 5);
        assert_ne!(live[0], quiet[0]);
    }

    #[test]
    fn refuses_master_as_phones_target() {
        assert!(!set_sink_mute("Siren_Master", true));
        assert!(!set_sink_volume("Siren_Master", 80));
        assert!(!set_sink_mute("", false));
    }

    #[test]
    fn fader_pct_top_is_loud() {
        assert_eq!(fader_pct(4, 4, 10), 100);
        assert_eq!(fader_pct(13, 4, 10), 0);
        let mid = fader_pct(8, 4, 10);
        assert!(mid >= 40 && mid <= 60, "mid was {mid}");
        assert_eq!(fader_pct(0, 4, 10), 100);
        assert_eq!(fader_pct(99, 4, 10), 0);
        assert_eq!(fader_pct(4, 4, 0), 0);
    }
}
