use crate::cli::Command;
use crate::config::Repo;
use crate::summarize::Shape;
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
        /// Config keys exported as MR_<KEY>, so a section can hand its command
        /// context without the command string having to interpolate anything.
        env: Vec<(String, String)>,
    },
    /// Steps in order; the first non-zero exit ends the chain.
    Sequence(Vec<Operation>),
    /// Nothing to do (e.g. checkout for already-existing repo)
    Skip { reason: String },
    /// Repo doesn't exist and we can't clone (no URL)
    NotCheckedOut,
}

impl Operation {
    /// What to call this step in a summary. Config-defined steps carry the key they
    /// came from, so a `post_update` failure reads back as `post_update`.
    pub fn label(&self) -> String {
        match self {
            Operation::Git { args, .. } => format!("git {}", args.join(" ")),
            Operation::Clone { .. } => "clone".into(),
            Operation::Shell { action, .. } => action.clone(),
            // Flattened or resolved before anything runs, as in `run_step`.
            Operation::Sequence(_) | Operation::Skip { .. } | Operation::NotCheckedOut => {
                String::new()
            }
        }
    }

    /// How this step's output should be read; see [`Shape`] for why it is
    /// decided here. A config-defined body is always `Shape::Generic`,
    /// whatever its action happens to be named.
    pub fn shape(&self) -> Shape {
        match self {
            Operation::Git { args, .. } => match args.first().map(String::as_str) {
                Some("pull") => Shape::Pull,
                Some("status") => Shape::Status,
                Some("diff") => Shape::Diff,
                Some("push") => Shape::Push,
                Some("fetch") => Shape::Fetch,
                _ => Shape::Generic,
            },
            Operation::Clone { .. } => Shape::Clone,
            Operation::Shell { .. } => Shape::Generic,
            Operation::Sequence(_) | Operation::Skip { .. } | Operation::NotCheckedOut => {
                Shape::Generic
            }
        }
    }
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

/// Every config key visible to this repo, as `MR_<KEY>` pairs.
///
/// Passed as environment rather than interpolated into the command string, so a
/// branch name cannot break out of its shell word. Keys that can't spell an
/// environment variable name are dropped.
fn config_env(repo: &Repo, defaults: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let mut env: BTreeMap<&str, &str> = BTreeMap::new();
    for (k, v) in defaults.iter().chain(repo.keys.iter()) {
        env.insert(k, v);
    }
    env.into_iter()
        .filter(|(k, _)| !k.is_empty() && !k.starts_with(|c: char| c.is_ascii_digit()))
        .map(|(k, v)| (env_var_name(k), v.to_string()))
        .collect()
}

/// Environment variable name for a config key: `post_update` becomes `MR_POST_UPDATE`.
/// Anything that can't spell an identifier becomes an underscore.
fn env_var_name(key: &str) -> String {
    let mut name = String::with_capacity(key.len() + 3);
    name.push_str("MR_");
    name.extend(key.chars().map(|c| {
        if c.is_ascii_alphanumeric() {
            c.to_ascii_uppercase()
        } else {
            '_'
        }
    }));
    name
}

fn shell(
    body: &str,
    work_dir: PathBuf,
    action: &str,
    args: Vec<String>,
    repo: &Repo,
    defaults: &BTreeMap<String, String>,
) -> Operation {
    Operation::Shell {
        cmd: body.to_string(),
        work_dir,
        action: action.to_string(),
        args,
        env: config_env(repo, defaults),
    }
}

/// Collapse steps into one operation, leaving a lone step unwrapped.
fn sequence(mut steps: Vec<Operation>) -> Operation {
    match steps.len() {
        0 => Operation::Skip {
            reason: "nothing to do".into(),
        },
        1 => steps.remove(0),
        _ => Operation::Sequence(steps),
    }
}

/// How this repo gets onto disk: its own `checkout` body if it has one, else the
/// URL parsed out of that key.
fn clone_step(repo: &Repo, defaults: &BTreeMap<String, String>) -> Option<Operation> {
    if let Some(body) = resolve_body(repo, defaults, "checkout") {
        let parent = repo
            .path
            .parent()
            .map_or_else(|| repo.path.clone(), PathBuf::from);
        return Some(shell(body, parent, "checkout", vec![], repo, defaults));
    }
    repo.clone_url.as_ref().map(|url| Operation::Clone {
        url: url.clone(),
        dest: repo.path.clone(),
    })
}

/// A `post_<action>` step, if one is defined anywhere in the cascade.
fn post_step(repo: &Repo, defaults: &BTreeMap<String, String>, action: &str) -> Option<Operation> {
    let key = format!("post_{action}");
    resolve_body(repo, defaults, &key)
        .map(|body| shell(body, repo.path.clone(), &key, vec![], repo, defaults))
}

/// `update`/`pull`: clone a repo that is not on disk yet, then run the `update`
/// the cascade defines, or `git pull` when it defines none.
fn update_plan(repo: &Repo, defaults: &BTreeMap<String, String>, exists: bool) -> Operation {
    let mut steps = Vec::new();

    if !exists {
        let Some(step) = clone_step(repo, defaults) else {
            return Operation::NotCheckedOut;
        };
        steps.push(step);
        // A fresh clone still needs the repo's update, which is where
        // install steps live, or it is the one repo never set up.
        if let Some(body) = resolve_body(repo, defaults, "update") {
            steps.push(shell(
                body,
                repo.path.clone(),
                "update",
                vec![],
                repo,
                defaults,
            ));
        }
    } else if let Some(body) = resolve_body(repo, defaults, "update") {
        steps.push(shell(
            body,
            repo.path.clone(),
            "update",
            vec![],
            repo,
            defaults,
        ));
    } else {
        steps.push(Operation::Git {
            args: vec!["pull".into()],
            work_dir: repo.path.clone(),
        });
    }

    steps.extend(post_step(repo, defaults, "update"));
    sequence(steps)
}

pub fn plan(command: &Command, repo: &Repo, defaults: &BTreeMap<String, String>) -> Operation {
    let exists = repo.path.is_dir();

    match command {
        Command::Update | Command::Pull => update_plan(repo, defaults, exists),

        // `--branch` for the `## main...origin/main [ahead 1, behind 2]`
        // header: working-tree changes alone leave out the half that decides
        // whether to push or pull. The counts are against the local
        // remote-tracking ref, so they are only as fresh as the last fetch.
        Command::Status => builtin_or_shell(
            repo,
            defaults,
            "status",
            exists,
            vec!["status".into(), "--short".into(), "--branch".into()],
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
            let Some(step) = clone_step(repo, defaults) else {
                return Operation::Skip {
                    reason: "no clone URL".into(),
                };
            };
            let mut steps = vec![step];
            steps.extend(post_step(repo, defaults, "checkout"));
            sequence(steps)
        }

        Command::Run { cmd } => {
            if !exists {
                return Operation::NotCheckedOut;
            }
            shell(
                &cmd.join(" "),
                repo.path.clone(),
                "run",
                vec![],
                repo,
                defaults,
            )
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
            let Some(body) = resolve_body(repo, defaults, &name) else {
                return Operation::Skip {
                    reason: format!("no {name} action defined"),
                };
            };
            let mut steps = vec![shell(body, repo.path.clone(), &name, tail, repo, defaults)];
            steps.extend(post_step(repo, defaults, &name));
            sequence(steps)
        }

        Command::List | Command::Ls | Command::Sets | Command::Register | Command::Ui => {
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
    let body = match resolve_body(repo, defaults, action) {
        Some(body) => shell(body, repo.path.clone(), action, vec![], repo, defaults),
        None => Operation::Git {
            args: git_args,
            work_dir: repo.path.clone(),
        },
    };
    let mut steps = vec![body];
    steps.extend(post_step(repo, defaults, action));
    sequence(steps)
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
            other => panic!("expected Shell, got {other:?}"),
        }
    }

    #[test]
    fn custom_action_missing_skips_with_reason() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_keys(dir.path().to_path_buf(), &[]);
        let cmd = Command::Custom(vec!["bake".into()]);

        match plan(&cmd, &repo, &BTreeMap::new()) {
            Operation::Skip { reason } => assert!(reason.contains("bake")),
            other => panic!("expected Skip, got {other:?}"),
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
            other => panic!("expected Shell from DEFAULT, got {other:?}"),
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
            other => panic!("expected Shell from repo keys, got {other:?}"),
        }
    }

    #[test]
    fn builtin_status_falls_through_when_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_keys(dir.path().to_path_buf(), &[]);

        match plan(&Command::Status, &repo, &BTreeMap::new()) {
            Operation::Git { args, .. } => assert_eq!(args, vec!["status", "--short", "--branch"]),
            other => panic!("expected Git fallback, got {other:?}"),
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
            other => panic!("expected NotCheckedOut, got {other:?}"),
        }
    }

    #[test]
    fn missing_repo_with_update_plans_clone_then_update() {
        let repo = Repo {
            name: "r".into(),
            path: PathBuf::from("/this/path/does/not/exist/ever"),
            clone_url: Some("git@example.com:r.git".into()),
            keys: BTreeMap::new(),
        };
        let defs = defaults(&[("update", "yarn install")]);

        match plan(&Command::Update, &repo, &defs) {
            Operation::Sequence(steps) => {
                assert_eq!(steps.len(), 2);
                assert!(matches!(steps[0], Operation::Clone { .. }));
                match &steps[1] {
                    Operation::Shell { cmd, work_dir, .. } => {
                        assert_eq!(cmd, "yarn install");
                        assert_eq!(work_dir, &repo.path, "setup runs inside the fresh clone");
                    }
                    other => panic!("expected Shell, got {other:?}"),
                }
            }
            other => panic!("expected Sequence, got {other:?}"),
        }
    }

    #[test]
    fn checkout_body_wins_over_parsed_clone_url() {
        let repo = Repo {
            name: "r".into(),
            path: PathBuf::from("/nope/r"),
            clone_url: Some("git@example.com:r.git".into()),
            keys: BTreeMap::from([(
                "checkout".to_string(),
                "git clone --depth 1 x r".to_string(),
            )]),
        };

        match plan(&Command::Checkout, &repo, &BTreeMap::new()) {
            Operation::Shell { cmd, work_dir, .. } => {
                assert_eq!(cmd, "git clone --depth 1 x r");
                assert_eq!(work_dir, PathBuf::from("/nope"), "clones from the parent");
            }
            other => panic!("expected Shell, got {other:?}"),
        }
    }

    #[test]
    fn post_action_appends_to_builtin_and_override_alike() {
        let dir = tempfile::tempdir().unwrap();

        let repo = repo_with_keys(dir.path().to_path_buf(), &[]);
        let defs = defaults(&[("post_update", "./setup.sh")]);
        match plan(&Command::Update, &repo, &defs) {
            Operation::Sequence(steps) => {
                assert!(matches!(steps[0], Operation::Git { .. }));
                assert!(matches!(&steps[1], Operation::Shell { cmd, .. } if cmd == "./setup.sh"));
            }
            other => panic!("expected Sequence after git pull, got {other:?}"),
        }

        let repo = repo_with_keys(dir.path().to_path_buf(), &[("status", "git status -sb")]);
        let defs = defaults(&[("post_status", "echo done")]);
        match plan(&Command::Status, &repo, &defs) {
            Operation::Sequence(steps) => assert_eq!(steps.len(), 2),
            other => panic!("expected Sequence, got {other:?}"),
        }
    }

    #[test]
    fn existing_repo_without_overrides_still_plans_a_bare_pull() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_keys(dir.path().to_path_buf(), &[]);

        match plan(&Command::Update, &repo, &BTreeMap::new()) {
            Operation::Git { args, .. } => assert_eq!(args, vec!["pull".to_string()]),
            other => panic!("default behaviour changed: {other:?}"),
        }
    }

    #[test]
    fn config_keys_are_exported_with_section_winning() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_keys(
            dir.path().to_path_buf(),
            &[("branch", "master"), ("update", "sync.sh")],
        );
        let defs = defaults(&[("branch", "main"), ("reset", "false")]);

        match plan(&Command::Update, &repo, &defs) {
            Operation::Shell { env, .. } => {
                let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
                assert_eq!(get("MR_BRANCH"), Some("master"), "section beats DEFAULT");
                assert_eq!(get("MR_RESET"), Some("false"), "DEFAULT still reaches env");
                assert_eq!(get("MR_UPDATE"), Some("sync.sh"));
            }
            other => panic!("expected Shell, got {other:?}"),
        }
    }

    #[test]
    fn env_names_are_sanitised() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_keys(
            dir.path().to_path_buf(),
            &[("post-hook", "x"), ("2fast", "y"), ("update", "sync.sh")],
        );

        match plan(&Command::Update, &repo, &BTreeMap::new()) {
            Operation::Shell { env, .. } => {
                let names: Vec<&str> = env.iter().map(|(n, _)| n.as_str()).collect();
                assert!(names.contains(&"MR_POST_HOOK"), "got {names:?}");
                assert!(
                    !names.iter().any(|n| n.contains("2FAST")),
                    "leading-digit keys are dropped: {names:?}"
                );
            }
            other => panic!("expected Shell, got {other:?}"),
        }
    }

    #[test]
    fn a_step_is_labelled_by_where_it_came_from() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_keys(dir.path().to_path_buf(), &[]);
        let defs = defaults(&[("post_update", "npm run build")]);

        match plan(&Command::Update, &repo, &defs) {
            Operation::Sequence(steps) => {
                assert_eq!(steps[0].label(), "git pull");
                assert_eq!(steps[1].label(), "post_update");
            }
            other => panic!("expected Sequence, got {other:?}"),
        }
    }

    #[test]
    fn builtin_git_operations_carry_their_matching_shape() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_keys(dir.path().to_path_buf(), &[]);

        assert_eq!(
            plan(&Command::Update, &repo, &BTreeMap::new()).shape(),
            Shape::Pull
        );
        assert_eq!(
            plan(&Command::Status, &repo, &BTreeMap::new()).shape(),
            Shape::Status
        );
        assert_eq!(
            plan(&Command::Diff, &repo, &BTreeMap::new()).shape(),
            Shape::Diff
        );
        assert_eq!(
            plan(&Command::Push, &repo, &BTreeMap::new()).shape(),
            Shape::Push
        );
        assert_eq!(
            plan(&Command::Fetch, &repo, &BTreeMap::new()).shape(),
            Shape::Fetch
        );
    }

    #[test]
    fn a_config_defined_body_is_generic_shaped_even_when_named_like_a_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_keys(dir.path().to_path_buf(), &[("status", "./health-check.sh")]);

        assert_eq!(
            plan(&Command::Status, &repo, &BTreeMap::new()).shape(),
            Shape::Generic,
            "a shell body's output isn't git's, whatever the action is named"
        );
    }

    #[test]
    fn a_bare_clone_is_clone_shaped_but_a_checkout_body_is_generic() {
        let repo = Repo {
            name: "r".into(),
            path: PathBuf::from("/nope/r"),
            clone_url: Some("git@example.com:r.git".into()),
            keys: BTreeMap::new(),
        };
        assert_eq!(
            plan(&Command::Checkout, &repo, &BTreeMap::new()).shape(),
            Shape::Clone
        );

        let with_body = repo_with_keys(
            PathBuf::from("/nope/r2"),
            &[("checkout", "git clone --depth 1 x r2")],
        );
        assert_eq!(
            plan(&Command::Checkout, &with_body, &BTreeMap::new()).shape(),
            Shape::Generic
        );
    }

    #[test]
    fn env_var_name_uppercases_and_sanitises() {
        assert_eq!(env_var_name("branch"), "MR_BRANCH");
        assert_eq!(env_var_name("post_update"), "MR_POST_UPDATE");
        assert_eq!(env_var_name("Reset"), "MR_RESET");
        assert_eq!(env_var_name("deploy-target"), "MR_DEPLOY_TARGET");
        assert_eq!(env_var_name("a.b c"), "MR_A_B_C");
    }
}
