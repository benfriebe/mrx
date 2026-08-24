use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Keys mrx consumes itself. They are removed from the key maps after parsing so
/// they can never be resolved as an action body.
const RESERVED_KEYS: &[&str] = &["base", "skip", "jobs", "auto_fetch"];

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
    /// `[DEFAULT] jobs`, if the config sets one. Resolved against `-j` by
    /// [`max_jobs`].
    pub jobs: Option<usize>,
    /// `[DEFAULT] auto_fetch`: how often ui mode should fetch this set on its
    /// own. `None` is a config that says nothing, `Some(ZERO)` one that says
    /// `off`; the two differ only to a caller that has its own default.
    pub auto_fetch: Option<Duration>,
}

/// The interval `auto_fetch = on` means, and the one ui mode's own `F` starts
/// at, so a set that asks for auto-fetch without saying how often and a
/// session that turns it on by hand agree.
pub const DEFAULT_AUTO_FETCH: Duration = Duration::from_mins(6);

/// A config that could not be loaded: which file, and what was wrong with it.
///
/// Separate fields because the one-shot CLI prints both, while ui mode has one
/// status line and already knows which config it asked for, so it shows
/// [`kind`](Self::kind) alone.
#[derive(Debug, thiserror::Error)]
#[error("{}: {kind}", path.display())]
pub struct ConfigError {
    pub path: PathBuf,
    #[source]
    pub kind: ConfigErrorKind,
}

impl ConfigError {
    fn new(path: &Path, kind: ConfigErrorKind) -> Self {
        Self {
            path: path.to_path_buf(),
            kind,
        }
    }
}

/// What was wrong with a config, phrased to read after its path.
#[derive(Debug, thiserror::Error)]
pub enum ConfigErrorKind {
    #[error("cannot be read: {0}")]
    Read(#[source] std::io::Error),
    #[error("cannot be parsed: {0}")]
    Parse(String),
    #[error("jobs must be a whole number of at least 1, got '{0}'")]
    Jobs(String),
    #[error("auto_fetch must be on, off, or an interval like 6m; {0}")]
    AutoFetch(String),
}

/// How many jobs to run at once: `-j` beats `[DEFAULT] jobs`, which beats one
/// per CPU capped at 8, since these are git processes spending most of their
/// time on the network rather than the CPU.
pub fn max_jobs(flag: Option<usize>, from_config: Option<usize>) -> usize {
    flag.or(from_config)
        .unwrap_or_else(|| num_cpus::get().min(8))
}

/// `[DEFAULT] jobs`, the config's own answer to `-j`. Rejected rather than
/// ignored when unusable, and zero for the reason `cli::parse_jobs` gives.
fn parse_jobs(
    defaults: &BTreeMap<String, String>,
    config_path: &Path,
) -> Result<Option<usize>, ConfigError> {
    let Some(raw) = defaults.get("jobs") else {
        return Ok(None);
    };
    match raw.trim().parse::<usize>() {
        Ok(n) if n >= 1 => Ok(Some(n)),
        _ => Err(ConfigError::new(
            config_path,
            ConfigErrorKind::Jobs(raw.trim().to_string()),
        )),
    }
}

/// `[DEFAULT] auto_fetch`: `on`, `off`, or how often, as `90s`, `6m`, `1h`.
/// Rejected rather than ignored when unusable, since a misspelt interval that
/// silently meant "never fetch" would look exactly like a working config.
fn parse_auto_fetch(
    defaults: &BTreeMap<String, String>,
    config_path: &Path,
) -> Result<Option<Duration>, ConfigError> {
    let Some(raw) = defaults.get("auto_fetch") else {
        return Ok(None);
    };
    let raw = raw.trim();
    if raw.eq_ignore_ascii_case("on") {
        return Ok(Some(DEFAULT_AUTO_FETCH));
    }
    let interval = crate::cli::parse_duration(raw)
        .map_err(|e| ConfigError::new(config_path, ConfigErrorKind::AutoFetch(e)))?;
    Ok(Some(interval))
}

/// Expand a leading `~` against the home directory.
pub fn expand_tilde(s: &str) -> PathBuf {
    let s = s.trim();
    if s == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(s));
    }
    s.strip_prefix("~/")
        .and_then(|rest| dirs::home_dir().map(|home| home.join(rest)))
        .unwrap_or_else(|| PathBuf::from(s))
}

/// Read a config file into repos plus the `[DEFAULT]` fallbacks, exiting the
/// process on an unreadable or unparseable file.
///
/// For the one-shot CLI paths only. ui mode calls [`try_load`], since exiting
/// from inside raw mode bypasses teardown and leaves the terminal wrecked.
pub fn load(config_path: &Path, dir_override: Option<&Path>) -> Config {
    match try_load(config_path, dir_override) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

/// Read a config file into repos plus the `[DEFAULT]` fallbacks.
///
/// The base directory section paths hang off resolves here, since it can come
/// from the config itself: `dir_override` (`-d`) beats `[DEFAULT] base`, which
/// beats the config file's own parent.
///
/// A missing file yields an empty `Config`; only an unreadable or unparseable
/// one yields `Err`.
pub fn try_load(config_path: &Path, dir_override: Option<&Path>) -> Result<Config, ConfigError> {
    let fallback_base = || {
        config_path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    };

    let content = match std::fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config {
                repos: Vec::new(),
                defaults: BTreeMap::new(),
                base: dir_override.map_or_else(fallback_base, Path::to_path_buf),
                jobs: None,
                auto_fetch: None,
            });
        }
        Err(e) => {
            return Err(ConfigError::new(config_path, ConfigErrorKind::Read(e)));
        }
    };

    // new_cs keeps section names verbatim; the default parser lowercases them, which
    // silently maps [dev/BulkPriceImport] to <base>/dev/bulkpriceimport. Keys are
    // lowercased by hand below so `Branch` and `branch` still mean the same thing.
    let mut ini = configparser::ini::Ini::new_cs();
    ini.set_multiline(true);
    // Values are shell command bodies, so inline comment stripping would
    // truncate `update = a; b` to `a`. An empty list disables it; whole-line
    // `;` and `#` comments still work.
    ini.set_inline_comment_symbols(Some(&[]));
    if let Err(e) = ini.read(content) {
        return Err(ConfigError::new(config_path, ConfigErrorKind::Parse(e)));
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

    let jobs = parse_jobs(&defaults, config_path)?;
    let auto_fetch = parse_auto_fetch(&defaults, config_path)?;

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
    Ok(Config {
        repos,
        defaults,
        base,
        jobs,
        auto_fetch,
    })
}

fn extract_clone_url(checkout_cmd: &str) -> Option<String> {
    let tokens: Vec<&str> = checkout_cmd.split_whitespace().collect();
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
        let cfg = write_config(
            dir.path(),
            "[DEFAULT]\nbase = /srv\njobs = 4\nauto_fetch = 6m\n\n[a]\nskip = false\n",
        );

        let config = load(&cfg, None);
        assert!(!config.defaults.contains_key("base"));
        assert!(!config.defaults.contains_key("jobs"));
        assert!(!config.defaults.contains_key("auto_fetch"));
        assert!(!config.repos[0].keys.contains_key("skip"));
    }

    #[test]
    fn a_default_jobs_key_is_read_off_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = write_config(dir.path(), "[DEFAULT]\njobs = 10\n\n[a]\n");
        assert_eq!(load(&cfg, None).jobs, Some(10));

        let bare = write_config(dir.path(), "[a]\n");
        assert_eq!(load(&bare, None).jobs, None);
    }

    #[test]
    fn a_default_auto_fetch_key_says_whether_and_how_often() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            ("on", Some(DEFAULT_AUTO_FETCH)),
            ("6m", Some(Duration::from_mins(6))),
            ("90s", Some(Duration::from_secs(90))),
            ("1h", Some(Duration::from_hours(1))),
            ("OFF", Some(Duration::ZERO)),
        ];
        for (raw, expected) in cases {
            let cfg = write_config(
                dir.path(),
                &format!("[DEFAULT]\nauto_fetch = {raw}\n\n[a]\n"),
            );
            assert_eq!(load(&cfg, None).auto_fetch, expected, "auto_fetch = {raw}");
        }

        let bare = write_config(dir.path(), "[a]\n");
        assert_eq!(
            load(&bare, None).auto_fetch,
            None,
            "a config that says nothing is not a config that says off"
        );
    }

    /// A misspelt interval silently meaning "never fetch" looks exactly like a
    /// working config, so it is rejected where it can still be reported.
    #[test]
    fn an_unusable_auto_fetch_is_an_error_rather_than_a_silent_off() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = write_config(dir.path(), "[DEFAULT]\nauto_fetch = sometimes\n\n[a]\n");
        let Err(error) = try_load(&cfg, None) else {
            panic!("an unusable interval is refused");
        };
        assert!(
            matches!(error.kind, ConfigErrorKind::AutoFetch(_)),
            "got {error:?}"
        );
    }

    #[test]
    fn an_unusable_jobs_value_is_an_error_rather_than_a_silent_default() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["0", "-2", "lots", ""] {
            let cfg = write_config(dir.path(), &format!("[DEFAULT]\njobs = {bad}\n\n[a]\n"));
            let err = match try_load(&cfg, None) {
                Err(e) => e,
                Ok(c) => panic!("jobs = {bad:?} should not load, got {:?}", c.jobs),
            };
            assert!(matches!(err.kind, ConfigErrorKind::Jobs(_)), "got {err:?}");
        }
    }

    #[test]
    fn the_jobs_flag_beats_the_config_which_beats_the_cpu_default() {
        assert_eq!(max_jobs(Some(2), Some(10)), 2);
        assert_eq!(max_jobs(None, Some(10)), 10);
        let fallback = max_jobs(None, None);
        assert!((1..=8).contains(&fallback), "got {fallback}");
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
        assert!(
            !Repo::flag(&BTreeMap::new(), "k", false),
            "unset falls back"
        );
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

    #[test]
    fn try_load_reports_an_unparseable_config_instead_of_exiting() {
        let dir = tempfile::tempdir().unwrap();
        // An unclosed section bracket is invalid INI; `[foo\n` never closes.
        let cfg = write_config(dir.path(), "[foo\nbar = baz\n");

        let Err(err) = try_load(&cfg, None) else {
            panic!("an unparseable config must be an Err")
        };
        assert!(matches!(err.kind, ConfigErrorKind::Parse(_)), "got {err:?}");
        assert_eq!(err.path, cfg);
    }

    /// `load` prints the error and nothing else, so the rendered form has to
    /// carry the path the structured form keeps in a field.
    #[test]
    fn a_config_error_still_names_its_file_when_printed() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = write_config(dir.path(), "[DEFAULT]\njobs = lots\n\n[a]\n");
        let Err(err) = try_load(&cfg, None) else {
            panic!("an unusable jobs value is refused")
        };
        let rendered = err.to_string();
        assert!(
            rendered.contains(&cfg.display().to_string()),
            "should name the file: {rendered}"
        );
        assert!(rendered.contains("jobs"), "should name the key: {rendered}");
    }

    #[test]
    fn try_load_reports_an_unreadable_config_instead_of_exiting() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".mrconfig");
        std::fs::create_dir(&cfg).unwrap(); // a directory can't be read as a file

        let Err(err) = try_load(&cfg, None) else {
            panic!("an unreadable config must be an Err")
        };
        assert!(matches!(err.kind, ConfigErrorKind::Read(_)), "got {err:?}");
        assert_eq!(err.path, cfg);
    }

    #[test]
    fn try_load_treats_a_missing_config_as_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("nope.mrconfig");

        let config = try_load(&cfg, None).expect("a missing config is not an error");
        assert!(config.repos.is_empty());
    }
}
