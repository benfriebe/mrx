use clap::Parser;
use mrx::cli::{Cli, Command};
use mrx::{config, executor, operations, render_plain, sets, ui};
use std::io::{stdout, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
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
            eprintln!("error: no config for set '{}'. Looked in:", name);
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
        let dir = sets::config_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_else(|| "~/.config/mrx".into());
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
fn restored_set(cli: &Cli, session: &ui::app::session::Session) -> Option<String> {
    if cli.config.is_some() || named_set(cli).is_some() {
        return None;
    }
    session
        .set
        .as_deref()
        .filter(|name| sets::resolve(name).is_some())
        .map(str::to_string)
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
        Some(name) => {
            absolutize(&sets::resolve(name).expect("restored_set already confirmed this resolves"))
        }
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
        let label = ui_set_label(&cli, ui_restored_set.as_deref());
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
        let name = parts.first().map(String::as_str).unwrap_or("");
        let known = !name.is_empty()
            && (defaults.contains_key(name) || repos.iter().any(|r| r.keys.contains_key(name)));
        if !known {
            eprintln!(
                "error: unknown action '{}' (not defined in any repo or [DEFAULT])",
                name
            );
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
            config_path.clone(),
            cli.exit_on_done,
        )
        .expect("TUI error")
    } else {
        render_plain::run(repos, cli.command.display_name(), rx).await
    };

    std::process::exit(if success { 0 } else { 1 });
}

fn register(config_path: &PathBuf, base_dir: &PathBuf) {
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

    let section = match cwd.strip_prefix(base_dir) {
        Ok(rel) => rel.to_string_lossy().to_string(),
        Err(_) => {
            eprintln!(
                "error: {} is not under base dir {}",
                cwd.display(),
                base_dir.display()
            );
            std::process::exit(1);
        }
    };

    let existing = std::fs::read_to_string(config_path).unwrap_or_default();
    let section_header = format!("[{}]", section);
    if existing.contains(&section_header) {
        eprintln!("already registered: {}", section);
        return;
    }

    let entry = format!(
        "\n[{}]\ncheckout = git clone '{}' '{}'\n",
        section, url, repo_name
    );

    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(config_path)
        .unwrap_or_else(|e| {
            eprintln!("error: cannot write {}: {}", config_path.display(), e);
            std::process::exit(1);
        });

    file.write_all(entry.as_bytes()).unwrap_or_else(|e| {
        eprintln!("error: write failed: {}", e);
        std::process::exit(1);
    });

    println!("registered {} ({})", section, url);
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// `XDG_CONFIG_HOME` is process-global; tests that point it at their own
    /// tempdir still need to serialise against each other.
    fn with_config_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", dir.path());
        let result = f(dir.path());
        match previous {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
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
            assert_eq!(restored_set(&cli, &session).as_deref(), Some("work"));
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
