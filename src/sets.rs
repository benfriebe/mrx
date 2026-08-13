//! Named repo sets. A set is just a config file with a name, so one binary and
//! one habit can cover several unrelated repo lists.

use std::path::PathBuf;

/// The set used when neither `-s` nor `$MRX_SET` names one.
pub const DEFAULT_SET: &str = "default";

pub const SET_SUFFIX: &str = ".mrconfig";

/// `$XDG_CONFIG_HOME/mrx`, falling back to `~/.config/mrx`.
///
/// Deliberately not `dirs::config_dir()`, which on macOS points at
/// ~/Library/Application Support.
pub fn config_dir() -> Option<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(x) if !x.is_empty() => Some(PathBuf::from(x).join("mrx")),
        _ => dirs::home_dir().map(|h| h.join(".config").join("mrx")),
    }
}

/// Where a set of this name could live, in precedence order.
pub fn candidates(name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = config_dir() {
        out.push(dir.join(format!("{name}{SET_SUFFIX}")));
    }
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(format!(".mrconfig-{name}")));
    }
    out
}

/// First candidate that exists.
pub fn resolve(name: &str) -> Option<PathBuf> {
    candidates(name).into_iter().find(|p| p.is_file())
}

/// The legacy single-config location, still the fallback for the default set.
pub fn legacy_config() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".mrconfig"))
}

/// Every set found on disk, sorted by name. A name found in both locations is
/// reported once, at the path `resolve` would pick.
pub fn discover() -> Vec<(String, PathBuf)> {
    let mut found: std::collections::BTreeMap<String, PathBuf> = Default::default();

    if let Some(home) = dirs::home_dir() {
        if let Ok(entries) = std::fs::read_dir(&home) {
            for entry in entries.flatten() {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                if let Some(set) = name.strip_prefix(".mrconfig-") {
                    if !set.is_empty() {
                        found.insert(set.to_string(), entry.path());
                    }
                }
            }
        }
    }

    // Second, so it overwrites the legacy path for the same name.
    if let Some(dir) = config_dir() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                if let Some(set) = name.strip_suffix(SET_SUFFIX) {
                    if !set.is_empty() {
                        found.insert(set.to_string(), entry.path());
                    }
                }
            }
        }
    }

    found.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_prefer_config_dir_over_dotfile() {
        let c = candidates("work");
        assert!(c.len() >= 2);
        assert!(c[0].ends_with("mrx/work.mrconfig"), "got {:?}", c[0]);
        assert!(c[1].ends_with(".mrconfig-work"), "got {:?}", c[1]);
    }

    #[test]
    fn config_dir_honours_xdg() {
        // Serialised implicitly: the other tests here don't touch XDG_CONFIG_HOME.
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/xdg-test");
        assert_eq!(config_dir(), Some(PathBuf::from("/tmp/xdg-test/mrx")));
        match previous {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
}
