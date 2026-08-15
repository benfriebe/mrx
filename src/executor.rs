use crate::config::Repo;
use crate::operations::Operation;
use crate::summarize::Shape;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;
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
    /// One line of a step's output, as it arrives. `Finished` still carries
    /// the whole transcript and remains the record of what a step produced;
    /// this only exists so a long run can be read while it runs instead of
    /// waited out.
    Output {
        index: usize,
        /// Position in the step chain, so a line lands under the right
        /// heading when several steps have already scrolled past.
        step: usize,
        stderr: bool,
        line: String,
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

/// A live run: the flag `spawn_run` checks to skip queued work.
pub struct RunHandle {
    cancel: Arc<AtomicBool>,
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

/// Where a step's output goes line by line while it is still running.
/// `None` for the one-shot CLI path, which prints once at the end and has
/// nothing to do with a line until then.
struct StepSink {
    tx: mpsc::UnboundedSender<RunEvent>,
    run_id: u64,
    index: usize,
    step: usize,
}

impl StepSink {
    fn line(&self, stderr: bool, line: &str) {
        let _ = self.tx.send(RunEvent {
            run_id: self.run_id,
            kind: TaskEvent::Output {
                index: self.index,
                step: self.step,
                stderr,
                line: line.to_string(),
            },
        });
    }
}

/// Spawn a run over `targets`, pairs of global repo index and planned
/// operation. Events carry the global index so a subset run attributes
/// output to the right repo, and are tagged with `run_id` so a cancelled
/// run's in-flight events don't paint over a newer one's.
///
/// `stream` adds a [`TaskEvent::Output`] per line as it is produced, for a
/// caller that shows a run while it runs. A caller that only prints at the
/// end passes `false` rather than filtering thousands of events it will
/// never look at.
pub fn spawn_run(
    repos: &[Repo],
    targets: Vec<(usize, Operation)>,
    max_jobs: usize,
    config_path: PathBuf,
    tx: mpsc::UnboundedSender<RunEvent>,
    run_id: u64,
    stream: bool,
) -> RunHandle {
    let semaphore = Arc::new(Semaphore::new(max_jobs));
    let config_path = Arc::new(config_path);
    let cancel = Arc::new(AtomicBool::new(false));

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
                    for (position, step) in steps.into_iter().enumerate() {
                        let label = step.label();
                        let shape = step.shape();
                        send(TaskEvent::Step {
                            index,
                            label: label.clone(),
                        });
                        let sink = stream.then(|| StepSink {
                            tx: tx.clone(),
                            run_id,
                            index,
                            step: position,
                        });
                        let out = run_step(step, &ctx, sink.as_ref()).await;
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

    RunHandle { cancel }
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
    spawn_run(repos, targets, max_jobs, config_path, run_tx, 0, false);

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

async fn run_step(op: Operation, ctx: &StepContext, sink: Option<&StepSink>) -> StepOutput {
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

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return StepOutput {
                stdout: String::new(),
                stderr: format!("failed to execute: {e}"),
                code: 1,
            }
        }
    };

    // Both pipes are drained concurrently: a step that fills one while the
    // reader is blocked on the other deadlocks against the pipe buffer.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (out, err) = tokio::join!(drain(stdout, false, sink), drain(stderr, true, sink));

    let code = match child.wait().await {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            return StepOutput {
                stdout: out,
                stderr: format!("{err}failed to wait on the process: {e}"),
                code: 1,
            }
        }
    };

    StepOutput {
        stdout: out,
        stderr: err,
        code,
    }
}

/// Read one pipe to EOF, handing each line to `sink` as it arrives and
/// returning the whole thing for the step's own record.
async fn drain<R>(pipe: Option<R>, stderr: bool, sink: Option<&StepSink>) -> String
where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(pipe) = pipe else {
        return String::new();
    };
    let mut lines = tokio::io::BufReader::new(pipe).lines();
    let mut text = String::new();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(sink) = sink {
            sink.line(stderr, &line);
        }
        text.push_str(&line);
        text.push('\n');
    }
    text
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
            None,
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
            None,
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
        spawn_run(&repos, targets, 4, PathBuf::from("/dev/null"), tx, 1, false);

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
        spawn_run(
            &repos,
            vec![(0, seq)],
            1,
            PathBuf::from("/dev/null"),
            tx,
            1,
            false,
        );

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

    /// The point of streaming: a line has to be readable while the step is
    /// still running, so it must arrive before that step's `Finished`.
    #[tokio::test]
    async fn output_lines_arrive_before_the_step_that_produced_them_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let repos = repos(&["r"], dir.path());
        let op = shell_at(dir.path(), "echo first\nsleep 0.2\necho second");

        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_run(
            &repos,
            vec![(0, op)],
            1,
            PathBuf::from("/dev/null"),
            tx,
            1,
            true,
        );

        let mut streamed = Vec::new();
        let mut finished = None;
        while let Some(evt) = rx.recv().await {
            match evt.kind {
                TaskEvent::Output { line, .. } => streamed.push(line),
                TaskEvent::Finished { steps, .. } => {
                    finished = Some(steps);
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(streamed, vec!["first", "second"]);
        let steps = finished.expect("the run finished");
        assert_eq!(
            steps[0].stdout, "first\nsecond\n",
            "the finished result still carries the whole thing"
        );
    }

    #[tokio::test]
    async fn a_caller_that_did_not_ask_for_streaming_gets_no_output_events() {
        let dir = tempfile::tempdir().unwrap();
        let repos = repos(&["r"], dir.path());
        let op = shell_at(dir.path(), "echo hello");

        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_run(
            &repos,
            vec![(0, op)],
            1,
            PathBuf::from("/dev/null"),
            tx,
            1,
            false,
        );

        while let Some(evt) = rx.recv().await {
            match evt.kind {
                TaskEvent::Output { .. } => panic!("streaming was not asked for"),
                TaskEvent::Finished { .. } => break,
                _ => {}
            }
        }
    }

    /// Both pipes are read concurrently, so a step that fills one while
    /// writing nothing to the other still completes rather than blocking on
    /// a full pipe buffer.
    #[tokio::test]
    async fn a_step_that_floods_one_pipe_still_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let repos = repos(&["r"], dir.path());
        let op = shell_at(dir.path(), "seq 1 20000 >&2");

        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_run(
            &repos,
            vec![(0, op)],
            1,
            PathBuf::from("/dev/null"),
            tx,
            1,
            false,
        );

        let mut code = None;
        while let Some(evt) = rx.recv().await {
            if let TaskEvent::Finished { exit_code, .. } = evt.kind {
                code = Some(exit_code);
                break;
            }
        }
        assert_eq!(code, Some(0));
    }

    fn shell_at(dir: &std::path::Path, cmd: &str) -> Operation {
        Operation::Shell {
            cmd: cmd.into(),
            work_dir: dir.to_path_buf(),
            action: "noop".into(),
            args: vec![],
            env: vec![],
        }
    }

    /// Stage A (section 06): cancelling a run stops everything still queued
    /// behind the job limit, but a repo already past its semaphore permit
    /// runs to completion, since `Command::output().await` has no kill.
    #[tokio::test]
    async fn cancelling_a_run_skips_queued_targets_but_finishes_the_one_already_running() {
        let dir = tempfile::tempdir().unwrap();
        let repos = repos(&["r0", "r1", "r2"], dir.path());
        // Long enough that the other two are still waiting on the single
        // permit by the time the test calls `request_cancel`.
        let targets = vec![
            (0, shell_at(dir.path(), "sleep 0.2")),
            (1, shell_at(dir.path(), "true")),
            (2, shell_at(dir.path(), "true")),
        ];

        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = spawn_run(&repos, targets, 1, PathBuf::from("/dev/null"), tx, 1, false);

        // Whichever target wins the single permit first; the test doesn't
        // care which index that is, only that cancelling now must let it
        // finish while the still-queued other two are skipped.
        let mut winner = None;
        while let Some(evt) = rx.recv().await {
            if let TaskEvent::Started { index } = evt.kind {
                winner = Some(index);
                break;
            }
        }
        let winner = winner.expect("one target must have started");
        handle.request_cancel();

        let mut finished = Vec::new();
        let mut skipped = Vec::new();
        while finished.len() + skipped.len() < 3 {
            match rx
                .recv()
                .await
                .expect("run must report on every target")
                .kind
            {
                TaskEvent::Finished { index, .. } => finished.push(index),
                TaskEvent::Skipped { index, reason } => skipped.push((index, reason)),
                _ => {}
            }
        }

        assert_eq!(
            finished,
            vec![winner],
            "the target already past its permit must run to completion"
        );
        let mut skipped_indices: Vec<usize> = skipped.iter().map(|(i, _)| *i).collect();
        skipped_indices.sort();
        let mut expected: Vec<usize> = (0..3).filter(|i| *i != winner).collect();
        expected.sort();
        assert_eq!(skipped_indices, expected);
        for (_, reason) in &skipped {
            assert_eq!(reason, "cancelled");
        }
    }
}
