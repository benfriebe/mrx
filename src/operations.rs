use crate::cli::Command;
use crate::config::Repo;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum Operation {
    /// Run a git command in the repo directory
    Git {
        args: Vec<String>,
        work_dir: PathBuf,
    },
    /// Clone a repo that doesn't exist yet
    Clone { url: String, dest: PathBuf },
    /// Run an arbitrary shell command
    Shell { cmd: String, work_dir: PathBuf },
    /// Nothing to do (e.g. checkout for already-existing repo)
    Skip { reason: String },
    /// Repo doesn't exist and we can't clone (no URL)
    NotCheckedOut,
}

pub fn plan(command: &Command, repo: &Repo) -> Operation {
    let exists = repo.path.is_dir();

    match command {
        Command::Update | Command::Pull => {
            if exists {
                Operation::Git {
                    args: vec!["pull".into()],
                    work_dir: repo.path.clone(),
                }
            } else if let Some(url) = &repo.clone_url {
                Operation::Clone {
                    url: url.clone(),
                    dest: repo.path.clone(),
                }
            } else {
                Operation::NotCheckedOut
            }
        }

        Command::Status => {
            if exists {
                Operation::Git {
                    args: vec!["status".into(), "--short".into()],
                    work_dir: repo.path.clone(),
                }
            } else {
                Operation::NotCheckedOut
            }
        }

        Command::Diff => {
            if exists {
                Operation::Git {
                    args: vec!["diff".into(), "--no-color".into()],
                    work_dir: repo.path.clone(),
                }
            } else {
                Operation::NotCheckedOut
            }
        }

        Command::Push => {
            if exists {
                Operation::Git {
                    args: vec!["push".into()],
                    work_dir: repo.path.clone(),
                }
            } else {
                Operation::NotCheckedOut
            }
        }

        Command::Fetch => {
            if exists {
                Operation::Git {
                    args: vec!["fetch".into()],
                    work_dir: repo.path.clone(),
                }
            } else {
                Operation::NotCheckedOut
            }
        }

        Command::Checkout | Command::Co => {
            if exists {
                Operation::Skip {
                    reason: "already exists".into(),
                }
            } else if let Some(url) = &repo.clone_url {
                Operation::Clone {
                    url: url.clone(),
                    dest: repo.path.clone(),
                }
            } else {
                Operation::Skip {
                    reason: "no clone URL".into(),
                }
            }
        }

        Command::Run { cmd } => {
            let full_cmd = cmd.join(" ");
            if exists {
                Operation::Shell {
                    cmd: full_cmd,
                    work_dir: repo.path.clone(),
                }
            } else {
                Operation::NotCheckedOut
            }
        }

        Command::List | Command::Ls => {
            unreachable!("list command doesn't use operations")
        }
    }
}
