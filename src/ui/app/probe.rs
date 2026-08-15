//! Background repo probe: one `git status --porcelain=v2 --branch` per repo,
//! bounded by the same job limit as a run so a refresh can't swamp a machine
//! that is already mid-update (section 07). The probe never fetches:
//! ahead/behind and dirtiness are only ever as fresh as the last time
//! something else did. It does read `FETCH_HEAD`'s timestamp, which is how
//! it notices when that something else was an `update` action or another
//! terminal rather than mrx's own poll.

use crate::config::Repo;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::process::Command;
use tokio::sync::{mpsc, Semaphore};

/// A cold-cache `git status` on a large repo can take seconds; past this, the
/// probe gives up on that one repo rather than holding up the rest of the
/// table indefinitely (section 11, "Probe cost on a cold cache").
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// One repo's branch, upstream tracking, and working-tree state as of the
/// last probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoState {
    pub index: usize,
    /// `None` for a detached HEAD; git's own literal for that case,
    /// `(detached)`, is not treated as a branch name.
    pub branch: Option<String>,
    /// `None` means no upstream, which is what makes a repo ineligible for
    /// auto-update however clean it looks.
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub changed: usize,
    pub present: bool,
    /// The probe hit its per-repo timeout before `git status` returned;
    /// every other field is a default and should not be trusted.
    pub timed_out: bool,
    /// Whether this result came from a poll cycle whose `git fetch` for this
    /// specific repo actually succeeded. Always `false` for a plain probe,
    /// which never fetches. A poll cycle can have some repos fetch
    /// successfully and others fail (offline, VPN, auth), so this is a
    /// per-repo fact, not a session-wide one.
    pub fetched: bool,
    /// When `FETCH_HEAD` was last written, or `None` for a repo that has
    /// never fetched. Git rewrites it on every successful fetch, so a value
    /// newer than the last probe's is evidence that the remote-tracking refs
    /// were refreshed by *something*: an `update` action, a `git pull` in
    /// another terminal, or mrx's own poll.
    pub fetch_head: Option<SystemTime>,
}

impl RepoState {
    fn absent(index: usize) -> Self {
        Self {
            index,
            branch: None,
            upstream: None,
            ahead: 0,
            behind: 0,
            changed: 0,
            present: false,
            timed_out: false,
            fetched: false,
            fetch_head: None,
        }
    }

    /// Visible to [`super::poll`], whose own per-repo timeout produces the
    /// same "unknown" row a probe timeout does.
    pub(super) fn timeout(index: usize) -> Self {
        Self {
            timed_out: true,
            ..Self::absent(index)
        }
    }
}

/// Probe every repo in `which`, concurrently, bounded by `max_jobs`. Results
/// arrive on `tx` as each repo finishes, not in index order, so the first
/// frame can paint long before the slowest one is back.
pub fn spawn_probe(
    repos: &[Repo],
    which: Vec<usize>,
    max_jobs: usize,
    tx: mpsc::UnboundedSender<RepoState>,
) {
    let semaphore = Arc::new(Semaphore::new(max_jobs));
    for index in which {
        let Some(repo) = repos.get(index) else {
            continue;
        };
        let path = repo.path.clone();
        let tx = tx.clone();
        let sem = semaphore.clone();
        tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let state = match tokio::time::timeout(PROBE_TIMEOUT, probe_one(index, &path)).await {
                Ok(state) => state,
                Err(_) => RepoState::timeout(index),
            };
            let _ = tx.send(state);
        });
    }
}

/// Visible to [`super::poll`], whose poll cycle is a `git fetch` followed by
/// exactly this same status parse (section 07).
pub(super) async fn probe_one(index: usize, path: &Path) -> RepoState {
    if !path.is_dir() {
        return RepoState::absent(index);
    }

    let output = Command::new("git")
        .args(["status", "--porcelain=v2", "--branch"])
        .current_dir(path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
            let mut state = parse_porcelain_v2(&String::from_utf8_lossy(&o.stdout));
            state.index = index;
            state.present = true;
            state.fetch_head = fetch_head_written_at(path);
            state
        }
        _ => RepoState::absent(index),
    }
}

/// When this repo's `FETCH_HEAD` was last written. One `stat` alongside a
/// `git status` that already spawned a process, and the only way the probe
/// learns about a fetch it did not perform itself.
fn fetch_head_written_at(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(git_dir(path).join("FETCH_HEAD"))
        .ok()?
        .modified()
        .ok()
}

/// The repo's git directory: `.git` itself, or wherever the `gitdir:` line
/// in it points for a linked worktree or a submodule.
fn git_dir(path: &Path) -> PathBuf {
    let dot_git = path.join(".git");
    if dot_git.is_dir() {
        return dot_git;
    }
    match std::fs::read_to_string(&dot_git) {
        Ok(text) => match text.strip_prefix("gitdir:").map(str::trim) {
            Some(target) if !target.is_empty() => path.join(target),
            _ => dot_git,
        },
        Err(_) => dot_git,
    }
}

/// Parse `git status --porcelain=v2 --branch` output into branch, upstream,
/// ahead/behind, and a count of changed lines. `index` and `present` are the
/// caller's to fill in afterwards; the porcelain stream carries neither.
fn parse_porcelain_v2(text: &str) -> RepoState {
    let mut state = RepoState::absent(0);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            state.branch = (rest != "(detached)").then_some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            state.upstream = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            let mut parts = rest.split_whitespace();
            state.ahead = parts
                .next()
                .and_then(|s| s.strip_prefix('+'))
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            state.behind = parts
                .next()
                .and_then(|s| s.strip_prefix('-'))
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
        } else if !line.starts_with('#') && !line.is_empty() {
            state.changed += 1;
        }
    }
    state
}

/// Branch text for a row: `-` when not checked out, `?` on a timeout,
/// `(detached)` for a checked out repo with no branch, otherwise the name.
pub fn branch_text(state: &RepoState) -> String {
    if state.timed_out {
        return "?".into();
    }
    if !state.present {
        return "-".into();
    }
    state.branch.clone().unwrap_or_else(|| "(detached)".into())
}

/// The working tree alone: `clean` or a modified count, with no ahead/behind
/// or timeout/absent handling layered on top. The building block both
/// [`dirty_text`] and the detail sidebar's briefer column share.
fn working_tree_text(state: &RepoState) -> String {
    if state.changed == 0 {
        "clean".to_string()
    } else {
        format!("{} modified", state.changed)
    }
}

/// Working-tree summary: clean or a modified count, plus ahead/behind when
/// there is an upstream to compare against. The behind count is withheld
/// entirely until this specific repo has fetched: it compares against the
/// local remote-tracking ref, so a stale one would read as "up to date"
/// rather than "not asked recently", and a repo whose own fetch failed
/// (offline, VPN, auth) must not borrow another repo's success (section 02:
/// "up to date" and "not asked recently" must not render identically).
pub fn dirty_text(state: &RepoState, repo_has_fetched: bool) -> String {
    if state.timed_out {
        return "timed out".into();
    }
    if !state.present {
        return "not checked out".into();
    }

    let mut text = working_tree_text(state);

    if state.upstream.is_some() {
        if state.ahead > 0 {
            text.push_str(&format!("  ↑{}", state.ahead));
        }
        // Silence rather than a `?` for a count nothing has established
        // yet: the question mark was on most rows most of the time, which
        // made it read as clutter rather than as the caveat it was. An
        // absent ↓ still isn't a claim of being up to date; a ↓0 would be,
        // which is why one is never drawn.
        if repo_has_fetched && state.behind > 0 {
            text.push_str(&format!("  ↓{}", state.behind));
        }
    }

    text
}

/// The detail sidebar's single state column: working-tree state without
/// ahead/behind, since the sidebar already dropped the branch column, and
/// showing `↑2 ↓3` with nothing to compare it against is more confusing
/// than useful there (section 02, "the sidebar drops to the name and one
/// state column").
pub fn dirty_text_brief(state: &RepoState) -> String {
    if state.timed_out {
        return "timed out".into();
    }
    if !state.present {
        return "not checked out".into();
    }
    working_tree_text(state)
}

/// A probe result tagged with the generation it belongs to, so a receiver
/// can tell a superseded probe's results from the current one's (section
/// 07, "superseded, not queued").
pub struct Probed {
    pub generation: u64,
    pub state: RepoState,
}

/// A sender that tags every result with `generation` before forwarding it
/// on `tx`; the forwarding task ends when either side closes. Shared by
/// [`spawn_probe_generation`] and `poll`'s equivalent, so mashing `r` or
/// switching sets twice quickly can't leave one cycle's results painted on
/// top of a newer one's.
pub(super) fn generation_tagged(
    generation: u64,
    tx: mpsc::UnboundedSender<Probed>,
) -> mpsc::UnboundedSender<RepoState> {
    let (inner_tx, mut inner_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(state) = inner_rx.recv().await {
            if tx.send(Probed { generation, state }).is_err() {
                break;
            }
        }
    });
    inner_tx
}

/// Like [`spawn_probe`], but tags every result with `generation`.
pub fn spawn_probe_generation(
    repos: &[Repo],
    which: Vec<usize>,
    max_jobs: usize,
    generation: u64,
    tx: mpsc::UnboundedSender<Probed>,
) {
    spawn_probe(repos, which, max_jobs, generation_tagged(generation, tx));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_clean_repo() {
        let text = concat!(
            "# branch.oid abc123\n",
            "# branch.head main\n",
            "# branch.upstream origin/main\n",
            "# branch.ab +0 -0\n",
        );
        let state = parse_porcelain_v2(text);
        assert_eq!(state.branch.as_deref(), Some("main"));
        assert_eq!(state.upstream.as_deref(), Some("origin/main"));
        assert_eq!(state.ahead, 0);
        assert_eq!(state.behind, 0);
        assert_eq!(state.changed, 0);
    }

    #[test]
    fn parses_a_dirty_repo() {
        let text = concat!(
            "# branch.oid abc123\n",
            "# branch.head main\n",
            "# branch.upstream origin/main\n",
            "# branch.ab +2 -3\n",
            "1 .M N... 100644 100644 100644 abc def package-lock.json\n",
            "1 M. N... 100644 100644 100644 abc def src/main.rs\n",
        );
        let state = parse_porcelain_v2(text);
        assert_eq!(state.ahead, 2);
        assert_eq!(state.behind, 3);
        assert_eq!(state.changed, 2);
    }

    #[test]
    fn parses_a_detached_head() {
        let text = "# branch.oid abc123\n# branch.head (detached)\n";
        let state = parse_porcelain_v2(text);
        assert_eq!(
            state.branch, None,
            "the literal (detached) is not a branch name"
        );
        assert_eq!(state.upstream, None);
    }

    #[test]
    fn parses_a_branch_with_no_upstream() {
        let text = "# branch.oid abc123\n# branch.head feature/x\n";
        let state = parse_porcelain_v2(text);
        assert_eq!(state.branch.as_deref(), Some("feature/x"));
        assert_eq!(state.upstream, None);
        assert_eq!(state.ahead, 0);
        assert_eq!(state.behind, 0);
    }

    #[test]
    fn parses_untracked_only() {
        let text = concat!(
            "# branch.oid abc123\n",
            "# branch.head main\n",
            "? new-file.txt\n",
            "? another.txt\n",
        );
        let state = parse_porcelain_v2(text);
        assert_eq!(state.changed, 2);
    }

    #[test]
    fn branch_text_reports_a_timeout_as_unknown() {
        assert_eq!(branch_text(&RepoState::timeout(0)), "?");
    }

    #[test]
    fn branch_text_reports_a_missing_repo_as_a_dash() {
        assert_eq!(branch_text(&RepoState::absent(0)), "-");
    }

    #[test]
    fn the_behind_count_is_withheld_until_something_fetches_this_session() {
        let mut state = parse_porcelain_v2(concat!(
            "# branch.oid abc123\n",
            "# branch.head main\n",
            "# branch.upstream origin/main\n",
            "# branch.ab +0 -3\n",
        ));
        state.present = true;
        assert_eq!(
            dirty_text(&state, false),
            "clean",
            "an unestablished count is left unsaid rather than marked unknown"
        );
        assert_eq!(dirty_text(&state, true), "clean  ↓3");
    }

    #[test]
    fn a_repo_that_has_never_fetched_has_no_fetch_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert_eq!(fetch_head_written_at(dir.path()), None);

        std::fs::write(dir.path().join(".git/FETCH_HEAD"), "abc123\n").unwrap();
        assert!(fetch_head_written_at(dir.path()).is_some());
    }

    #[test]
    fn a_worktrees_gitdir_file_is_followed_to_where_fetch_head_really_lives() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real-git-dir");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("FETCH_HEAD"), "abc123\n").unwrap();

        let worktree = dir.path().join("worktree");
        std::fs::create_dir(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", real.display()),
        )
        .unwrap();

        assert!(
            fetch_head_written_at(&worktree).is_some(),
            "a .git file points at the git dir instead of being one"
        );
    }

    #[test]
    fn the_brief_form_drops_ahead_behind() {
        let mut state = parse_porcelain_v2(concat!(
            "# branch.oid abc123\n",
            "# branch.head main\n",
            "# branch.upstream origin/main\n",
            "# branch.ab +2 -3\n",
        ));
        state.present = true;
        assert_eq!(dirty_text_brief(&state), "clean");
    }
}
