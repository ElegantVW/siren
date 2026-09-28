//! Mixer — per-source output control.
//!
//! Lists every live sink-input (app playing audio), lets the user
//! mute/unmute and adjust each one. Synchronous pactl parsing,
//! throttled by the caller — cheap enough to run on the UI tick.

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
                    cur.volume_pct = pct
                        .trim()
                        .trim_end_matches('%')
                        .parse()
                        .unwrap_or(100);
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
    out.retain(|s| {
        !(s.app.contains("loopback") || s.media.contains("loopback"))
    });
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
}
