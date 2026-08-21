//! Line-per-repo output for runs with nobody watching.
//!
//! The alternate screen the TUI draws into only corrupts a log file, and there is
//! no one there to press `q`, so a non-TTY run comes here instead. Returns true
//! when every repo succeeded, matching `ui::run::run`.

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
            // Progress has nowhere to go in a line-per-repo report, and `Output`
            // never arrives at all: the plain path opts out of streaming.
            TaskEvent::Started { .. } | TaskEvent::Step { .. } | TaskEvent::Output { .. } => {}
            TaskEvent::Skipped { index, reason } => {
                done += 1;
                println!("{:width$} | skipped: {}", repos[index].name, reason);
            }
            TaskEvent::Finished {
                index,
                steps,
                exit_code,
            } => {
                done += 1;
                let summary = summarize::summarize_steps(&steps, exit_code);
                if exit_code == 0 {
                    println!("{:width$} | {}", repos[index].name, summary);
                } else {
                    failed += 1;
                    println!(
                        "{:width$} | FAILED ({}): {}",
                        repos[index].name, exit_code, summary
                    );
                    // Enough detail to diagnose without re-running, stripped
                    // because this path is routinely redirected to a file. A
                    // one-line failure adds nothing over the summary above.
                    let detail: Vec<String> = steps
                        .iter()
                        .flat_map(|s| s.stdout.lines().chain(s.stderr.lines()))
                        .map(crate::ansi::strip)
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
        eprintln!("mrx {action}: {done}/{total} done, {failed} failed");
    } else {
        eprintln!("mrx {action}: {done}/{total} done");
    }

    failed == 0
}
