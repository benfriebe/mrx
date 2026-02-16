use crate::config::Repo;
use crate::operations::Operation;
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
    _repos: &[Repo],
    operations: Vec<Operation>,
    max_jobs: usize,
) -> mpsc::UnboundedReceiver<TaskEvent> {
    let (tx, rx) = mpsc::unbounded_channel();
    let semaphore = Arc::new(Semaphore::new(max_jobs));

    for (i, op) in operations.into_iter().enumerate() {
        let tx = tx.clone();
        let sem = semaphore.clone();

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
                Operation::Shell { cmd, work_dir } => {
                    let _permit = sem.acquire().await.unwrap();
                    let _ = tx.send(TaskEvent::Started { index: i });
                    let result = Command::new("sh")
                        .args(["-c", &cmd])
                        .current_dir(&work_dir)
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
