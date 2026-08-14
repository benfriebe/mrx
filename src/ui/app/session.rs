//! Persisted UI state (section 09): the last set, filter, selection,
//! cursor, and the freshness poll's own settings, written on change and
//! read at startup so reopening the app puts you back where you left off.
//!
//! No serde: the shape is five scalars, a string list, and an optional
//! number, and a hand-written reader and writer is smaller than the
//! dependency would be. Three rules hold regardless of what is on disk: a
//! name the current set no longer has is dropped silently, since a config
//! edit is not an error; a missing or unparseable file reads exactly like
//! no file at all, so deleting it is a supported reset and a crash
//! mid-write can never lock the app out; and `-s` on the command line
//! always wins over whatever set was stored (enforced by the caller in
//! `main.rs`, not here).

use super::state::App;
use std::path::PathBuf;
use std::time::Duration;

/// `$XDG_STATE_HOME/mrx/ui.json`, falling back to `~/.local/state/mrx/ui.json`.
///
/// Deliberately not `dirs::state_dir()`, which is `None` on macOS and
/// Windows; the plan calls for this exact XDG-style path on every platform,
/// so `dirs::home_dir()` supplies the fallback home and the env var is read
/// directly (the same shape `sets::config_dir` uses for `XDG_CONFIG_HOME`).
fn session_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => dirs::home_dir()?.join(".local").join("state"),
    };
    Some(base.join("mrx").join("ui.json"))
}

/// The persisted shape itself. Every field is independently optional in
/// spirit even though some are non-`Option` types: an absent or malformed
/// value just falls back to that field's default.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    pub set: Option<String>,
    pub filter: String,
    pub selected: Vec<String>,
    pub cursor: Option<String>,
    /// `Some` means the poll was on, at this interval; `None` means it was
    /// off. This one field is both the poll's setting and its on/off state,
    /// matching the plan's own on-disk shape (a bare `"poll": 300` rather
    /// than a separate enabled flag).
    pub poll_interval: Option<Duration>,
    pub auto_update: bool,
}

impl Session {
    /// Snapshot the parts of `app` worth remembering across a restart.
    fn snapshot(app: &App) -> Self {
        Self {
            set: (app.set_label != "(unnamed)").then(|| app.set_label.clone()),
            filter: app.filter.clone(),
            selected: app
                .selected
                .iter()
                .filter_map(|&i| app.repos.get(i).map(|r| r.name.clone()))
                .collect(),
            cursor: app.repos.get(app.cursor).map(|r| r.name.clone()),
            poll_interval: app.poll_enabled.then_some(app.poll_interval),
            auto_update: app.auto_update,
        }
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
                    .collect()
            }
            "cursor" => session.cursor = value.into_string(),
            "poll" => session.poll_interval = value.into_u64().map(Duration::from_secs),
            "auto_update" => session.auto_update = value.into_bool().unwrap_or(false),
            _ => {}
        }
    }
    session
}

/// Write the session file, replacing it atomically (write, then rename)
/// so a crash mid-write can never leave a truncated file where a good one
/// used to be; [`load`] tolerates a truncated file anyway, this just makes
/// one less likely.
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
    let mut out = String::from("{\n");
    if let Some(set) = &s.set {
        out.push_str(&format!("  \"set\": {},\n", json::string(set)));
    }
    out.push_str(&format!("  \"filter\": {},\n", json::string(&s.filter)));
    let selected = s
        .selected
        .iter()
        .map(|n| json::string(n))
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!("  \"selected\": [{selected}],\n"));
    if let Some(cursor) = &s.cursor {
        out.push_str(&format!("  \"cursor\": {},\n", json::string(cursor)));
    }
    if let Some(interval) = s.poll_interval {
        out.push_str(&format!("  \"poll\": {},\n", interval.as_secs()));
    }
    out.push_str(&format!("  \"auto_update\": {}\n", s.auto_update));
    out.push_str("}\n");
    out
}

/// A minimal, hand-rolled JSON reader for exactly the flat shape this file
/// is written in: an object of strings, a number, a bool, and one
/// string array. Not a general-purpose parser.
mod json {
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
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
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
                ',' => continue,
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
                ',' => {
                    c.skip_ws();
                    continue;
                }
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
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn repo(name: &str) -> Repo {
        Repo {
            name: name.to_string(),
            path: PathBuf::from(format!("/nonexistent/{name}")),
            clone_url: None,
            keys: Default::default(),
        }
    }

    fn app_with(names: &[&str]) -> App {
        App::new(
            names.iter().map(|n| repo(n)).collect(),
            "work".into(),
            4,
            Default::default(),
            PathBuf::from("/dev/null"),
            false,
            None,
        )
    }

    /// Every session test points `XDG_STATE_HOME` at its own tempdir, but
    /// the variable itself is process-global, so tests that touch it share
    /// this lock rather than risk one clobbering another's.
    fn with_state_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

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
            app.poll_interval = Duration::from_secs(300);
            app.auto_update = true;

            save(&app).unwrap();
            let loaded = load();

            assert_eq!(loaded.set, Some("work".into()));
            assert_eq!(loaded.filter, "api");
            assert_eq!(loaded.selected, vec!["bill-api", "menu-api"]);
            assert_eq!(loaded.cursor, Some("mr-yum".into()));
            assert_eq!(loaded.poll_interval, Some(Duration::from_secs(300)));
            assert!(loaded.auto_update);
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

    #[test]
    fn a_poll_that_was_off_round_trips_as_off() {
        with_state_home(|_| {
            let app = app_with(&["foo"]);
            save(&app).unwrap();
            let loaded = load();
            assert_eq!(loaded.poll_interval, None);
            assert!(!loaded.auto_update);
        });
    }
}
