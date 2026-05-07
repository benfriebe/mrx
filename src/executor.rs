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
    },
    Skipped {
        index: usize,
        reason: String,
    },
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
        let repo_name = repos[i].name.clone();
        let repo_path = repos[i].path.clone();
        let config_path = config_path.clone();

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
                Operation::Git { args, work_dir } => {
                    let _permit = sem.acquire().await.unwrap();
                    let _ = tx.send(TaskEvent::Started { index: i });
                    let result = Command::new("git")
                        .args(&args)
                        .current_dir(&work_dir)
                        .env("GIT_TERMINAL_PROMPT", "0")
                        .env("GIT_PAGER", "cat")
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .output()
                        .await;

                    match result {
                        Ok(output) => {
                            let _ = tx.send(TaskEvent::Finished {
                                index: i,
                                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                                exit_code: output.status.code().unwrap_or(1),
                            });
                        }
                        Err(e) => {
                            let _ = tx.send(TaskEvent::Finished {
                                index: i,
                                stdout: String::new(),
                                stderr: format!("failed to execute: {}", e),
                                exit_code: 1,
                            });
                        }
                    }
                }
                Operation::Clone { url, dest } => {
                    let _permit = sem.acquire().await.unwrap();
                    let _ = tx.send(TaskEvent::Started { index: i });

                    let parent = dest.parent().unwrap_or(&dest);
                    let _ = tokio::fs::create_dir_all(parent).await;

                    let result = Command::new("git")
                        .args(["clone", &url, &dest.to_string_lossy()])
                        .env("GIT_TERMINAL_PROMPT", "0")
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .output()
                        .await;

                    match result {
                        Ok(output) => {
                            let _ = tx.send(TaskEvent::Finished {
                                index: i,
                                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                                exit_code: output.status.code().unwrap_or(1),
                            });
                        }
                        Err(e) => {
                            let _ = tx.send(TaskEvent::Finished {
                                index: i,
                                stdout: String::new(),
                                stderr: format!("failed to execute: {}", e),
                                exit_code: 1,
                            });
                        }
                    }
                }
                Operation::Shell {
                    cmd,
                    work_dir,
                    action,
                    args,
                } => {
                    let _permit = sem.acquire().await.unwrap();
                    let _ = tx.send(TaskEvent::Started { index: i });

                    // sh -c '<body>' mrx <arg1> <arg2> ...
                    // exposes the args as $1, $2 inside the body, and $0 as "mrx".
                    let mut sh_args: Vec<String> = vec!["-c".into(), cmd, "mrx".into()];
                    sh_args.extend(args);

                    let result = Command::new("sh")
                        .args(&sh_args)
                        .current_dir(&work_dir)
                        .env("MR_REPO", &repo_path)
                        .env("MR_REPONAME", &repo_name)
                        .env("MR_CONFIG", config_path.as_path())
                        .env("MR_ACTION", &action)
                        .env("GIT_TERMINAL_PROMPT", "0")
                        .env("GIT_PAGER", "cat")
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .output()
                        .await;

                    match result {
                        Ok(output) => {
                            let _ = tx.send(TaskEvent::Finished {
                                index: i,
                                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                                exit_code: output.status.code().unwrap_or(1),
                            });
                        }
                        Err(e) => {
                            let _ = tx.send(TaskEvent::Finished {
                                index: i,
                                stdout: String::new(),
                                stderr: format!("failed to execute: {}", e),
                                exit_code: 1,
                            });
                        }
                    }
                }
            }
        });
    }

    rx
}
