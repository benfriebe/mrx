use crate::config::Repo;
use crate::operations::Operation;
use crate::summarize::Shape;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::{mpsc, Semaphore};

#[derive(Debug, Clone)]
pub enum TaskEvent {
    Started {
        index: usize,
    },
    /// Emitted as each step begins, so a row can report what it is doing
    /// rather than which action it belongs to.
    Step {
        index: usize,
        label: String,
    },
    Finished {
        index: usize,
        steps: Vec<StepResult>,
        exit_code: i32,
    },
    Skipped {
        index: usize,
        reason: String,
    },
}

/// One step's output, labelled and shaped by `operations::plan`, the only
/// place that knows whether it was a built-in git call, a config-defined
/// body, or a `post_` hook.
#[derive(Debug, Clone)]
pub struct StepResult {
    pub label: String,
    pub shape: Shape,
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

/// A live run: the flag `spawn_run` checks to skip queued work, the id every
/// event it emits is tagged with, and how many targets it covers.
pub struct RunHandle {
    cancel: Arc<AtomicBool>,
    pub run_id: u64,
    pub total: usize,
}

impl RunHandle {
    /// Stop every repo still queued from starting. Repos already past their
    /// semaphore permit keep running to completion; see `spawn_run`.
    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// One executor event, tagged with the run it belongs to so a receiver can
/// drop events from a run that has since been cancelled and superseded
/// rather than painting them over a newer run's results.
pub struct RunEvent {
    pub run_id: u64,
    pub kind: TaskEvent,
}

/// Per-repo context every step of a chain shares.
struct StepContext {
    repo_name: String,
    repo_path: PathBuf,
    config_path: Arc<PathBuf>,
}

struct StepOutput {
    stdout: String,
    stderr: String,
    code: i32,
}

/// Spawn a run over `targets`, pairs of global repo index and planned
/// operation. Events carry the global index so a subset run attributes
/// output to the right repo, and are tagged with `run_id` so a cancelled
/// run's in-flight events don't paint over a newer one's.
pub fn spawn_run(
    repos: &[Repo],
    targets: Vec<(usize, Operation)>,
    max_jobs: usize,
    config_path: PathBuf,
    tx: mpsc::UnboundedSender<RunEvent>,
    run_id: u64,
) -> RunHandle {
    let semaphore = Arc::new(Semaphore::new(max_jobs));
    let config_path = Arc::new(config_path);
    let cancel = Arc::new(AtomicBool::new(false));
    let total = targets.len();

    for (index, op) in targets {
        let Some(repo) = repos.get(index) else {
            continue;
        };
        let tx = tx.clone();
        let sem = semaphore.clone();
        let cancel = cancel.clone();
        let ctx = StepContext {
            repo_name: repo.name.clone(),
            repo_path: repo.path.clone(),
            config_path: config_path.clone(),
        };

        tokio::spawn(async move {
            let send = |kind| {
                let _ = tx.send(RunEvent { run_id, kind });
            };

            match op {
                Operation::Skip { reason } => send(TaskEvent::Skipped { index, reason }),
                Operation::NotCheckedOut => send(TaskEvent::Skipped {
                    index,
                    reason: "not checked out".into(),
                }),
                runnable => {
                    // Stage A queue cancellation: checked once before queuing and
                    // again once the permit is granted, since the permit is where
                    // the waiting happens. A repo already past this point runs to
                    // completion; killing it is stage B (section 06).
                    if cancel.load(Ordering::Relaxed) {
                        send(TaskEvent::Skipped {
                            index,
                            reason: "cancelled".into(),
                        });
                        return;
                    }
                    let _permit = sem.acquire().await.unwrap();
                    if cancel.load(Ordering::Relaxed) {
                        send(TaskEvent::Skipped {
                            index,
                            reason: "cancelled".into(),
                        });
                        return;
                    }

                    send(TaskEvent::Started { index });

                    let steps = match runnable {
                        Operation::Sequence(steps) => steps,
                        single => vec![single],
                    };

                    let mut results = Vec::new();
                    let mut code = 0;
                    for step in steps {
                        let label = step.label();
                        let shape = step.shape();
                        send(TaskEvent::Step {
                            index,
                            label: label.clone(),
                        });
                        let out = run_step(step, &ctx).await;
                        code = out.code;
                        results.push(StepResult {
                            label,
                            shape,
                            stdout: out.stdout,
                            stderr: out.stderr,
                            code: out.code,
                        });
                        if out.code != 0 {
                            // Keep the steps collected so far; stop the chain.
                            break;
                        }
                    }

                    send(TaskEvent::Finished {
                        index,
                        steps: results,
                        exit_code: code,
                    });
                }
            }
        });
    }

    RunHandle {
        cancel,
        run_id,
        total,
    }
}

/// The one-shot CLI path keeps its old shape: make a channel, run
/// everything, and hand back a receiver that closes once the run is done.
///
/// It must not share `spawn_run`'s channel design with the resident app:
/// `render_plain.rs` loops until the channel closes, which only happens
/// once every sender has dropped, and the app holds a sender for its whole
/// life.
pub fn execute_all(
    repos: &[Repo],
    operations: Vec<Operation>,
    max_jobs: usize,
    config_path: PathBuf,
) -> mpsc::UnboundedReceiver<TaskEvent> {
    let targets: Vec<(usize, Operation)> = operations.into_iter().enumerate().collect();
    let (run_tx, mut run_rx) = mpsc::unbounded_channel();
    spawn_run(repos, targets, max_jobs, config_path, run_tx, 0);

    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(event) = run_rx.recv().await {
            if tx.send(event.kind).is_err() {
                break;
            }
        }
    });
    rx
}

async fn run_step(op: Operation, ctx: &StepContext) -> StepOutput {
    let mut command = match &op {
        Operation::Git { args, work_dir } => {
            let mut c = Command::new("git");
            c.args(args).current_dir(work_dir);
            c
        }
        Operation::Clone { url, dest } => {
            let parent = dest.parent().unwrap_or(dest);
            let _ = tokio::fs::create_dir_all(parent).await;
            let mut c = Command::new("git");
            c.args(["clone", url, &dest.to_string_lossy()]);
            c
        }
        Operation::Shell {
            cmd,
            work_dir,
            action,
            args,
            env,
        } => {
            // sh -e -c '<body>' mrx <arg1> <arg2> ...
            // exposes the args as $1, $2 inside the body, and $0 as "mrx".
            //
            // `-e` because a body is usually a list of commands, and without it only
            // the last one's exit code survives: a body that fails to pull and then
            // succeeds at building reports success. `||` still tolerates a failure
            // for the commands that are meant to be allowed to fail.
            let mut sh_args: Vec<String> =
                vec!["-e".into(), "-c".into(), cmd.clone(), "mrx".into()];
            sh_args.extend(args.iter().cloned());

            let mut c = Command::new("sh");
            c.args(&sh_args)
                .current_dir(work_dir)
                // Config-derived vars first, so the fixed four always win.
                .envs(env.iter().map(|(k, v)| (k.clone(), v.clone())))
                .env("MR_REPO", &ctx.repo_path)
                .env("MR_REPONAME", &ctx.repo_name)
                .env("MR_CONFIG", ctx.config_path.as_path())
                .env("MR_ACTION", action);
            c
        }
        Operation::Sequence(_) | Operation::Skip { .. } | Operation::NotCheckedOut => {
            // Sequences are flattened by the caller and the other two never reach here.
            return StepOutput {
                stdout: String::new(),
                stderr: String::new(),
                code: 0,
            };
        }
    };

    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    match command.output().await {
        Ok(output) => StepOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            code: output.status.code().unwrap_or(1),
        },
        Err(e) => StepOutput {
            stdout: String::new(),
            stderr: format!("failed to execute: {}", e),
            code: 1,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn run_body(cmd: &str) -> StepOutput {
        let dir = tempfile::tempdir().unwrap();
        let ctx = StepContext {
            repo_name: "r".into(),
            repo_path: dir.path().to_path_buf(),
            config_path: Arc::new(PathBuf::from("/dev/null")),
        };
        run_step(
            Operation::Shell {
                cmd: cmd.into(),
                work_dir: dir.path().to_path_buf(),
                action: "update".into(),
                args: vec![],
                env: vec![],
            },
            &ctx,
        )
        .await
    }

    #[tokio::test]
    async fn a_failed_command_ends_the_body() {
        // The shape that started this: pull, install, build. Without `-e` the failed
        // pull is masked by the build that follows it and the repo reports success.
        let out = run_body("echo 'error: cannot pull' >&2; false\necho built").await;
        assert_ne!(out.code, 0, "a failed line has to fail the body");
        assert!(!out.stdout.contains("built"), "got {:?}", out.stdout);
    }

    #[tokio::test]
    async fn a_failure_the_body_handles_itself_is_still_allowed() {
        let out = run_body("false || true\necho built").await;
        assert_eq!(out.code, 0);
        assert!(out.stdout.contains("built"));
    }

    #[tokio::test]
    async fn positional_args_still_reach_the_body() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = StepContext {
            repo_name: "r".into(),
            repo_path: dir.path().to_path_buf(),
            config_path: Arc::new(PathBuf::from("/dev/null")),
        };
        let out = run_step(
            Operation::Shell {
                cmd: "echo \"$0 $1\"".into(),
                work_dir: dir.path().to_path_buf(),
                action: "update".into(),
                args: vec!["--offline".into()],
                env: vec![],
            },
            &ctx,
        )
        .await;
        assert_eq!(out.stdout.trim(), "mrx --offline");
    }

    fn repos(names: &[&str], dir: &std::path::Path) -> Vec<Repo> {
        names
            .iter()
            .map(|n| Repo {
                name: (*n).to_string(),
                path: dir.to_path_buf(),
                clone_url: None,
                keys: Default::default(),
            })
            .collect()
    }

    fn noop_at(dir: &std::path::Path, action: &str) -> Operation {
        Operation::Shell {
            cmd: "true".into(),
            work_dir: dir.to_path_buf(),
            action: action.into(),
            args: vec![],
            env: vec![],
        }
    }

    #[tokio::test]
    async fn a_subset_run_attributes_events_to_the_global_repo_index() {
        let dir = tempfile::tempdir().unwrap();
        let names: Vec<String> = (0..10).map(|i| format!("r{i}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let repos = repos(&names, dir.path());

        let targets = vec![
            (3, noop_at(dir.path(), "noop")),
            (7, noop_at(dir.path(), "noop")),
        ];

        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_run(&repos, targets, 4, PathBuf::from("/dev/null"), tx, 1);

        let mut finished_indices = Vec::new();
        while let Some(evt) = rx.recv().await {
            if let TaskEvent::Finished { index, .. } = evt.kind {
                finished_indices.push(index);
                if finished_indices.len() == 2 {
                    break;
                }
            }
        }
        finished_indices.sort();
        assert_eq!(
            finished_indices,
            vec![3, 7],
            "events must carry the global repo index, not a position in the target list"
        );
    }

    #[tokio::test]
    async fn step_labels_arrive_in_order_for_a_three_step_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let repos = repos(&["r"], dir.path());
        let seq = Operation::Sequence(vec![
            noop_at(dir.path(), "one"),
            noop_at(dir.path(), "two"),
            noop_at(dir.path(), "three"),
        ]);

        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_run(&repos, vec![(0, seq)], 1, PathBuf::from("/dev/null"), tx, 1);

        let mut labels = Vec::new();
        while let Some(evt) = rx.recv().await {
            match evt.kind {
                TaskEvent::Step { label, .. } => labels.push(label),
                TaskEvent::Finished { .. } => break,
                _ => {}
            }
        }
        assert_eq!(labels, vec!["one", "two", "three"]);
    }
}
