use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;

/// Rejects `-j 0`: it becomes `Semaphore::new(0)` downstream, which never
/// grants a permit, so every probe, run, and poll waits forever with no
/// error and no visible cause.
fn parse_jobs(s: &str) -> Result<usize, String> {
    let n: usize = s.parse().map_err(|_| format!("`{s}` is not a number"))?;
    if n == 0 {
        return Err("must be at least 1 (0 jobs can never run anything)".to_string());
    }
    Ok(n)
}

#[derive(Parser)]
#[command(
    name = "mrx",
    about = "Multi Repo eXtreme: parallel multi-repo operations with TUI"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Working directory (default: config file's parent)
    #[arg(short = 'd', long, global = true)]
    pub directory: Option<PathBuf>,

    /// Config file. Overrides --set.
    #[arg(short = 'c', long, global = true)]
    pub config: Option<PathBuf>,

    /// Named repo set: ~/.config/mrx/NAME.mrconfig [env: MRX_SET]
    #[arg(short = 's', long, global = true)]
    pub set: Option<String>,

    /// Max parallel jobs (default: min(cpus, 8))
    #[arg(short = 'j', long, global = true, value_parser = parse_jobs)]
    pub jobs: Option<usize>,

    /// Don't recurse into subdirectories
    #[arg(short = 'n', long = "no-recurse", global = true)]
    pub no_recurse: bool,

    /// Force operation
    #[arg(short = 'f', long, global = true)]
    pub force: bool,

    /// Quit once every repo has finished, instead of waiting for `q`. Ignored
    /// by `ui`, which has no single run to wait on.
    #[arg(long, global = true)]
    pub exit_on_done: bool,

    /// Never use the TUI, even on a terminal
    #[arg(long, global = true)]
    pub plain: bool,

    /// How long `ui` keeps a run's result on its row: `6m`, `90s`, or `off`
    /// to keep it until the next run (default: 6m)
    #[arg(long, global = true, value_parser = parse_duration)]
    pub result_ttl: Option<Duration>,
}

/// A short duration for `--result-ttl`: a bare number of seconds, `90s`,
/// `6m`, `1h`, or `off`/`0`.
///
/// `off` parses to [`Duration::ZERO`], not `None`: clap reads `None` as "the
/// flag was not passed", which falls back to the default instead of turning
/// expiry off. `main.rs` maps the zero back to "never expire".
pub(crate) fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("off") {
        return Ok(Duration::ZERO);
    }
    let (digits, scale) = [('s', 1), ('m', 60), ('h', 3600)]
        .into_iter()
        .find_map(|(suffix, scale)| {
            s.strip_suffix([suffix, suffix.to_ascii_uppercase()])
                .map(|rest| (rest, scale))
        })
        .unwrap_or((s, 1));
    let n: u64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("`{s}` is not a duration like 90s, 6m, 1h, or off"))?;
    let secs = n
        .checked_mul(scale)
        .ok_or_else(|| format!("`{s}` is longer than any session will last"))?;
    Ok(Duration::from_secs(secs))
}

#[derive(Subcommand, Clone)]
pub enum Command {
    /// Pull latest changes (clone if missing)
    Update,
    /// Alias for update
    Pull,
    /// Show working tree status
    Status,
    /// Show diffs
    Diff,
    /// Push commits
    Push,
    /// Fetch from remotes
    Fetch,
    /// Clone repos (skip if exists)
    Checkout,
    /// Alias for checkout
    Co,
    /// Run an arbitrary command in each repo
    Run {
        /// Command to run
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Register current repo in config
    Register,
    /// List configured repos
    List,
    /// Alias for list
    Ls,
    /// List named repo sets
    Sets,
    /// Open ui mode: browse a set, select repos, and run actions
    /// without leaving the screen
    Ui,
    /// Any subcommand defined in .mrconfig (per-repo or [DEFAULT])
    #[command(external_subcommand)]
    Custom(Vec<String>),
}

impl Command {
    pub fn display_name(&self) -> &str {
        match self {
            Command::Update | Command::Pull => "update",
            Command::Status => "status",
            Command::Diff => "diff",
            Command::Push => "push",
            Command::Fetch => "fetch",
            Command::Checkout | Command::Co => "checkout",
            Command::Run { .. } => "run",
            Command::Register => "register",
            Command::List | Command::Ls => "list",
            Command::Sets => "sets",
            Command::Ui => "ui",
            Command::Custom(args) => args.first().map(String::as_str).unwrap_or(""),
        }
    }

    pub fn is_list(&self) -> bool {
        matches!(self, Command::List | Command::Ls)
    }

    pub fn is_register(&self) -> bool {
        matches!(self, Command::Register)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dash_j_zero_is_rejected_at_parse_time() {
        let result = Cli::try_parse_from(["mrx", "-j", "0", "status"]);
        let Err(err) = result else {
            panic!("-j 0 must be rejected, not accepted");
        };
        assert!(err.to_string().contains("at least 1"), "got {err}");
    }

    #[test]
    fn dash_j_with_a_positive_count_is_accepted() {
        let cli = Cli::try_parse_from(["mrx", "-j", "4", "status"]).unwrap();
        assert_eq!(cli.jobs, Some(4));
    }

    #[test]
    fn jobs_defaults_to_none_when_dash_j_is_absent() {
        let cli = Cli::try_parse_from(["mrx", "status"]).unwrap();
        assert_eq!(cli.jobs, None);
    }

    #[test]
    fn result_ttl_accepts_seconds_minutes_and_hours() {
        assert_eq!(parse_duration("90"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_duration("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_duration("6m"), Ok(Duration::from_secs(360)));
        assert_eq!(parse_duration("1h"), Ok(Duration::from_secs(3600)));
    }

    /// The distinction `parse_duration` documents, from clap's side.
    #[test]
    fn result_ttl_off_is_not_the_same_as_an_absent_flag() {
        let off = Cli::try_parse_from(["mrx", "--result-ttl", "off", "ui"]).unwrap();
        assert_eq!(off.result_ttl, Some(Duration::ZERO));

        let absent = Cli::try_parse_from(["mrx", "ui"]).unwrap();
        assert_eq!(absent.result_ttl, None);
    }

    #[test]
    fn result_ttl_rejects_something_that_is_not_a_duration() {
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("").is_err());
    }
}
