mod cli;
mod config;
mod executor;
mod operations;
mod summarize;
mod tui;

use clap::Parser;
use cli::Cli;
use std::path::PathBuf;

fn resolve_config_path(cli: &Cli) -> PathBuf {
    if let Some(ref p) = cli.config {
        p.clone()
    } else {
        dirs::home_dir()
            .expect("cannot determine home directory")
            .join(".mrconfig")
    }
}

fn resolve_base_dir(cli: &Cli, config_path: &PathBuf) -> PathBuf {
    if let Some(ref d) = cli.directory {
        d.clone()
    } else {
        config_path
            .parent()
            .expect("config has no parent dir")
            .to_path_buf()
    }
}

fn max_jobs(cli: &Cli) -> usize {
    cli.jobs.unwrap_or_else(|| num_cpus::get().min(8))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let config_path = resolve_config_path(&cli);
    let base_dir = resolve_base_dir(&cli, &config_path);
    let repos = config::parse_config(&config_path, &base_dir);

    // List command: just print and exit
    if cli.command.is_list() {
        for repo in &repos {
            let exists = repo.path.is_dir();
            let marker = if exists { "✓" } else { "-" };
            println!("{} {:24} {}", marker, repo.name, repo.path.display());
        }
        return;
    }

    // Plan operations
    let ops: Vec<operations::Operation> = repos
        .iter()
        .map(|r| operations::plan(&cli.command, r))
        .collect();

    // Execute
    let jobs = max_jobs(&cli);
    let rx = executor::execute_all(&repos, ops, jobs);

    // Run TUI
    let success = tui::run(repos, &cli.command, rx).expect("TUI error");

    std::process::exit(if success { 0 } else { 1 });
}
