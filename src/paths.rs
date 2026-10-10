//! User directories. No `/root` fallback, no host-specific home.

use std::path::{Path, PathBuf};

pub fn home_dir() -> PathBuf {
    for key in ["HOME", "USERPROFILE"] {
        if let Ok(h) = std::env::var(key) {
            if !h.is_empty() {
                return PathBuf::from(h);
            }
        }
    }
    std::env::temp_dir()
}

pub fn config_dir() -> PathBuf {
    if let Ok(d) = std::env::var("SIREN_CONFIG_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    home_dir().join(".config").join("siren")
}

pub fn cache_dir() -> PathBuf {
    home_dir().join(".cache").join("siren")
}

pub fn music_dir() -> PathBuf {
    home_dir().join("Music")
}

/// Unix: `$XDG_RUNTIME_DIR` or temp. Windows: temp.
pub fn runtime_dir() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    std::env::temp_dir()
}

pub fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        home_dir().join(rest)
    } else if p == "~" {
        home_dir()
    } else {
        PathBuf::from(p)
    }
}

pub fn default_library_roots() -> Vec<String> {
    vec!["~/Music".into()]
}

/// mpv IPC endpoint. Unix socket path or Windows named pipe.
pub fn mpv_ipc() -> String {
    if let Ok(s) = std::env::var("SIREN_SOCK") {
        if !s.is_empty() {
            return s;
        }
    }
    #[cfg(windows)]
    {
        r"\\.\pipe\siren-mpv".to_string()
    }
    #[cfg(not(windows))]
    {
        runtime_dir().join("siren-mpv.sock").to_string_lossy().into_owned()
    }
}

pub fn is_windows_pipe(s: &str) -> bool {
    s.starts_with(r"\\.\pipe\") || s.starts_with("//./pipe/")
}

pub fn follow_json() -> PathBuf {
    cache_dir().join("follow.json")
}

pub fn join_config(name: &str) -> PathBuf {
    config_dir().join(name)
}

pub fn join_cache(name: &str) -> PathBuf {
    cache_dir().join(name)
}

pub fn ether_heos() -> PathBuf {
    home_dir().join(".config").join("ether").join("heos.json")
}

/// True when `p` is under `home`.
pub fn under_home(p: &Path) -> bool {
    p.starts_with(home_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_home_tilde() {
        let h = home_dir();
        assert_eq!(expand_home("~/Music"), h.join("Music"));
        assert_eq!(expand_home("~"), h);
        assert_eq!(expand_home("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn config_and_cache_live_under_home() {
        let h = home_dir();
        assert!(config_dir().starts_with(&h));
        assert!(cache_dir().starts_with(&h));
        assert_eq!(music_dir(), h.join("Music"));
        assert_eq!(follow_json(), h.join(".cache/siren/follow.json"));
    }

    #[test]
    fn mpv_ipc_shape() {
        if std::env::var("SIREN_SOCK").is_ok() {
            return;
        }
        let s = mpv_ipc();
        #[cfg(windows)]
        assert!(is_windows_pipe(&s), "{s}");
        #[cfg(not(windows))]
        {
            assert!(!is_windows_pipe(&s), "{s}");
            assert!(s.ends_with("siren-mpv.sock"), "{s}");
        }
        assert!(is_windows_pipe(r"\\.\pipe\siren-mpv"));
        assert!(!is_windows_pipe("/tmp/siren-mpv.sock"));
    }

    #[test]
    fn production_src_has_no_host_home() {
        for f in std::fs::read_dir("src").unwrap() {
            let p = f.unwrap().path();
            if p.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let src = std::fs::read_to_string(&p).unwrap();
            let prod = src.split("#[cfg(test)]").next().unwrap();
            assert!(
                !prod.contains("/home/evenweaker"),
                "{} production must not hardcode this host home",
                p.display()
            );
            assert!(
                !prod.contains("unwrap_or_else(|_| \"/root\".into())"),
                "{} production must not fall back to /root",
                p.display()
            );
        }
    }
}
