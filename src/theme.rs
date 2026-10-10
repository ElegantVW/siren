//! TUI palettes. RGB is compiled in (no host env, no house config files).
//!
//! Five names from the house book: pixie, blush, silver, siren, cave.
//! `ok` / `warn` stay the same on every set so play vs mute stay honest.

use ratatui::style::Color;

pub const NAMES: [&str; 5] = ["pixie", "blush", "silver", "siren", "cave"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub name: &'static str,
    pub accent: Color,
    pub silver: Color,
    pub dim: Color,
    pub ok: Color,
    pub warn: Color,
    pub fg: Color,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// House ok / warn — same on every palette.
const OK: Color = rgb(0x3d, 0xd6, 0x8c);
const WARN: Color = rgb(0xff, 0xb0, 0x20);
const FG: Color = rgb(0xff, 0xeb, 0xf2);
const SILVER: Color = rgb(0xc0, 0xc0, 0xc8);
const SILVER_DIM: Color = rgb(0x8a, 0x8a, 0x96);
const PINK_DIM: Color = rgb(0x9d, 0x5c, 0x75);

pub fn named(name: &str) -> Palette {
    match name.trim().to_ascii_lowercase().as_str() {
        "blush" => Palette {
            name: "blush",
            accent: rgb(0xf0, 0xb4, 0xc8),
            silver: SILVER,
            dim: PINK_DIM,
            ok: OK,
            warn: WARN,
            fg: FG,
        },
        "silver" => Palette {
            name: "silver",
            accent: SILVER,
            silver: FG,
            dim: SILVER_DIM,
            ok: OK,
            warn: WARN,
            fg: FG,
        },
        "siren" => Palette {
            name: "siren",
            accent: rgb(0x8e, 0xc4, 0xc8),
            silver: SILVER,
            dim: SILVER_DIM,
            ok: OK,
            warn: WARN,
            fg: FG,
        },
        "cave" => Palette {
            name: "cave",
            accent: rgb(0xc4, 0x4d, 0x7a),
            silver: SILVER,
            dim: PINK_DIM,
            ok: OK,
            warn: WARN,
            fg: FG,
        },
        _ => Palette {
            name: "pixie",
            accent: rgb(0xe8, 0x79, 0xa0),
            silver: SILVER,
            dim: PINK_DIM,
            ok: OK,
            warn: WARN,
            fg: FG,
        },
    }
}

/// Next name in the ring. Unknown → pixie, then blush.
pub fn cycle(current: &str) -> &'static str {
    let cur = named(current).name;
    let i = NAMES.iter().position(|n| *n == cur).unwrap_or(0);
    NAMES[(i + 1) % NAMES.len()]
}

pub fn is_name(s: &str) -> bool {
    NAMES.iter().any(|n| n.eq_ignore_ascii_case(s.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_named_and_unknown_falls_to_pixie() {
        for n in NAMES {
            assert_eq!(named(n).name, n);
            assert!(is_name(n));
        }
        assert_eq!(named("pixie").accent, rgb(0xe8, 0x79, 0xa0));
        assert_eq!(named("blush").accent, rgb(0xf0, 0xb4, 0xc8));
        assert_eq!(named("silver").accent, SILVER);
        assert_eq!(named("siren").accent, rgb(0x8e, 0xc4, 0xc8));
        assert_eq!(named("cave").accent, rgb(0xc4, 0x4d, 0x7a));
        assert_eq!(named("").name, "pixie");
        assert_eq!(named("Aether").name, "pixie");
        assert_eq!(named("SIREN").name, "siren");
        assert!(!is_name("aether"));
        assert!(!is_name("nope"));
    }

    #[test]
    fn cycle_wraps_the_five() {
        assert_eq!(cycle("pixie"), "blush");
        assert_eq!(cycle("blush"), "silver");
        assert_eq!(cycle("silver"), "siren");
        assert_eq!(cycle("siren"), "cave");
        assert_eq!(cycle("cave"), "pixie");
        assert_eq!(cycle("nope"), "blush");
        let mut n = "pixie";
        let mut seen = std::collections::HashSet::new();
        for _ in 0..NAMES.len() {
            seen.insert(n);
            n = cycle(n);
        }
        assert_eq!(seen.len(), 5);
        assert_eq!(n, "pixie");
    }

    #[test]
    fn ok_and_warn_are_stable_across_palettes() {
        let pixie = named("pixie");
        for n in NAMES {
            let p = named(n);
            assert_eq!(p.ok, pixie.ok, "{n} ok");
            assert_eq!(p.warn, pixie.warn, "{n} warn");
        }
    }

    /// Palettes must not consult this box. A foreign host with no
    /// house tree still gets the five colours.
    #[test]
    fn compiled_in_no_host_files() {
        let src = std::fs::read_to_string("src/theme.rs").expect("theme.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap();
        for needle in [
            "palette.env",
            "PALETTE_PINK",
            "std::env",
            "faeOS",
            "/home/evenweaker",
            ".config/fae",
        ] {
            assert!(
                !prod.contains(needle),
                "theme.rs production must not touch host paths/env ({needle})"
            );
        }
        assert!(!prod.contains("aether"), "teal set is named siren");
    }
}
