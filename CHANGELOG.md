# Siren changelog

## v0.1.0 (2026-09-23)

- Cut over to Rust engine: config, library/fuzzy, mpv IPC, queue/playlist,
  transport CLI, two-box TUI, audio menu + `cast`, trove, tags, dir browser.
- Speaker: HEOS CLI :1255 + DLNA; test casts at low volume.
- Evidence: live-verified; `cargo test` 2 pass.
- Holes: speaker radio rides TuneIn; raw-URL `play_stream` won't hold on this unit.
