//! Line-per-repo output for runs with nobody watching.
//!
//! The alternate screen the TUI draws into only corrupts a log file, and there is
//! no one there to press `q`, so a non-TTY run comes here instead. Returns true
//! when every repo succeeded, matching `tui::run`.

use crate::config::Repo;
use crate::executor::TaskEvent;
use crate::summarize;
use tokio::sync::mpsc;

/// Longest repo name to pad to. Past this, alignment costs more than it buys.
const MAX_LABEL: usize = 28;

pub async fn run(
    repos: Vec<Repo>,
    action: &str,
    mut rx: mpsc::UnboundedReceiver<TaskEvent>,
) -> bool {
    let width = repos
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(MAX_LABEL);

    let total = repos.len();
    let mut done = 0usize;
    let mut failed = 0usize;

    while done < total {
        let Some(event) = rx.recv().await else { break };

        match event {
            TaskEvent::Started { .. } => continue,
            TaskEvent::Skipped { index, reason } => {
                done += 1;
                println!("{:width$} | skipped: {}", repos[index].name, reason);
            }
            TaskEvent::Finished {
                index,
                stdout,
                stderr,
                exit_code,
                failed_step,
            } => {
                done += 1;
                let summary = summarize::with_step(
                    failed_step.as_deref(),
                    summarize::summarize(action, &stdout, &stderr, exit_code),
                );
                if exit_code == 0 {
                    println!("{:width$} | {}", repos[index].name, summary);
                } else {
                    failed += 1;
                    println!(
                        "{:width$} | FAILED ({}): {}",
                        repos[index].name, exit_code, summary
                    );
                    // A summary is enough to spot the failure; the log needs enough
                    // to diagnose it without re-running. One-line failures are
                    // already fully described by the summary above.
                    let detail: Vec<&str> = stdout
                        .lines()
                        .chain(stderr.lines())
                        .filter(|l| !l.trim().is_empty())
                        .collect();
                    if detail.len() > 1 {
                        for line in detail {
                            println!("{:width$} |   {}", "", line);
                        }
                    }
                }
            }
        }
    }

    if failed > 0 {
        eprintln!("mrx {}: {}/{} done, {} failed", action, done, total, failed);
    } else {
        eprintln!("mrx {}: {}/{} done", action, done, total);
    }

    failed == 0
}
