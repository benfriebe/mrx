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

/// One event from a live run, about one of its targets.
///
/// The stream is a grammar rather than a bag of events, and its consumers fold
/// it on that basis:
///
/// - Every target index reports exactly one terminal event: a lone `Skipped`,
///   or a `Finished` closing a chain that opened with `Started`. This holds
///   unconditionally, an index naming no repo included, since counting
///   terminal events is how a consumer knows the run is over.
/// - Within a chain, each step sends its `Step`, then any `Output` it
///   produces, before the next step's `Step`. Ordering is per index only:
///   targets run concurrently and their events interleave.
/// - `Output.step` is the zero-based ordinal of the step that produced the
///   line, so it lines up with the `Step` events already sent for that index.
///   A consumer indexing a per-step list by it drops anything that doesn't.
/// - A chain stops at the first step to exit non-zero, so the last entry in
///   `Finished.steps` is the one that decided the outcome.
/// - `Finished.steps` supersedes whatever was streamed for that index.
/// - `stream: false` in [`spawn_run`] removes every `Output` and nothing else.
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
    /// One line of a step's output, as it arrives, so a long run can be read
    /// while it runs. `Finished` remains the record of what a step produced.
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

/// One step's output, labelled and shaped by `operations::plan`.
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
    /// semaphore permit keep running to completion.
    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// One executor event, tagged with the run it belongs to so a receiver can
/// drop a superseded run's in-flight events rather than painting them over a
/// newer run's results.
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

/// Where a step's output goes line by line while it is still running. `None`
/// for the one-shot CLI path, which only prints at the end.
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
/// `stream` adds a [`TaskEvent::Output`] per line as it is produced. A caller
/// that only prints at the end passes `false` rather than filtering thousands
/// of events it will never look at.
// `tx` is taken by value because the drop matters: the spawned tasks each hold
// a clone, so this binding going out of scope is what leaves them as the only
// senders and lets the channel close when the last one finishes. Borrowing it
// would keep the caller's sender alive and hang every `recv()` loop.
#[expect(clippy::needless_pass_by_value)]
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
            // A caller bug, not a user-facing condition, but it still reports:
            // dropping the target silently would leave every consumer waiting
            // on a terminal event that never comes.
            let _ = tx.send(RunEvent {
                run_id,
                kind: TaskEvent::Skipped {
                    index,
                    reason: "unknown repo".into(),
                },
            });
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
                    // Checked twice because the permit is where the waiting
                    // happens. A repo already past this point runs to
                    // completion; there is no kill.
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

/// Run everything and hand back a receiver that closes once the run is done.
///
/// It cannot share ui mode's sender: `render_plain.rs` loops until the channel
/// closes, which needs every sender dropped, and ui mode holds one for its
/// whole life.
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

/// The `GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` slot our forced `color.ui` entry
/// should claim, computed from whatever `GIT_CONFIG_COUNT` the child will
/// already see: it has to land past the user's own slots, or their entries at
/// index 0 would be silently overwritten. Anything that doesn't parse as a
/// count (missing, empty, non-numeric) is treated as no prior config.
fn next_git_config_slot(existing_count: Option<&str>) -> usize {
    existing_count.and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// The last value for `key` in a config-derived environment list, which is the
/// one the child ends up with since later entries overwrite earlier ones.
fn env_value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
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
            // `-e` because without it only the last command's exit code
            // survives: a body that fails to pull then succeeds at building
            // reports success. `||` still tolerates a failure on purpose.
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

    // A pipe makes most tools turn colour off, but ui mode shows full
    // transcripts, so force it back on. CLICOLOR_FORCE and FORCE_COLOR cover
    // most modern CLIs; git needs its config-through-environment protocol
    // because `-c color.ui=always` would have to be threaded through every
    // args branch above instead of set once here.
    //
    // git resolves later-indexed GIT_CONFIG_* slots over earlier ones, so
    // appending after the user's slots also beats a color.ui they set
    // themselves. A config-supplied count is what the child actually sees, so
    // it shadows the ambient one rather than the other way round.
    let from_config = match &op {
        Operation::Shell { env, .. } => env_value(env, "GIT_CONFIG_COUNT").map(str::to_string),
        _ => None,
    };
    let inherited = from_config.or_else(|| std::env::var("GIT_CONFIG_COUNT").ok());
    let slot = next_git_config_slot(inherited.as_deref());
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("CLICOLOR_FORCE", "1")
        .env("FORCE_COLOR", "1")
        .env("GIT_CONFIG_COUNT", (slot + 1).to_string())
        .env(format!("GIT_CONFIG_KEY_{slot}"), "color.ui")
        .env(format!("GIT_CONFIG_VALUE_{slot}"), "always")
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
        // pull, install, build: without `-e` the failed pull is masked by the
        // build that follows it and the repo reports success.
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
                keys: std::collections::BTreeMap::new(),
            })
            .collect()
    }

    /// A shell step, labelled by its action and running `cmd`.
    fn step_at(dir: &std::path::Path, action: &str, cmd: &str) -> Operation {
        Operation::Shell {
            cmd: cmd.into(),
            work_dir: dir.to_path_buf(),
            action: action.into(),
            args: vec![],
            env: vec![],
        }
    }

    fn noop_at(dir: &std::path::Path, action: &str) -> Operation {
        step_at(dir, action, "true")
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
        finished_indices.sort_unstable();
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
        step_at(dir, "noop", cmd)
    }

    /// Cancelling stops everything still queued behind the job limit, but a
    /// repo already past its semaphore permit runs to completion: there is no
    /// kill.
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

        // Which target wins the single permit is not fixed; only that
        // cancelling now lets it finish while the other two are skipped.
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
        skipped_indices.sort_unstable();
        let mut expected: Vec<usize> = (0..3).filter(|i| *i != winner).collect();
        expected.sort_unstable();
        assert_eq!(skipped_indices, expected);
        for (_, reason) in &skipped {
            assert_eq!(reason, "cancelled");
        }
    }

    #[test]
    fn next_git_config_slot_defaults_to_zero_with_no_prior_count() {
        assert_eq!(next_git_config_slot(None), 0);
    }

    #[test]
    fn next_git_config_slot_appends_after_an_existing_count() {
        assert_eq!(next_git_config_slot(Some("2")), 2);
    }

    #[test]
    fn next_git_config_slot_treats_a_malformed_count_as_absent() {
        assert_eq!(next_git_config_slot(Some("not-a-number")), 0);
        assert_eq!(next_git_config_slot(Some("")), 0);
    }

    #[test]
    fn a_count_set_in_the_config_is_the_one_that_counts() {
        // The child never sees mrx's own environment for this key once the
        // config sets it, so the config's value is what our slot must clear.
        let env = vec![
            ("GIT_CONFIG_COUNT".to_string(), "2".to_string()),
            ("GIT_CONFIG_KEY_0".to_string(), "user.name".to_string()),
        ];
        assert_eq!(env_value(&env, "GIT_CONFIG_COUNT"), Some("2"));
        assert_eq!(next_git_config_slot(env_value(&env, "GIT_CONFIG_COUNT")), 2);
        assert_eq!(env_value(&env, "GIT_CONFIG_KEY_9"), None);
    }

    #[test]
    fn a_repeated_config_key_resolves_to_its_last_value() {
        let env = vec![
            ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
            ("GIT_CONFIG_COUNT".to_string(), "3".to_string()),
        ];
        assert_eq!(env_value(&env, "GIT_CONFIG_COUNT"), Some("3"));
    }

    /// `GIT_CONFIG_COUNT` and friends are process-global; tests that set them to
    /// simulate a user's ambient environment must serialise against each other.
    /// `None` removes a key for the duration instead of setting it, so a test
    /// can also guarantee a clean slate regardless of the outer environment.
    async fn with_env<T>(
        vars: &[(&str, Option<&str>)],
        f: impl std::future::Future<Output = T>,
    ) -> T {
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let _guard = LOCK.lock().await;

        let previous: Vec<(String, Option<String>)> = vars
            .iter()
            .map(|(k, _)| ((*k).to_string(), std::env::var(k).ok()))
            .collect();
        for (k, v) in vars {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }

        let result = f.await;

        for (k, prev) in previous {
            match prev {
                Some(v) => std::env::set_var(&k, v),
                None => std::env::remove_var(&k),
            }
        }
        result
    }

    #[tokio::test]
    async fn a_user_supplied_git_config_count_survives_alongside_the_forced_color_slot() {
        let out = with_env(
            &[
                ("GIT_CONFIG_COUNT", Some("2")),
                ("GIT_CONFIG_KEY_0", Some("user.name")),
                ("GIT_CONFIG_VALUE_0", Some("Ambient User")),
                ("GIT_CONFIG_KEY_1", Some("core.editor")),
                ("GIT_CONFIG_VALUE_1", Some("true")),
            ],
            run_body(
                "echo \"$GIT_CONFIG_COUNT $GIT_CONFIG_KEY_0=$GIT_CONFIG_VALUE_0 $GIT_CONFIG_KEY_1=$GIT_CONFIG_VALUE_1 $GIT_CONFIG_KEY_2=$GIT_CONFIG_VALUE_2\"",
            ),
        )
        .await;
        assert_eq!(
            out.stdout.trim(),
            "3 user.name=Ambient User core.editor=true color.ui=always",
            "the user's two slots must survive untouched and the forced colour entry must land at the next free slot"
        );
    }

    #[tokio::test]
    async fn with_no_user_git_config_env_the_default_single_slot_is_unchanged() {
        let out = with_env(
            &[
                ("GIT_CONFIG_COUNT", None),
                ("GIT_CONFIG_KEY_0", None),
                ("GIT_CONFIG_VALUE_0", None),
            ],
            run_body("echo \"$GIT_CONFIG_COUNT $GIT_CONFIG_KEY_0=$GIT_CONFIG_VALUE_0\""),
        )
        .await;
        assert_eq!(out.stdout.trim(), "1 color.ui=always");
    }

    /// Every event of one run, in arrival order. Draining to close is what
    /// proves the run ended: the channel only closes once the last spawned
    /// task has dropped its sender.
    async fn collect(
        repos: &[Repo],
        targets: Vec<(usize, Operation)>,
        stream: bool,
    ) -> Vec<TaskEvent> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_run(repos, targets, 4, PathBuf::from("/dev/null"), tx, 1, stream);
        let mut events = Vec::new();
        while let Some(evt) = rx.recv().await {
            events.push(evt.kind);
        }
        events
    }

    fn event_index(event: &TaskEvent) -> usize {
        match event {
            TaskEvent::Started { index }
            | TaskEvent::Step { index, .. }
            | TaskEvent::Output { index, .. }
            | TaskEvent::Finished { index, .. }
            | TaskEvent::Skipped { index, .. } => *index,
        }
    }

    fn for_index(events: &[TaskEvent], index: usize) -> Vec<&TaskEvent> {
        events.iter().filter(|e| event_index(e) == index).collect()
    }

    /// One target's chain, checked against the grammar and reduced to the step
    /// labels in order and the lines streamed under each of them.
    fn walk_chain(events: &[&TaskEvent]) -> (Vec<String>, Vec<Vec<String>>) {
        let (first, rest) = events.split_first().expect("a target reports something");
        assert!(
            matches!(first, TaskEvent::Started { .. }),
            "a chain opens with Started, got {first:?}"
        );
        let (last, middle) = rest.split_last().expect("a chain ends with Finished");
        assert!(
            matches!(last, TaskEvent::Finished { .. }),
            "a chain ends with Finished, got {last:?}"
        );

        let mut labels: Vec<String> = Vec::new();
        let mut lines: Vec<Vec<String>> = Vec::new();
        for event in middle {
            match event {
                TaskEvent::Step { label, .. } => {
                    labels.push(label.clone());
                    lines.push(Vec::new());
                }
                TaskEvent::Output { step, line, .. } => {
                    assert_eq!(
                        *step + 1,
                        labels.len(),
                        "Output.step must be the ordinal of the Step events already sent"
                    );
                    lines[*step].push(line.clone());
                }
                other => panic!("nothing else belongs mid-chain, got {other:?}"),
            }
        }
        (labels, lines)
    }

    /// Every shape of target in one run: a planned skip, a three-step chain, a
    /// chain whose middle step fails, and an index past the end of the repos.
    fn mixed_targets(dir: &std::path::Path) -> Vec<(usize, Operation)> {
        vec![
            (
                0,
                Operation::Skip {
                    reason: "no update action defined".into(),
                },
            ),
            (
                1,
                Operation::Sequence(vec![
                    step_at(dir, "one", "echo one"),
                    step_at(dir, "two", "echo two"),
                    step_at(dir, "three", "echo three"),
                ]),
            ),
            (
                2,
                Operation::Sequence(vec![
                    step_at(dir, "first", "echo fine"),
                    step_at(dir, "boom", "echo broke >&2; exit 3"),
                    step_at(dir, "never", "echo never"),
                ]),
            ),
            (9, step_at(dir, "ghost", "true")),
        ]
    }

    /// The grammar documented on [`TaskEvent`], end to end over a run holding
    /// every case at once.
    #[tokio::test]
    async fn a_run_reports_the_documented_grammar_for_every_target_it_was_given() {
        let dir = tempfile::tempdir().unwrap();
        let repos = repos(&["r0", "r1", "r2"], dir.path());
        let events = collect(&repos, mixed_targets(dir.path()), true).await;

        let mut terminal: Vec<usize> = events
            .iter()
            .filter(|e| matches!(e, TaskEvent::Finished { .. } | TaskEvent::Skipped { .. }))
            .map(event_index)
            .collect();
        terminal.sort_unstable();
        assert_eq!(
            terminal,
            vec![0, 1, 2, 9],
            "every target reports exactly once terminally, index 9 naming no repo included"
        );

        match for_index(&events, 0).as_slice() {
            [TaskEvent::Skipped { reason, .. }] => assert_eq!(reason, "no update action defined"),
            other => panic!("a skip is the whole of that target's stream, got {other:?}"),
        }
        match for_index(&events, 9).as_slice() {
            [TaskEvent::Skipped { reason, .. }] => assert_eq!(reason, "unknown repo"),
            other => panic!("an index naming no repo still reports, got {other:?}"),
        }

        let (labels, lines) = walk_chain(&for_index(&events, 1));
        assert_eq!(labels, ["one", "two", "three"]);
        assert_eq!(
            lines,
            [["one"], ["two"], ["three"]],
            "each line lands under the step that produced it"
        );

        let failing = for_index(&events, 2);
        let (labels, _) = walk_chain(&failing);
        assert_eq!(
            labels,
            ["first", "boom"],
            "the chain stops at the first non-zero exit"
        );
        match failing.last() {
            Some(TaskEvent::Finished {
                steps, exit_code, ..
            }) => {
                assert_eq!(*exit_code, 3);
                assert_eq!(steps.len(), 2, "the step that never ran leaves no result");
            }
            other => panic!("expected Finished, got {other:?}"),
        }

        // The same run unstreamed: the grammar is unchanged apart from Output.
        let quiet = collect(&repos, mixed_targets(dir.path()), false).await;
        assert!(
            !quiet.iter().any(|e| matches!(e, TaskEvent::Output { .. })),
            "stream: false removes every Output event"
        );
        let (labels, lines) = walk_chain(&for_index(&quiet, 1));
        assert_eq!(labels, ["one", "two", "three"]);
        assert!(lines.iter().all(Vec::is_empty));
    }
}
