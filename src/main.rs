mod cli;
mod config;
mod executor;
mod operations;
mod summarize;
mod tui;

use clap::Parser;
use cli::{Cli, Command};
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

fn resolve_config_path(cli: &Cli) -> PathBuf {
    let raw = if let Some(ref p) = cli.config {
        p.clone()
    } else {
        dirs::home_dir()
            .expect("cannot determine home directory")
            .join(".mrconfig")
    };
    absolutize(&raw)
}

fn max_jobs(cli: &Cli) -> usize {
    cli.jobs.unwrap_or_else(|| num_cpus::get().min(8))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let config_path = resolve_config_path(&cli);
    let dir_override = cli.directory.as_deref().map(absolutize);
    let config::Config {
        repos,
        defaults,
        base,
    } = config::load(&config_path, dir_override.as_deref());

    // Register command: add current dir to config
    if cli.command.is_register() {
        register(&config_path, &base);
        return;
    }

    // List command: just print and exit
    if cli.command.is_list() {
        for repo in &repos {
            let exists = repo.path.is_dir();
            let marker = if exists { "✓" } else { "-" };
            println!("{} {:24} {}", marker, repo.name, repo.path.display());
        }
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

    // Plan operations
    let ops: Vec<operations::Operation> = repos
        .iter()
        .map(|r| operations::plan(&cli.command, r, &defaults))
        .collect();

    // Execute
    let jobs = max_jobs(&cli);
    let rx = executor::execute_all(&repos, ops, jobs, config_path.clone());

    // Run TUI
    let success = tui::run(repos, &cli.command, rx).expect("TUI error");

    std::process::exit(if success { 0 } else { 1 });
}

fn register(config_path: &PathBuf, base_dir: &PathBuf) {
    let cwd = std::env::current_dir().expect("cannot determine current directory");

    // Get the remote URL
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

    // Compute relative section path from base_dir
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

    // Check if already registered
    let existing = std::fs::read_to_string(config_path).unwrap_or_default();
    let section_header = format!("[{}]", section);
    if existing.contains(&section_header) {
        eprintln!("already registered: {}", section);
        return;
    }

    // Append to config
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
