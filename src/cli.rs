use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "mrx",
    about = "Multi Repo eXtreme — parallel multi-repo operations with TUI"
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

    /// Config file (default: ~/.mrconfig)
    #[arg(short = 'c', long, global = true)]
    pub config: Option<PathBuf>,

    /// Max parallel jobs (default: min(cpus, 8))
    #[arg(short = 'j', long, global = true)]
    pub jobs: Option<usize>,

    /// Don't recurse into subdirectories
    #[arg(short = 'n', long = "no-recurse", global = true)]
    pub no_recurse: bool,

    /// Force operation
    #[arg(short = 'f', long, global = true)]
    pub force: bool,
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
    /// List configured repos
    List,
    /// Alias for list
    Ls,
}

impl Command {
    pub fn display_name(&self) -> &'static str {
        match self {
            Command::Update | Command::Pull => "update",
            Command::Status => "status",
            Command::Diff => "diff",
            Command::Push => "push",
            Command::Fetch => "fetch",
            Command::Checkout | Command::Co => "checkout",
            Command::Run { .. } => "run",
            Command::List | Command::Ls => "list",
        }
    }

    pub fn is_list(&self) -> bool {
        matches!(self, Command::List | Command::Ls)
    }
}
