use clap::Parser;
use mrx::cli::{Cli, Command};
use mrx::{config, executor, operations, render_plain, sets, ui};
use std::io::{stdout, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| p.to_path_buf(), |d| d.join(p))
    }
}

/// The set named by `-s` or `$MRX_SET`, in that order, ignoring blanks.
fn named_set(cli: &Cli) -> Option<String> {
    cli.set
        .clone()
        .or_else(|| std::env::var("MRX_SET").ok())
        .filter(|s| !s.trim().is_empty())
}

/// `-c` wins outright. Otherwise the set named by `-s` or `$MRX_SET` must exist:
/// a named set that resolves to nothing is a typo, not a reason to silently
/// operate on a different repo list. Only the implicit default set falls back to
/// ~/.mrconfig, which keeps an untouched setup working.
fn resolve_config_path(cli: &Cli) -> PathBuf {
    if let Some(ref p) = cli.config {
        return absolutize(p);
    }

    let raw = match named_set(cli) {
        Some(name) => sets::resolve(&name).unwrap_or_else(|| {
            eprintln!("error: no config for set '{name}'. Looked in:");
            for candidate in sets::candidates(&name) {
                eprintln!("  {}", candidate.display());
            }
            eprintln!("run `mrx sets` to see what is defined");
            std::process::exit(2);
        }),
        None => sets::resolve(sets::DEFAULT_SET)
            .or_else(sets::legacy_config)
            .expect("cannot determine home directory"),
    };
    absolutize(&raw)
}

fn print_sets(active: &Path) {
    let found = sets::discover();

    if found.is_empty() {
        let dir =
            sets::config_dir().map_or_else(|| "~/.config/mrx".into(), |d| d.display().to_string());
        println!("no sets defined. Create {}/<name>{}", dir, sets::SET_SUFFIX);
    }

    for (name, path) in &found {
        let marker = if path == active { "*" } else { " " };
        println!("{} {:16} {}", marker, name, path.display());
    }

    // The active config may be an unnamed one: ~/.mrconfig, or an explicit -c.
    if !found.iter().any(|(_, p)| p == active) {
        println!(
            "* {:16} {}{}",
            "(unnamed)",
            active.display(),
            missing_note(active.is_file())
        );
    }
}

/// `discover` only returns files that exist, so the unnamed row is the one place
/// `sets` can point at a path that is not there.
fn missing_note(exists: bool) -> &'static str {
    if exists {
        ""
    } else {
        "  (missing)"
    }
}

/// Label shown in ui mode's header: the named set if `-s` or `$MRX_SET` gave
/// one, the persisted session's set if `restored` named one, otherwise
/// `(unnamed)` for the bare config file, the same label `print_sets` uses.
fn ui_set_label(cli: &Cli, restored: Option<&str>) -> String {
    named_set(cli)
        .or_else(|| restored.map(String::from))
        .unwrap_or_else(|| "(unnamed)".to_string())
}

/// The persisted session's stored set, but only when it should actually be
/// used: nothing on the command line named one (`-s` always wins), and the
/// stored name still resolves to a config on disk. A set removed since the
/// last session falls back to the ordinary default rather than erroring.
///
/// The name comes back with the path it resolved to, so the caller uses the
/// answer this lookup gave rather than asking again: a set file removed
/// between the two calls would have made the second one fail.
fn restored_set(
    cli: &Cli,
    session: &ui::app::session::Session,
) -> Option<(String, std::path::PathBuf)> {
    if cli.config.is_some() || named_set(cli).is_some() {
        return None;
    }
    let name = session.set.as_deref()?;
    Some((name.to_string(), sets::resolve(name)?))
}

/// `ui` needs an interactive terminal and contradicts `--plain`. Both are
/// invocation errors independent of whether a config exists, so they are
/// checked before anything that depends on the config being there.
fn reject_bad_ui_invocation(cli: &Cli) {
    if !matches!(cli.command, Command::Ui) {
        return;
    }
    if cli.plain {
        eprintln!("error: `ui` and `--plain` contradict each other");
        eprintln!("`--plain` disables the interactive view that `ui` opens");
        std::process::exit(2);
    }
    if !stdout().is_terminal() {
        eprintln!("error: `ui` needs an interactive terminal");
        eprintln!(
            "stdout is not a tty; use a non-interactive subcommand instead, e.g. `mrx status` or `mrx list`"
        );
        std::process::exit(2);
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    reject_bad_ui_invocation(&cli);

    // Only `ui` has a session to restore.
    let ui_session = matches!(cli.command, Command::Ui).then(ui::app::session::load);
    let ui_restored_set = ui_session.as_ref().and_then(|s| restored_set(&cli, s));

    let config_path = match &ui_restored_set {
        Some((_, resolved)) => absolutize(resolved),
        None => resolve_config_path(&cli),
    };

    // Independent of the config's contents.
    if matches!(cli.command, Command::Sets) {
        print_sets(&config_path);
        return;
    }

    // A config that is not there is a mistake, not an empty repo list: without
    // this every action succeeds having done nothing. `register` is exempt
    // because it is how the first config gets written.
    if !config_path.is_file() && !cli.command.is_register() {
        eprintln!("error: no config at {}", config_path.display());
        if cli.config.is_none() && named_set(&cli).is_none() {
            eprintln!("no set was named, so this is the default location");
            eprintln!("name one with --set or MRX_SET, or run `mrx sets` to see what is defined");
        }
        std::process::exit(2);
    }

    let dir_override = cli.directory.as_deref().map(absolutize);
    let config::Config {
        repos,
        defaults,
        base,
        jobs: config_jobs,
        auto_fetch,
    } = config::load(&config_path, dir_override.as_deref());

    if cli.command.is_register() {
        register(&config_path, &base);
        return;
    }

    if cli.command.is_list() {
        for repo in &repos {
            let exists = repo.path.is_dir();
            let marker = if exists { "✓" } else { "-" };
            println!("{} {:24} {}", marker, repo.name, repo.path.display());
        }
        return;
    }

    if matches!(cli.command, Command::Ui) {
        let label = ui_set_label(
            &cli,
            ui_restored_set.as_ref().map(|(name, _)| name.as_str()),
        );
        ui::app::run(ui::app::RunOptions {
            repos,
            set_label: label,
            jobs: config::max_jobs(cli.jobs, config_jobs),
            jobs_flag: cli.jobs,
            defaults,
            config_path: config_path.clone(),
            force: cli.force,
            dir_override: dir_override.clone(),
            session: ui_session.unwrap_or_default(),
            auto_fetch,
            // `--result-ttl off` arrives as zero; see `cli::parse_duration`.
            result_ttl: match cli.result_ttl {
                None => Some(ui::app::state::DEFAULT_RESULT_TTL),
                Some(d) if d.is_zero() => None,
                Some(d) => Some(d),
            },
        })
        .await
        .expect("ui error");
        return;
    }

    // Reject unknown actions before dispatch so typos like `mrx statsu` don't
    // silently succeed-by-skipping every repo.
    if let Command::Custom(parts) = &cli.command {
        let name = parts.first().map_or("", String::as_str);
        let known = !name.is_empty()
            && (defaults.contains_key(name) || repos.iter().any(|r| r.keys.contains_key(name)));
        if !known {
            eprintln!("error: unknown action '{name}' (not defined in any repo or [DEFAULT])");
            std::process::exit(2);
        }
    }

    let ops: Vec<operations::Operation> = repos
        .iter()
        .map(|r| operations::plan(&cli.command, r, &defaults))
        .collect();

    let jobs = config::max_jobs(cli.jobs, config_jobs);
    let rx = executor::execute_all(&repos, ops, jobs, config_path.clone());

    let success = if stdout().is_terminal() && !cli.plain {
        ui::run::run(
            repos,
            &cli.command,
            rx,
            jobs,
            &defaults,
            &config_path,
            cli.exit_on_done,
        )
        .expect("TUI error")
    } else {
        render_plain::run(repos, cli.command.display_name(), rx).await
    };

    std::process::exit(i32::from(!success));
}

/// Whether `existing` already declares `[section]`.
///
/// Only an unindented line counts. A section header sits at column 0, while
/// `configparser` reads an indented line as a continuation of the value above
/// it, so `[repos/x]` inside a multi-line command body is text rather than a
/// registration.
fn has_section(existing: &str, section: &str) -> bool {
    let header = format!("[{section}]");
    existing
        .lines()
        .filter(|line| !line.starts_with([' ', '\t']))
        .any(|line| line.trim_end() == header)
}

/// `s` as a single-quoted shell word. A quote inside the word has to close,
/// escape and reopen, since single quotes admit no escape of their own.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn register(config_path: &Path, base_dir: &Path) {
    let cwd = std::env::current_dir().expect("cannot determine current directory");

    let output = StdCommand::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(&cwd)
        .output();

    let url = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => {
            eprintln!("error: not a git repo or no 'origin' remote");
            std::process::exit(1);
        }
    };

    let repo_name = cwd
        .file_name()
        .expect("cannot determine directory name")
        .to_string_lossy();

    let Ok(relative) = cwd.strip_prefix(base_dir) else {
        eprintln!(
            "error: {} is not under base dir {}",
            cwd.display(),
            base_dir.display()
        );
        std::process::exit(1);
    };
    let section = relative.to_string_lossy().to_string();

    let existing = std::fs::read_to_string(config_path).unwrap_or_default();
    if has_section(&existing, &section) {
        eprintln!("already registered: {section}");
        return;
    }

    // A newline cannot be quoted onto a single config line, and writing it
    // anyway would append a stray unindented line to someone's config.
    if url.contains('\n') || repo_name.contains('\n') {
        eprintln!("error: the remote url or directory name contains a newline");
        std::process::exit(1);
    }

    let entry = format!(
        "\n[{section}]\ncheckout = git clone {} {}\n",
        shell_quote(&url),
        shell_quote(&repo_name)
    );

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(config_path)
        .unwrap_or_else(|e| {
            eprintln!("error: cannot write {}: {}", config_path.display(), e);
            std::process::exit(1);
        });

    file.write_all(entry.as_bytes()).unwrap_or_else(|e| {
        eprintln!("error: write failed: {e}");
        std::process::exit(1);
    });

    println!("registered {section} ({url})");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The check used to be a substring test over the whole file, so a
    /// `[repos/x]` anywhere in it, most easily inside a command body, read as
    /// a registration and made `mrx register` a silent no-op.
    #[test]
    fn a_section_name_inside_a_command_body_is_not_a_registration() {
        let config = "[repos/a]\nupdate = grep -n '[repos/x]' notes.txt\n";
        assert!(has_section(config, "repos/a"));
        assert!(!has_section(config, "repos/x"));
    }

    /// Continuation lines of a multi-line value are indented, which is the
    /// only thing separating them from a header.
    #[test]
    fn an_indented_header_is_a_continuation_line_rather_than_a_section() {
        let config = "[repos/a]\nupdate = echo one\n    [repos/x]\n";
        assert!(!has_section(config, "repos/x"));
    }

    #[test]
    fn a_header_is_still_found_with_trailing_whitespace_or_no_final_newline() {
        assert!(has_section("[repos/x]  ", "repos/x"));
        assert!(has_section(
            "[DEFAULT]\nbase = /tmp\n\n[repos/x]",
            "repos/x"
        ));
    }

    #[test]
    fn a_longer_section_name_does_not_match_a_shorter_one() {
        assert!(!has_section("[repos/xy]\n", "repos/x"));
    }

    /// The entry is a shell command body, so a quote in a directory name or a
    /// remote url used to end the quoting early and hand the rest to `sh`.
    #[test]
    fn a_quote_in_a_name_stays_inside_the_quoted_word() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(
            shell_quote("a'; touch pwned; '"),
            r"'a'\''; touch pwned; '\'''"
        );
    }

    /// The escaping is only worth anything if a real shell agrees, so this
    /// asks one what the word came out as.
    #[test]
    fn a_shell_reads_a_quoted_word_back_as_the_original() {
        for raw in [
            "plain",
            "it's",
            "a'; touch pwned; '",
            "two words",
            "$HOME `id`",
        ] {
            let out = StdCommand::new("sh")
                .args(["-c", &format!("printf %s {}", shell_quote(raw))])
                .output()
                .expect("sh is available");
            assert!(out.status.success(), "sh rejected {raw:?}");
            assert_eq!(String::from_utf8_lossy(&out.stdout), raw);
        }
    }

    #[test]
    fn the_unnamed_row_says_when_its_path_is_not_there() {
        assert_eq!(missing_note(true), "");
        assert_eq!(missing_note(false), "  (missing)");
    }

    #[test]
    fn an_explicit_set_beats_the_environment_and_blanks_are_ignored() {
        let cli = Cli::parse_from(["mrx", "--set", "work", "status"]);
        assert_eq!(named_set(&cli).as_deref(), Some("work"));

        let blank = Cli::parse_from(["mrx", "--set", "   ", "status"]);
        assert_eq!(blank.set.as_deref(), Some("   "), "clap kept the raw value");
        assert_eq!(
            named_set(&blank),
            None,
            "a blank set is not a set, so the default applies"
        );
    }

    #[test]
    fn an_explicit_set_beats_a_stored_session_set() {
        let cli = Cli::parse_from(["mrx", "--set", "other", "ui"]);
        let session = ui::app::session::Session {
            set: Some("work".into()),
            ..Default::default()
        };
        assert_eq!(
            restored_set(&cli, &session),
            None,
            "-s always wins over whatever set was stored"
        );
    }

    fn restore(key: &str, previous: Option<std::ffi::OsString>) {
        match previous {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    /// The environment is process-global; tests that point it at their own
    /// tempdir still need to serialise against each other.
    ///
    /// `MRX_SET` is cleared alongside it because these tests ask what happens
    /// when nothing names a set, and clap reads that variable into `--set`. A
    /// developer who exports it for their own use would otherwise fail them.
    fn with_config_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        use std::sync::{Mutex, PoisonError};
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(PoisonError::into_inner);

        let dir = tempfile::tempdir().unwrap();
        let previous_config_home = std::env::var_os("XDG_CONFIG_HOME");
        let previous_set = std::env::var_os("MRX_SET");
        std::env::set_var("XDG_CONFIG_HOME", dir.path());
        std::env::remove_var("MRX_SET");
        let result = f(dir.path());
        restore("XDG_CONFIG_HOME", previous_config_home);
        restore("MRX_SET", previous_set);
        result
    }

    #[test]
    fn a_stored_session_set_is_used_when_nothing_on_the_command_line_names_one() {
        with_config_home(|dir| {
            std::fs::create_dir_all(dir.join("mrx")).unwrap();
            std::fs::write(dir.join("mrx/work.mrconfig"), "[repo]\n").unwrap();

            let cli = Cli::parse_from(["mrx", "ui"]);
            let session = ui::app::session::Session {
                set: Some("work".into()),
                ..Default::default()
            };
            let (name, path) = restored_set(&cli, &session).expect("the stored set resolves");
            assert_eq!(name, "work");
            assert_eq!(path, dir.join("mrx/work.mrconfig"));
        });
    }

    #[test]
    fn a_stored_set_that_no_longer_resolves_is_dropped_silently() {
        with_config_home(|_| {
            let cli = Cli::parse_from(["mrx", "ui"]);
            let session = ui::app::session::Session {
                set: Some("gone".into()),
                ..Default::default()
            };
            assert_eq!(restored_set(&cli, &session), None);
        });
    }
}
