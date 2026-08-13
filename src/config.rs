use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Keys mrx consumes itself. They are removed from the key maps after parsing so
/// they can never be resolved as an action body.
const RESERVED_KEYS: &[&str] = &["base", "skip"];

#[derive(Debug, Clone)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
    pub clone_url: Option<String>,
    pub keys: BTreeMap<String, String>,
}

impl Repo {
    /// Parse a key as a boolean, falling back to `default` when unset or unparseable.
    pub fn flag(keys: &BTreeMap<String, String>, key: &str, default: bool) -> bool {
        match keys.get(key).map(|v| v.trim().to_ascii_lowercase()) {
            Some(v) if v == "true" || v == "yes" || v == "1" => true,
            Some(v) if v == "false" || v == "no" || v == "0" => false,
            _ => default,
        }
    }
}

pub struct Config {
    pub repos: Vec<Repo>,
    pub defaults: BTreeMap<String, String>,
    /// Directory that section paths resolve against.
    pub base: PathBuf,
}

/// Expand a leading `~` against the home directory.
pub fn expand_tilde(s: &str) -> PathBuf {
    let s = s.trim();
    if s == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(s));
    }
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(s)
}

/// Read a config file into repos plus the `[DEFAULT]` fallbacks.
///
/// The base directory that section paths hang off is resolved here because it can
/// come from the config itself: `dir_override` (`-d`) beats `[DEFAULT] base`, which
/// beats the config file's own parent.
pub fn load(config_path: &Path, dir_override: Option<&Path>) -> Config {
    let fallback_base = || {
        config_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };

    let content = match std::fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Config {
                repos: Vec::new(),
                defaults: BTreeMap::new(),
                base: dir_override.map(Path::to_path_buf).unwrap_or_else(fallback_base),
            };
        }
        Err(e) => {
            eprintln!("error: cannot read {}: {}", config_path.display(), e);
            std::process::exit(1);
        }
    };

    // new_cs keeps section names verbatim; the default parser lowercases them, which
    // silently maps [dev/BulkPriceImport] to <base>/dev/bulkpriceimport. Keys are
    // lowercased by hand below so `Branch` and `branch` still mean the same thing.
    let mut ini = configparser::ini::Ini::new_cs();
    ini.set_multiline(true);
    // Values are shell command bodies, where `;` and `#` are ordinary characters.
    // Inline comment stripping would silently truncate `update = a; b` to `a`.
    // An empty list disables it; whole-line `;` and `#` comments still work.
    ini.set_inline_comment_symbols(Some(&[]));
    if let Err(e) = ini.read(content) {
        eprintln!("error: cannot parse {}: {}", config_path.display(), e);
        std::process::exit(1);
    }

    let mut sections: Vec<(String, BTreeMap<String, String>)> = Vec::new();
    let mut defaults: BTreeMap<String, String> = BTreeMap::new();

    for section in ini.sections() {
        let mut keys: BTreeMap<String, String> = BTreeMap::new();
        if let Some(map) = ini.get_map_ref().get(&section) {
            for (k, v) in map {
                if let Some(val) = v {
                    keys.insert(k.to_ascii_lowercase(), val.clone());
                }
            }
        }

        if section.eq_ignore_ascii_case("default") {
            defaults = keys;
        } else {
            sections.push((section, keys));
        }
    }

    let base = dir_override
        .map(Path::to_path_buf)
        .or_else(|| defaults.get("base").map(|b| expand_tilde(b)))
        .unwrap_or_else(fallback_base);

    let mut repos: Vec<Repo> = sections
        .into_iter()
        .filter(|(_, keys)| !Repo::flag(keys, "skip", false))
        .map(|(section, mut keys)| {
            for reserved in RESERVED_KEYS {
                keys.remove(*reserved);
            }
            Repo {
                name: section.rsplit('/').next().unwrap_or(&section).to_string(),
                path: base.join(&section),
                clone_url: keys.get("checkout").and_then(|cmd| extract_clone_url(cmd)),
                keys,
            }
        })
        .collect();

    for reserved in RESERVED_KEYS {
        defaults.remove(*reserved);
    }

    repos.sort_by(|a, b| a.name.cmp(&b.name));
    Config {
        repos,
        defaults,
        base,
    }
}

fn extract_clone_url(checkout_cmd: &str) -> Option<String> {
    let tokens: Vec<&str> = checkout_cmd.split_whitespace().collect();
    // find "clone" then take the next token as the URL
    for (i, tok) in tokens.iter().enumerate() {
        if *tok == "clone" {
            if let Some(url) = tokens.get(i + 1) {
                return Some(url.trim_matches('\'').trim_matches('"').to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &Path, body: &str) -> PathBuf {
        let cfg = dir.join(".mrconfig");
        std::fs::write(&cfg, body).unwrap();
        cfg
    }

    #[test]
    fn test_extract_clone_url_https() {
        let cmd = "git clone 'https://github.com/mr-yum/bill-api' 'bill-api'";
        assert_eq!(
            extract_clone_url(cmd),
            Some("https://github.com/mr-yum/bill-api".to_string())
        );
    }

    #[test]
    fn test_extract_clone_url_ssh() {
        let cmd = "git clone 'git@github.com:mr-yum/cli.git' 'cli'";
        assert_eq!(
            extract_clone_url(cmd),
            Some("git@github.com:mr-yum/cli.git".to_string())
        );
    }

    #[test]
    fn test_parse_config_with_defaults_and_custom_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = write_config(
            dir.path(),
            "[DEFAULT]\nupdate = git pull --rebase\n\n\
             [git/foo]\ncheckout = git clone 'git@github.com:example/foo.git' 'foo'\n\
             main = git checkout master\ninstall = npm install\n",
        );

        let config = load(&cfg, None);

        assert_eq!(config.repos.len(), 1);
        assert_eq!(
            config.defaults.get("update").map(String::as_str),
            Some("git pull --rebase")
        );

        let foo = &config.repos[0];
        assert_eq!(foo.name, "foo");
        assert_eq!(
            foo.clone_url.as_deref(),
            Some("git@github.com:example/foo.git")
        );
        assert_eq!(
            foo.keys.get("main").map(String::as_str),
            Some("git checkout master")
        );
        assert_eq!(
            foo.keys.get("install").map(String::as_str),
            Some("npm install")
        );
        assert!(foo.keys.contains_key("checkout"));
    }

    #[test]
    fn section_case_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = write_config(
            dir.path(),
            "[dev-trex/BulkPriceImport]\ncheckout = git clone 'git@example.com:BulkPriceImport' 'BulkPriceImport'\n",
        );

        let config = load(&cfg, None);
        assert_eq!(config.repos[0].name, "BulkPriceImport");
        assert!(config.repos[0].path.ends_with("dev-trex/BulkPriceImport"));
    }

    #[test]
    fn keys_are_lowercased() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = write_config(dir.path(), "[a]\nBranch = master\nUPDATE = echo hi\n");

        let config = load(&cfg, None);
        assert_eq!(
            config.repos[0].keys.get("branch").map(String::as_str),
            Some("master")
        );
        assert_eq!(
            config.repos[0].keys.get("update").map(String::as_str),
            Some("echo hi")
        );
    }

    #[test]
    fn base_precedence_override_beats_default_beats_parent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = write_config(dir.path(), "[DEFAULT]\nbase = /srv/code\n\n[a]\n");

        assert_eq!(load(&cfg, None).base, PathBuf::from("/srv/code"));
        assert_eq!(
            load(&cfg, Some(Path::new("/flag/wins"))).base,
            PathBuf::from("/flag/wins")
        );

        let bare = write_config(dir.path(), "[a]\n");
        assert_eq!(load(&bare, None).base, dir.path());
    }

    #[test]
    fn base_expands_tilde() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = write_config(dir.path(), "[DEFAULT]\nbase = ~/dev\n\n[a]\n");

        let home = dirs::home_dir().unwrap();
        assert_eq!(load(&cfg, None).base, home.join("dev"));
        assert_eq!(load(&cfg, None).repos[0].path, home.join("dev/a"));
    }

    #[test]
    fn default_section_never_becomes_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = write_config(dir.path(), "[DEFAULT]\nupdate = echo hi\n\n[a]\n[b]\n");

        let config = load(&cfg, None);
        let names: Vec<&str> = config.repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn skip_excludes_a_section() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = write_config(dir.path(), "[a]\n[b]\nskip = true\n[c]\nskip = no\n");

        let config = load(&cfg, None);
        let names: Vec<&str> = config.repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["a", "c"]);
    }

    #[test]
    fn reserved_keys_are_not_resolvable_actions() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = write_config(dir.path(), "[DEFAULT]\nbase = /srv\n\n[a]\nskip = false\n");

        let config = load(&cfg, None);
        assert!(!config.defaults.contains_key("base"));
        assert!(!config.repos[0].keys.contains_key("skip"));
    }

    #[test]
    fn flag_parses_truthy_and_falsy_spellings() {
        let mut keys = BTreeMap::new();
        for v in ["true", "yes", "1", "TRUE", " Yes "] {
            keys.insert("k".to_string(), v.to_string());
            assert!(Repo::flag(&keys, "k", false), "{v} should be true");
        }
        for v in ["false", "no", "0", "No"] {
            keys.insert("k".to_string(), v.to_string());
            assert!(!Repo::flag(&keys, "k", true), "{v} should be false");
        }
        keys.insert("k".to_string(), "maybe".to_string());
        assert!(Repo::flag(&keys, "k", true), "unparseable falls back");
        assert!(!Repo::flag(&BTreeMap::new(), "k", false), "unset falls back");
    }

    #[test]
    fn missing_config_yields_empty_with_usable_base() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("nope.mrconfig");

        let config = load(&cfg, None);
        assert!(config.repos.is_empty());
        assert_eq!(config.base, dir.path());
    }

    #[test]
    fn multiline_values_join_continuation_lines() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = write_config(dir.path(), "[a]\nupdate = git fetch -p\n  git pull\n");

        let body = load(&cfg, None).repos[0].keys["update"].clone();
        assert!(body.contains("git fetch -p"), "got {body:?}");
        assert!(body.contains("git pull"), "got {body:?}");
    }

#[test]
fn semicolon_in_a_command_body_survives() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), "[a]\nupdate = echo one; echo two\n");
    let body = load(&cfg, None).repos[0].keys["update"].clone();
    assert_eq!(body, "echo one; echo two", "semicolon treated as a comment");
}
}
