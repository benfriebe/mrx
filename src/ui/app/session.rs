//! Persisted UI state: the last set, filter, selection, cursor, sort order and
//! the freshness poll's settings, written on change and read at startup.
//!
//! No serde: the shape is a handful of scalars and two string lists. Nothing on
//! disk is ever fatal, so a missing or unparseable file reads like no file at
//! all and deleting it is a supported reset.

use super::poll;
use super::state::{App, Direction, Sort};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `$XDG_STATE_HOME/mrx/ui.json`, falling back to `~/.local/state/mrx/ui.json`.
///
/// Deliberately not `dirs::state_dir()`, which is `None` on macOS and Windows;
/// this path is wanted on every platform, so the env var is read directly and
/// `dirs::home_dir()` only supplies the fallback home.
fn session_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => dirs::home_dir()?.join(".local").join("state"),
    };
    Some(base.join("mrx").join("ui.json"))
}

/// The persisted shape. An absent or malformed value falls back to that
/// field's default, whether or not the field is an `Option`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub set: Option<String>,
    pub filter: String,
    pub selected: Vec<String>,
    pub cursor: Option<String>,
    /// Repos known to have fetched, by name. `FETCH_HEAD` alone cannot
    /// re-derive this: a new process reads it for the first time with nothing
    /// to compare it against, so without this the behind count a previous
    /// session settled would go back to reading as unknown.
    pub fetched: Vec<String>,
    /// Both the setting and the on/off state, matching the on-disk shape (a
    /// bare `"poll": 300`, no separate enabled flag). `Some(ZERO)` is a session
    /// that turned the poll off and overrules a set's `auto_fetch`; `None` is a
    /// file that never said, and leaves `auto_fetch` to decide.
    pub poll_interval: Option<Duration>,
    /// When the last poll cycle went out, as wall-clock time: the boot fetch
    /// asks how stale the sync answers on screen are, and the answer has to
    /// survive the process that measured it. Absent when nothing has polled.
    pub checked: Option<SystemTime>,
    pub auto_update: bool,
    /// The order the table was left in. Column and direction are stored
    /// separately because the pair is what was chosen: a column reversed in
    /// place is not the same view as that column freshly picked.
    pub sort: Sort,
    pub sort_direction: Direction,
}

/// `Session::default` has to agree with `App::new` about the order a first
/// run opens in, so both take it from here.
impl Default for Session {
    fn default() -> Self {
        Self {
            set: None,
            filter: String::new(),
            selected: Vec::new(),
            cursor: None,
            fetched: Vec::new(),
            poll_interval: None,
            checked: None,
            auto_update: false,
            sort: Sort::default(),
            sort_direction: Sort::default().natural(),
        }
    }
}

impl Session {
    /// Snapshot the parts of `app` worth remembering across a restart.
    /// Visible to the rest of `app` so a restart can be exercised without
    /// touching the real session file.
    pub(super) fn snapshot(app: &App) -> Self {
        Self {
            set: (app.set_label != "(unnamed)").then(|| app.set_label.clone()),
            filter: app.filter.clone(),
            selected: app
                .selected
                .iter()
                .filter_map(|&i| app.repos.get(i).map(|r| r.name.clone()))
                .collect(),
            cursor: app.repos.get(app.cursor).map(|r| r.name.clone()),
            fetched: app
                .fetched_repos
                .iter()
                .filter_map(|&i| app.repos.get(i).map(|r| r.name.clone()))
                .collect(),
            poll_interval: Some(if app.poll_enabled {
                app.poll_interval
            } else {
                Duration::ZERO
            }),
            // Taken from the monotonic clock the app measures with, so a
            // wall clock nudged mid-session cannot backdate it.
            checked: app
                .last_poll_at
                .and_then(|at| SystemTime::now().checked_sub(at.elapsed())),
            auto_update: app.auto_update,
            sort: app.sort,
            sort_direction: app.sort_direction,
        }
    }

    /// How long ago the persisted cycle ran, for the boot fetch to judge.
    /// A stamp in the future, which a clock change is enough to produce,
    /// reads the same as no stamp at all: fetch, rather than trust it.
    pub fn checked_ago(&self) -> Option<Duration> {
        self.checked
            .and_then(|at| SystemTime::now().duration_since(at).ok())
    }
}

/// Read the persisted session, or [`Session::default`] when there is
/// nothing usable: no file, an unreadable one, or one that does not parse
/// as the object this module writes.
pub fn load() -> Session {
    let Some(path) = session_path() else {
        return Session::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Session::default();
    };
    let Some(fields) = json::parse_object(&text) else {
        return Session::default();
    };
    from_fields(fields)
}

fn from_fields(fields: Vec<(String, json::Value)>) -> Session {
    let mut session = Session::default();
    let mut direction = None;
    for (key, value) in fields {
        match key.as_str() {
            "set" => session.set = value.into_string(),
            "filter" => session.filter = value.into_string().unwrap_or_default(),
            "selected" => {
                session.selected = value
                    .into_array()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(json::Value::into_string)
                    .collect();
            }
            "cursor" => session.cursor = value.into_string(),
            "fetched" => {
                session.fetched = value
                    .into_array()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(json::Value::into_string)
                    .collect();
            }
            // A value beyond `MAX_POLL_INTERVAL` reads as the field being
            // absent rather than clamped down to the max: a corrupted
            // `ui.json` must not resurrect the poll at a value nobody asked
            // for.
            "poll" => {
                session.poll_interval = value
                    .into_u64()
                    .filter(|&secs| secs <= poll::MAX_POLL_INTERVAL.as_secs())
                    .map(Duration::from_secs);
            }
            "checked" => {
                session.checked = value
                    .into_u64()
                    .map(|secs| UNIX_EPOCH + Duration::from_secs(secs));
            }
            "auto_update" => session.auto_update = value.into_bool().unwrap_or(false),
            // A column mrx no longer has reads as the field being absent.
            "sort" => {
                if let Some(sort) = value.into_string().as_deref().and_then(Sort::from_name) {
                    session.sort = sort;
                }
            }
            "sort_direction" => {
                direction = value
                    .into_string()
                    .as_deref()
                    .and_then(Direction::from_name);
            }
            _ => {}
        }
    }
    // A direction the file does not have, or does not have usably, opens the
    // column the way choosing it fresh would. Resolved after the loop so it
    // cannot depend on which of the two keys the file lists first.
    session.sort_direction = direction.unwrap_or_else(|| session.sort.natural());
    session
}

/// Write the session file, replacing it atomically (write, then rename) so a
/// crash mid-write can't leave a truncated file where a good one used to be.
pub fn save(app: &App) -> std::io::Result<()> {
    let Some(path) = session_path() else {
        return Ok(());
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = to_json(&Session::snapshot(app));
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

fn to_json(s: &Session) -> String {
    // Writing into a `String` never fails, so every result here is discarded.
    let mut out = String::from("{\n");
    if let Some(set) = &s.set {
        let _ = writeln!(out, "  \"set\": {},", json::string(set));
    }
    let _ = writeln!(out, "  \"filter\": {},", json::string(&s.filter));
    let selected = s
        .selected
        .iter()
        .map(|n| json::string(n))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(out, "  \"selected\": [{selected}],");
    if let Some(cursor) = &s.cursor {
        let _ = writeln!(out, "  \"cursor\": {},", json::string(cursor));
    }
    let fetched = s
        .fetched
        .iter()
        .map(|n| json::string(n))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(out, "  \"fetched\": [{fetched}],");
    if let Some(interval) = s.poll_interval {
        let _ = writeln!(out, "  \"poll\": {},", interval.as_secs());
    }
    if let Some(secs) = s.checked.and_then(|t| t.duration_since(UNIX_EPOCH).ok()) {
        let _ = writeln!(out, "  \"checked\": {},", secs.as_secs());
    }
    let _ = writeln!(out, "  \"auto_update\": {},", s.auto_update);
    let _ = writeln!(out, "  \"sort\": {},", json::string(s.sort.name()));
    let _ = writeln!(
        out,
        "  \"sort_direction\": {}",
        json::string(s.sort_direction.name())
    );
    out.push_str("}\n");
    out
}

/// A JSON reader for exactly the flat shape this file is written in: an
/// object of strings, a number, a bool, and one string array. Not a
/// general-purpose parser.
mod json {
    use std::fmt::Write as _;

    #[derive(Debug, Clone, PartialEq)]
    pub enum Value {
        Str(String),
        Num(f64),
        Bool(bool),
        Array(Vec<Value>),
        Null,
    }

    impl Value {
        pub fn into_string(self) -> Option<String> {
            match self {
                Value::Str(s) => Some(s),
                _ => None,
            }
        }

        pub fn into_array(self) -> Option<Vec<Value>> {
            match self {
                Value::Array(a) => Some(a),
                _ => None,
            }
        }

        pub fn into_bool(self) -> Option<bool> {
            match self {
                Value::Bool(b) => Some(b),
                _ => None,
            }
        }

        pub fn into_u64(self) -> Option<u64> {
            match self {
                // The guard rules out a negative; a number too large saturates
                // at `u64::MAX`, which every caller's own bound rejects anyway.
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                Value::Num(n) if n >= 0.0 => Some(n as u64),
                _ => None,
            }
        }
    }

    /// Escape `s` into a JSON string literal, quotes included.
    pub fn string(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    let _ = write!(out, "\\u{:04x}", c as u32);
                }
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    struct Cursor {
        chars: Vec<char>,
        pos: usize,
    }

    impl Cursor {
        fn new(s: &str) -> Self {
            Self {
                chars: s.chars().collect(),
                pos: 0,
            }
        }

        fn peek(&self) -> Option<char> {
            self.chars.get(self.pos).copied()
        }

        fn advance(&mut self) -> Option<char> {
            let c = self.peek();
            if c.is_some() {
                self.pos += 1;
            }
            c
        }

        fn skip_ws(&mut self) {
            while matches!(self.peek(), Some(c) if c.is_whitespace()) {
                self.pos += 1;
            }
        }

        fn expect(&mut self, want: char) -> Option<()> {
            if self.peek() == Some(want) {
                self.pos += 1;
                Some(())
            } else {
                None
            }
        }
    }

    /// Parse a top-level JSON object into its fields, in file order.
    /// Returns `None` for anything that is not exactly a well-formed
    /// object: truncated input, a stray character, an unterminated string.
    pub fn parse_object(input: &str) -> Option<Vec<(String, Value)>> {
        let mut c = Cursor::new(input);
        c.skip_ws();
        c.expect('{')?;
        let mut fields = Vec::new();
        c.skip_ws();
        if c.peek() == Some('}') {
            c.pos += 1;
            return Some(fields);
        }
        loop {
            c.skip_ws();
            let key = parse_string(&mut c)?;
            c.skip_ws();
            c.expect(':')?;
            c.skip_ws();
            let value = parse_value(&mut c)?;
            fields.push((key, value));
            c.skip_ws();
            match c.advance()? {
                ',' => {}
                '}' => break,
                _ => return None,
            }
        }
        c.skip_ws();
        Some(fields)
    }

    fn parse_value(c: &mut Cursor) -> Option<Value> {
        c.skip_ws();
        match c.peek()? {
            '"' => parse_string(c).map(Value::Str),
            '[' => parse_array(c),
            't' => parse_literal(c, "true", Value::Bool(true)),
            'f' => parse_literal(c, "false", Value::Bool(false)),
            'n' => parse_literal(c, "null", Value::Null),
            _ => parse_number(c),
        }
    }

    fn parse_literal(c: &mut Cursor, literal: &str, value: Value) -> Option<Value> {
        for expected in literal.chars() {
            if c.advance()? != expected {
                return None;
            }
        }
        Some(value)
    }

    fn parse_string(c: &mut Cursor) -> Option<String> {
        c.expect('"')?;
        let mut s = String::new();
        loop {
            match c.advance()? {
                '"' => return Some(s),
                '\\' => match c.advance()? {
                    '"' => s.push('"'),
                    '\\' => s.push('\\'),
                    '/' => s.push('/'),
                    'n' => s.push('\n'),
                    't' => s.push('\t'),
                    'r' => s.push('\r'),
                    'u' => {
                        let mut hex = String::with_capacity(4);
                        for _ in 0..4 {
                            hex.push(c.advance()?);
                        }
                        let code = u32::from_str_radix(&hex, 16).ok()?;
                        s.push(char::from_u32(code)?);
                    }
                    _ => return None,
                },
                ch => s.push(ch),
            }
        }
    }

    fn parse_number(c: &mut Cursor) -> Option<Value> {
        let start = c.pos;
        if c.peek() == Some('-') {
            c.pos += 1;
        }
        while matches!(c.peek(), Some(ch) if ch.is_ascii_digit() || matches!(ch, '.' | 'e' | 'E' | '+' | '-'))
        {
            c.pos += 1;
        }
        if c.pos == start {
            return None;
        }
        let text: String = c.chars[start..c.pos].iter().collect();
        text.parse::<f64>().ok().map(Value::Num)
    }

    fn parse_array(c: &mut Cursor) -> Option<Value> {
        c.expect('[')?;
        let mut items = Vec::new();
        c.skip_ws();
        if c.peek() == Some(']') {
            c.pos += 1;
            return Some(Value::Array(items));
        }
        loop {
            items.push(parse_value(c)?);
            c.skip_ws();
            match c.advance()? {
                ',' => c.skip_ws(),
                ']' => break,
                _ => return None,
            }
        }
        Some(Value::Array(items))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Repo;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    fn repo(name: &str) -> Repo {
        Repo {
            name: name.to_string(),
            path: PathBuf::from(format!("/nonexistent/{name}")),
            clone_url: None,
            keys: BTreeMap::default(),
        }
    }

    fn app_with(names: &[&str]) -> App {
        App::new(
            names.iter().map(|n| repo(n)).collect(),
            "work".into(),
            4,
            BTreeMap::default(),
            PathBuf::from("/dev/null"),
            false,
            None,
        )
    }

    /// Every session test points `XDG_STATE_HOME` at its own tempdir, but the
    /// variable is process-global, so they share this lock rather than risk
    /// one clobbering another's.
    fn with_state_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        use std::sync::{Mutex, PoisonError};
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(PoisonError::into_inner);

        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_STATE_HOME");
        std::env::set_var("XDG_STATE_HOME", dir.path());
        let result = f(dir.path());
        match previous {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        result
    }

    #[test]
    fn a_session_round_trips_through_a_save_and_a_load() {
        with_state_home(|_| {
            let mut app = app_with(&["bill-api", "menu-api", "mr-yum"]);
            app.filter = "api".into();
            app.selected = BTreeSet::from([0, 1]);
            app.cursor = 2;
            app.poll_enabled = true;
            app.poll_interval = Duration::from_mins(5);
            app.auto_update = true;

            save(&app).unwrap();
            let loaded = load();

            assert_eq!(loaded.set, Some("work".into()));
            assert_eq!(loaded.filter, "api");
            assert_eq!(loaded.selected, vec!["bill-api", "menu-api"]);
            assert_eq!(loaded.cursor, Some("mr-yum".into()));
            assert_eq!(loaded.poll_interval, Some(Duration::from_mins(5)));
            assert!(loaded.auto_update);
        });
    }

    #[test]
    fn the_order_the_table_was_left_in_survives_a_restart() {
        with_state_home(|_| {
            let mut app = app_with(&["alpha", "beta"]);
            app.choose_sort(Sort::State);
            app.choose_sort(Sort::State); // and reversed off its natural direction
            save(&app).unwrap();

            let mut restarted = app_with(&["alpha", "beta"]);
            restarted.restore_session(&load());

            assert_eq!(restarted.sort, Sort::State);
            assert_eq!(restarted.sort_direction, Direction::Ascending);
        });
    }

    /// A file written before mrx sorted anything has neither key, and must
    /// open the way a first run does rather than on some half-set order.
    #[test]
    fn a_session_file_without_a_sort_opens_on_the_default_order() {
        let session = from_fields(vec![("filter".into(), json::Value::Str("api".into()))]);
        assert_eq!(session.sort, Sort::default());
        assert_eq!(session.sort_direction, Sort::default().natural());
    }

    /// The two keys are read independently, so a file listing them the other
    /// way round, or naming a column this build no longer has, still lands on
    /// a usable pair.
    #[test]
    fn an_unusable_sort_falls_back_a_field_at_a_time() {
        let named = |sort: &str, direction: &str| {
            from_fields(vec![
                ("sort_direction".into(), json::Value::Str(direction.into())),
                ("sort".into(), json::Value::Str(sort.into())),
            ])
        };
        let session = named("branch", "desc");
        assert_eq!(session.sort, Sort::Branch);
        assert_eq!(session.sort_direction, Direction::Descending);

        let session = named("nonesuch", "sideways");
        assert_eq!(session.sort, Sort::default());
        assert_eq!(session.sort_direction, Sort::default().natural());
    }

    #[test]
    fn a_repo_known_to_have_fetched_survives_a_save_and_a_load() {
        with_state_home(|_| {
            let mut app = app_with(&["alpha", "beta"]);
            app.fetched_repos = BTreeSet::from([1]);
            save(&app).unwrap();

            let mut restarted = app_with(&["alpha", "beta"]);
            restarted.restore_session(&load());

            assert_eq!(restarted.fetched_repos, BTreeSet::from([1]));
        });
    }

    /// The boot fetch reads this back after a restart, so a stamp that does
    /// not survive the file silently becomes "fetch the whole set on every
    /// launch".
    #[test]
    fn the_time_of_the_last_check_survives_a_save_and_a_load() {
        with_state_home(|_| {
            let mut app = app_with(&["alpha"]);
            app.poll_enabled = true;
            app.on_poll_due();
            save(&app).unwrap();

            let ago = load().checked_ago().expect("the cycle was recorded");
            assert!(ago < Duration::from_mins(1), "just now, got {ago:?}");
        });
    }

    #[test]
    fn a_session_that_never_polled_records_no_check_at_all() {
        with_state_home(|_| {
            let app = app_with(&["alpha"]);
            save(&app).unwrap();
            assert_eq!(load().checked_ago(), None);
        });
    }

    #[test]
    fn a_missing_file_loads_as_defaults() {
        with_state_home(|_| {
            assert_eq!(load(), Session::default());
        });
    }

    #[test]
    fn a_truncated_file_loads_as_defaults_instead_of_erroring() {
        with_state_home(|dir| {
            let path = dir.join("mrx").join("ui.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "{\"set\": \"work\", \"filter\"").unwrap();
            assert_eq!(load(), Session::default());
        });
    }

    /// The off state has to be recorded, not merely implied by the field's
    /// absence: a set whose `auto_fetch` says otherwise is applied first, and
    /// only an explicit zero can outrank it.
    #[test]
    fn a_poll_that_was_off_round_trips_as_an_explicit_off() {
        with_state_home(|_| {
            let app = app_with(&["foo"]);
            save(&app).unwrap();
            let loaded = load();
            assert_eq!(loaded.poll_interval, Some(Duration::ZERO));
            assert!(!loaded.auto_update);
        });
    }

    #[test]
    fn a_poll_value_beyond_the_sane_bound_loads_as_no_opinion_at_all() {
        with_state_home(|dir| {
            let path = dir.join("mrx").join("ui.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "{\"poll\": 18446744073709551615}").unwrap();
            let loaded = load();
            assert_eq!(
                loaded.poll_interval, None,
                "a hostile or fat-fingered poll value must degrade to the field \
                 being absent, not resurrect the poll at some clamped value"
            );
        });
    }

    #[test]
    fn a_sane_poll_value_still_loads_normally() {
        with_state_home(|dir| {
            let path = dir.join("mrx").join("ui.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "{\"poll\": 300}").unwrap();
            let loaded = load();
            assert_eq!(loaded.poll_interval, Some(Duration::from_mins(5)));
        });
    }
}
