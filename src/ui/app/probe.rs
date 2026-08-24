//! Background repo probe: one `git status --porcelain=v2 --branch` per repo,
//! bounded by the same job limit as a run. The probe never fetches, so
//! ahead/behind and dirtiness are only as fresh as the last time something else
//! did. It reads `FETCH_HEAD`'s timestamp to notice when that something else
//! was an `update` action or another terminal.

use crate::config::Repo;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::process::Command;
use tokio::sync::{mpsc, Semaphore};

/// A cold-cache `git status` on a large repo can take seconds; past this, the
/// probe gives up on that one repo rather than holding up the rest of the
/// table indefinitely.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// What the buckets in a working-tree summary count, in the order they are
/// reported. Deliberately the same three `crate::summarize` uses for an `s`
/// run, so the STATE column and RESULT describe one repo the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    Modified,
    Untracked,
    Deleted,
}

/// The working-tree entries of [`RepoState::changed`] split by kind. Kinds
/// git reports that fit none of the buckets (unmerged, ignored) are in the
/// total and in none of these.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changes {
    pub modified: usize,
    pub untracked: usize,
    pub deleted: usize,
}

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
    /// Every working-tree entry git reported, whatever kind; also the
    /// dirty/clean predicate `can_fast_forward` and `dirty_count` read.
    pub changed: usize,
    pub changes: Changes,
    pub present: bool,
    /// The probe hit its per-repo timeout before `git status` returned;
    /// every other field is a default and should not be trusted.
    pub timed_out: bool,
    /// Whether this result came from a poll cycle whose `git fetch` for this
    /// specific repo succeeded; always `false` for a plain probe, which never
    /// fetches. Fetches fail per repo (offline, VPN, auth), so this is not a
    /// session-wide fact.
    pub fetched: bool,
    /// When `FETCH_HEAD` was last written, or `None` for a repo that has never
    /// fetched. Git rewrites it on every successful fetch, so a value newer
    /// than the last probe's means the remote-tracking refs were refreshed by
    /// something.
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
            changes: Changes::default(),
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

/// Which per-repo cycle a fan-out runs. The two share the job limit, the
/// timeout-becomes-an-unknown-row rule and the generation tagging, so they
/// share the fan-out and vary here.
#[derive(Clone, Copy)]
pub enum Cycle {
    /// The status parse alone, off whatever the last fetch left behind.
    Probe,
    /// A `git fetch --quiet` and then that same parse.
    Poll,
}

impl Cycle {
    /// How long one repo gets before it is written off as an unknown row. A
    /// poll waits on the network, so it is given far longer than a local
    /// status read.
    fn timeout(self) -> Duration {
        match self {
            Self::Probe => PROBE_TIMEOUT,
            Self::Poll => super::poll::POLL_TIMEOUT,
        }
    }

    async fn run(self, index: usize, path: &Path) -> RepoState {
        match self {
            Self::Probe => probe_one(index, path).await,
            Self::Poll => super::poll::poll_one(index, path).await,
        }
    }
}

/// Run `cycle` against every repo in `which`, concurrently, bounded by
/// `max_jobs`.
///
/// Results arrive on `tx` as each repo finishes, not in index order, so the
/// first frame paints before the slowest one is back. Each carries
/// `generation`, so an older cycle's results cannot paint over a newer one's.
pub fn spawn_cycle(
    repos: &[Repo],
    which: Vec<usize>,
    max_jobs: usize,
    generation: u64,
    tx: &mpsc::UnboundedSender<Probed>,
    cycle: Cycle,
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
            let state = match tokio::time::timeout(cycle.timeout(), cycle.run(index, &path)).await {
                Ok(state) => state,
                Err(_) => RepoState::timeout(index),
            };
            let _ = tx.send(Probed { generation, state });
        });
    }
}

/// Visible to [`super::poll`], whose poll cycle is a `git fetch` followed by
/// exactly this same status parse.
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

/// When this repo's `FETCH_HEAD` was last written: the only way the probe
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
            match change_kind(line) {
                Some(Change::Modified) => state.changes.modified += 1,
                Some(Change::Untracked) => state.changes.untracked += 1,
                Some(Change::Deleted) => state.changes.deleted += 1,
                None => {}
            }
        }
    }
    state
}

/// Which bucket a porcelain v2 entry falls in. `?` is untracked outright;
/// `1` (ordinary) and `2` (renamed or copied) carry an `XY` field, staged
/// then unstaged, read here the way `crate::summarize` reads short format:
/// a line counts once, under the first column that says anything. Unmerged
/// (`u`) and ignored (`!`) entries have no bucket and are left to the total.
fn change_kind(line: &str) -> Option<Change> {
    let mut fields = line.split(' ');
    match fields.next()? {
        "?" => Some(Change::Untracked),
        "1" | "2" => {
            let mut xy = fields.next()?.chars();
            let (staged, unstaged) = (xy.next()?, xy.next()?);
            [staged, unstaged].into_iter().find_map(|c| match c {
                'M' | 'R' | 'C' => Some(Change::Modified),
                'A' => Some(Change::Untracked),
                'D' => Some(Change::Deleted),
                _ => None,
            })
        }
        _ => None,
    }
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

/// The working tree alone, phrased by [`summarize::working_tree`]. The building
/// block [`dirty_text`] and the detail sidebar's briefer column share, so it
/// adds no ahead/behind or timeout/absent handling of its own.
fn working_tree_text(state: &RepoState) -> String {
    crate::summarize::working_tree(
        state.changes.modified,
        state.changes.untracked,
        state.changes.deleted,
        state.changed,
    )
}

/// The STATE column: what the working tree is carrying, or why there is no
/// answer. Distance from the upstream is [`sync_counts`]' column, not this
/// one, so a row's state text ends where the next row's does.
pub fn dirty_text(state: &RepoState) -> String {
    if state.timed_out {
        return "timed out".into();
    }
    if !state.present {
        return "not checked out".into();
    }
    working_tree_text(state)
}

/// The SYNC column's raw counts, `(ahead, behind)`, or `None` when there is
/// nothing to measure against: no upstream, or a probe that never got far
/// enough to read one.
///
/// Behind is reported as zero until this repo itself has fetched. It compares
/// against the local remote-tracking ref, so a stale one would render "not
/// asked recently" as "up to date".
pub fn sync_counts(state: &RepoState, repo_has_fetched: bool) -> Option<(u32, u32)> {
    if state.timed_out || !state.present || state.upstream.is_none() {
        return None;
    }
    let behind = if repo_has_fetched { state.behind } else { 0 };
    Some((state.ahead, behind))
}

/// A probe result tagged with the generation it belongs to, so a receiver
/// can tell a superseded probe's results from the current one's.
pub struct Probed {
    pub generation: u64,
    pub state: RepoState,
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

    /// A parsed fixture as a row would see it: the porcelain stream carries
    /// neither `index` nor `present`.
    fn checked_out(text: &str) -> RepoState {
        RepoState {
            present: true,
            ..parse_porcelain_v2(text)
        }
    }

    #[test]
    fn an_untracked_file_is_not_called_modified() {
        let state = checked_out("# branch.head main\n? new-file.txt\n");
        assert_eq!(dirty_text(&state), "1 untracked");
    }

    #[test]
    fn an_edited_file_is_called_modified() {
        let state = checked_out(concat!(
            "# branch.head main\n",
            "1 .M N... 100644 100644 100644 abc def src/main.rs\n",
        ));
        assert_eq!(dirty_text(&state), "1 modified");
    }

    #[test]
    fn mixed_kinds_are_named_rather_than_summed() {
        let state = checked_out(concat!(
            "# branch.head main\n",
            "1 .M N... 100644 100644 100644 abc def src/main.rs\n",
            "1 M. N... 100644 100644 100644 abc def Cargo.toml\n",
            "? new-file.txt\n",
        ));
        assert_eq!(dirty_text(&state), "2 modified, 1 untracked");
    }

    #[test]
    fn a_deletion_and_a_rename_land_in_their_own_buckets() {
        let state = checked_out(concat!(
            "# branch.head main\n",
            "1 .D N... 100644 100644 000000 abc def gone.txt\n",
            "2 R. N... 100644 100644 100644 abc def R100 new.txt\told.txt\n",
        ));
        assert_eq!(dirty_text(&state), "1 modified, 1 deleted");
    }

    #[test]
    fn a_file_staged_then_edited_again_counts_once() {
        let state = checked_out(concat!(
            "# branch.head main\n",
            "1 MM N... 100644 100644 100644 abc def src/main.rs\n",
        ));
        assert_eq!(dirty_text(&state), "1 modified");
    }

    /// The two forms of one real repo, captured from git itself. The STATE
    /// column reads the v2 form and an `s` run's RESULT reads the short one,
    /// so the point of the fixture is that they still say the same sentence.
    #[test]
    fn the_state_column_and_an_s_run_phrase_one_repo_the_same_way() {
        let porcelain_v2 = concat!(
            "# branch.oid 256d2aa5\n",
            "# branch.head main\n",
            "1 .D N... 100644 100644 000000 61780798 61780798 gone.txt\n",
            "1 .M N... 100644 100644 100644 78981922 78981922 keep.txt\n",
            "2 R. N... 100644 100644 100644 f2ad6c76 f2ad6c76 R100 renamed.txt\told.txt\n",
            "1 A. N... 000000 100644 100644 00000000 19d9cc85 staged.txt\n",
            "? brand-new.txt\n",
        );
        let short = concat!(
            "## main\n",
            " D gone.txt\n",
            " M keep.txt\n",
            "R  old.txt -> renamed.txt\n",
            "A  staged.txt\n",
            "?? brand-new.txt\n",
        );
        assert_eq!(
            dirty_text(&checked_out(porcelain_v2)),
            crate::summarize::summarize(crate::summarize::Shape::Status, short, "", 0),
        );
    }

    #[test]
    fn a_change_with_no_bucket_falls_back_to_a_bare_total() {
        let state = checked_out(concat!(
            "# branch.head main\n",
            "u UU N... 100644 100644 100644 100644 abc def ghi conflict.txt\n",
        ));
        assert_eq!(dirty_text(&state), "1 changed");
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
            sync_counts(&state, false),
            Some((0, 0)),
            "an unestablished count reads as no distance rather than as unknown"
        );
        assert_eq!(sync_counts(&state, true), Some((0, 3)));
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

    /// STATE and SYNC answer different questions from the same probe, and a
    /// row's state text has to end where every other row's does.
    #[test]
    fn the_state_text_carries_no_distance_and_the_counts_carry_no_state() {
        let mut state = parse_porcelain_v2(concat!(
            "# branch.oid abc123\n",
            "# branch.head main\n",
            "# branch.upstream origin/main\n",
            "# branch.ab +2 -3\n",
            "1 .M N... 100644 100644 100644 abc abc src/main.rs\n",
        ));
        state.present = true;
        assert_eq!(dirty_text(&state), "1 modified");
        assert_eq!(sync_counts(&state, true), Some((2, 3)));
    }

    /// Nothing to be ahead or behind of is not the same as being level with it.
    #[test]
    fn a_branch_with_no_upstream_has_no_distance_at_all() {
        let mut state =
            parse_porcelain_v2(concat!("# branch.oid abc123\n", "# branch.head wip\n",));
        state.present = true;
        assert_eq!(sync_counts(&state, true), None);
        assert_eq!(dirty_text(&state), "clean");

        state.timed_out = true;
        assert_eq!(sync_counts(&state, true), None);
    }
    fn repo_at(name: &str) -> Repo {
        Repo {
            name: name.to_string(),
            path: PathBuf::from(format!("/nonexistent/{name}")),
            clone_url: None,
            keys: std::collections::BTreeMap::new(),
        }
    }

    /// Both cycles run through the one fan-out, so this is what says every
    /// repo asked for comes back, carrying the generation the caller began
    /// with, whichever cycle it was. The paths do not exist, which is a state
    /// the probe has an answer for and needs no git to reach.
    #[tokio::test]
    async fn a_cycle_reports_on_every_repo_it_was_asked_about() {
        for cycle in [Cycle::Probe, Cycle::Poll] {
            let repos = vec![repo_at("a"), repo_at("b"), repo_at("c")];
            let (tx, mut rx) = mpsc::unbounded_channel();
            // 9 names no repo: unlike a run, a cycle feeds a table keyed by
            // the rows themselves, so there is nothing for it to report to.
            spawn_cycle(&repos, vec![0, 2, 9], 4, 7, &tx, cycle);
            drop(tx);

            let mut seen: Vec<(u64, usize)> = Vec::new();
            while let Some(probed) = rx.recv().await {
                assert!(!probed.state.present, "the path does not exist");
                seen.push((probed.generation, probed.state.index));
            }
            seen.sort_unstable();
            assert_eq!(seen, vec![(7, 0), (7, 2)]);
        }
    }
}
