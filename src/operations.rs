use crate::cli::Command;
use crate::config::Repo;
use std::collections::BTreeMap;
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
    /// Run a shell command body. `args` become positional ($1, $2, ...) inside the body.
    Shell {
        cmd: String,
        work_dir: PathBuf,
        action: String,
        args: Vec<String>,
    },
    /// Nothing to do (e.g. checkout for already-existing repo)
    Skip { reason: String },
    /// Repo doesn't exist and we can't clone (no URL)
    NotCheckedOut,
}

/// Look up a shell body for `action` in the cascade: repo section -> [DEFAULT].
fn resolve_body<'a>(
    repo: &'a Repo,
    defaults: &'a BTreeMap<String, String>,
    action: &str,
) -> Option<&'a str> {
    repo.keys
        .get(action)
        .map(String::as_str)
        .or_else(|| defaults.get(action).map(String::as_str))
}

pub fn plan(command: &Command, repo: &Repo, defaults: &BTreeMap<String, String>) -> Operation {
    let exists = repo.path.is_dir();

    match command {
        Command::Update | Command::Pull => {
            if let Some(body) = resolve_body(repo, defaults, "update") {
                if exists {
                    return Operation::Shell {
                        cmd: body.to_string(),
                        work_dir: repo.path.clone(),
                        action: "update".into(),
                        args: vec![],
                    };
                }
                // not checked out: fall through to clone-if-possible
            }
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

        Command::Status => builtin_or_shell(
            repo,
            defaults,
            "status",
            exists,
            vec!["status".into(), "--short".into()],
        ),

        Command::Diff => builtin_or_shell(
            repo,
            defaults,
            "diff",
            exists,
            vec!["diff".into(), "--no-color".into()],
        ),

        Command::Push => builtin_or_shell(repo, defaults, "push", exists, vec!["push".into()]),

        Command::Fetch => builtin_or_shell(repo, defaults, "fetch", exists, vec!["fetch".into()]),

        Command::Checkout | Command::Co => {
            if exists {
                return Operation::Skip {
                    reason: "already exists".into(),
                };
            }
            if let Some(body) = resolve_body(repo, defaults, "checkout") {
                let parent = repo
                    .path
                    .parent()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| repo.path.clone());
                return Operation::Shell {
                    cmd: body.to_string(),
                    work_dir: parent,
                    action: "checkout".into(),
                    args: vec![],
                };
            }
            if let Some(url) = &repo.clone_url {
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
                    action: "run".into(),
                    args: vec![],
                }
            } else {
                Operation::NotCheckedOut
            }
        }

        Command::Custom(parts) => {
            let (name, tail) = match parts.split_first() {
                Some((n, rest)) => (n.clone(), rest.to_vec()),
                None => {
                    return Operation::Skip {
                        reason: "empty action name".into(),
                    };
                }
            };
            if !exists {
                return Operation::NotCheckedOut;
            }
            match resolve_body(repo, defaults, &name) {
                Some(body) => Operation::Shell {
                    cmd: body.to_string(),
                    work_dir: repo.path.clone(),
                    action: name,
                    args: tail,
                },
                None => Operation::Skip {
                    reason: format!("no {} action defined", name),
                },
            }
        }

        Command::List | Command::Ls | Command::Register => {
            unreachable!("command doesn't use operations")
        }
    }
}

fn builtin_or_shell(
    repo: &Repo,
    defaults: &BTreeMap<String, String>,
    action: &str,
    exists: bool,
    git_args: Vec<String>,
) -> Operation {
    if !exists {
        return Operation::NotCheckedOut;
    }
    if let Some(body) = resolve_body(repo, defaults, action) {
        return Operation::Shell {
            cmd: body.to_string(),
            work_dir: repo.path.clone(),
            action: action.into(),
            args: vec![],
        };
    }
    Operation::Git {
        args: git_args,
        work_dir: repo.path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_with_keys(path: PathBuf, keys: &[(&str, &str)]) -> Repo {
        let mut map = BTreeMap::new();
        for (k, v) in keys {
            map.insert((*k).to_string(), (*v).to_string());
        }
        Repo {
            name: "r".into(),
            path,
            clone_url: None,
            keys: map,
        }
    }

    fn defaults(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), (*v).to_string());
        }
        m
    }

    #[test]
    fn custom_action_resolves_from_repo_keys() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_keys(dir.path().to_path_buf(), &[("install", "npm install")]);
        let cmd = Command::Custom(vec!["install".into(), "--frozen-lockfile".into()]);

        match plan(&cmd, &repo, &BTreeMap::new()) {
            Operation::Shell {
                cmd, action, args, ..
            } => {
                assert_eq!(cmd, "npm install");
                assert_eq!(action, "install");
                assert_eq!(args, vec!["--frozen-lockfile".to_string()]);
            }
            other => panic!("expected Shell, got {:?}", other),
        }
    }

    #[test]
    fn custom_action_missing_skips_with_reason() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_keys(dir.path().to_path_buf(), &[]);
        let cmd = Command::Custom(vec!["bake".into()]);

        match plan(&cmd, &repo, &BTreeMap::new()) {
            Operation::Skip { reason } => assert!(reason.contains("bake")),
            other => panic!("expected Skip, got {:?}", other),
        }
    }

    #[test]
    fn default_section_overrides_builtin_update() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_keys(dir.path().to_path_buf(), &[]);
        let defs = defaults(&[("update", "git pull --rebase")]);

        match plan(&Command::Update, &repo, &defs) {
            Operation::Shell { cmd, action, .. } => {
                assert_eq!(cmd, "git pull --rebase");
                assert_eq!(action, "update");
            }
            other => panic!("expected Shell from DEFAULT, got {:?}", other),
        }
    }

    #[test]
    fn repo_keys_win_over_default_section() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_keys(
            dir.path().to_path_buf(),
            &[("update", "git pull --ff-only")],
        );
        let defs = defaults(&[("update", "git pull --rebase")]);

        match plan(&Command::Update, &repo, &defs) {
            Operation::Shell { cmd, .. } => assert_eq!(cmd, "git pull --ff-only"),
            other => panic!("expected Shell from repo keys, got {:?}", other),
        }
    }

    #[test]
    fn builtin_status_falls_through_when_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_keys(dir.path().to_path_buf(), &[]);

        match plan(&Command::Status, &repo, &BTreeMap::new()) {
            Operation::Git { args, .. } => assert_eq!(args, vec!["status", "--short"]),
            other => panic!("expected Git fallback, got {:?}", other),
        }
    }

    #[test]
    fn custom_action_on_missing_repo_is_not_checked_out() {
        let repo = repo_with_keys(
            PathBuf::from("/this/path/does/not/exist/ever"),
            &[("install", "echo hi")],
        );
        let cmd = Command::Custom(vec!["install".into()]);

        match plan(&cmd, &repo, &BTreeMap::new()) {
            Operation::NotCheckedOut => {}
            other => panic!("expected NotCheckedOut, got {:?}", other),
        }
    }
}
