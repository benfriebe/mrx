use crate::config::Repo;
use crate::operations::Operation;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::{mpsc, Semaphore};

#[derive(Debug, Clone)]
pub enum TaskEvent {
    Started {
        index: usize,
    },
    Finished {
        index: usize,
        stdout: String,
        stderr: String,
        exit_code: i32,
        /// Which step ended the chain, when there was more than one to choose from.
        failed_step: Option<String>,
    },
    Skipped {
        index: usize,
        reason: String,
    },
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

pub fn execute_all(
    repos: &[Repo],
    operations: Vec<Operation>,
    max_jobs: usize,
    config_path: PathBuf,
) -> mpsc::UnboundedReceiver<TaskEvent> {
    let (tx, rx) = mpsc::unbounded_channel();
    let semaphore = Arc::new(Semaphore::new(max_jobs));
    let config_path = Arc::new(config_path);

    for (i, op) in operations.into_iter().enumerate() {
        let tx = tx.clone();
        let sem = semaphore.clone();
        let ctx = StepContext {
            repo_name: repos[i].name.clone(),
            repo_path: repos[i].path.clone(),
            config_path: config_path.clone(),
        };

        tokio::spawn(async move {
            match op {
                Operation::Skip { reason } => {
                    let _ = tx.send(TaskEvent::Skipped { index: i, reason });
                }
                Operation::NotCheckedOut => {
                    let _ = tx.send(TaskEvent::Skipped {
                        index: i,
                        reason: "not checked out".into(),
                    });
                }
                runnable => {
                    let _permit = sem.acquire().await.unwrap();
                    let _ = tx.send(TaskEvent::Started { index: i });

                    let steps = match runnable {
                        Operation::Sequence(steps) => steps,
                        single => vec![single],
                    };

                    // Naming the step only earns its keep when the row could be
                    // reporting any of several.
                    let name_steps = steps.len() > 1;

                    let (mut stdout, mut stderr, mut code) = (String::new(), String::new(), 0);
                    let mut failed_step = None;
                    for step in steps {
                        let label = step.label();
                        let result = run_step(step, &ctx).await;
                        stdout.push_str(&result.stdout);
                        stderr.push_str(&result.stderr);
                        if result.code != 0 {
                            // Keep the output collected so far; stop the chain.
                            code = result.code;
                            failed_step = name_steps.then_some(label);
                            break;
                        }
                    }

                    let _ = tx.send(TaskEvent::Finished {
                        index: i,
                        stdout,
                        stderr,
                        exit_code: code,
                        failed_step,
                    });
                }
            }
        });
    }

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
            // sh -c '<body>' mrx <arg1> <arg2> ...
            // exposes the args as $1, $2 inside the body, and $0 as "mrx".
            let mut sh_args: Vec<String> = vec!["-c".into(), cmd.clone(), "mrx".into()];
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
