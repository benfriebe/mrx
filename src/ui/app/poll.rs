//! The freshness poll (`F`) and auto-update (`Ctrl-A`). A poll cycle is
//! `git fetch --quiet` followed by the same porcelain parse the probe uses,
//! so its results land through the existing `RepoState` path and generation
//! counter; auto-update is a narrow `git merge --ff-only` layered on top,
//! run only where [`can_fast_forward`] says it is safe.

use super::probe::{self, generation_tagged, Probed, RepoState};
use crate::config::Repo;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::{mpsc, Semaphore};

/// How often `F` fetches, unless a config or a persisted session says
/// otherwise; the interval lives on `App` rather than in the timer, so it can
/// vary per session. Shared with `[DEFAULT] auto_fetch = on` so both routes
/// into the poll start at the same cadence.
pub const DEFAULT_POLL_INTERVAL: Duration = crate::config::DEFAULT_AUTO_FETCH;

/// Upper bound on a poll interval from any external source (a hand-edited
/// or corrupted `ui.json`, chiefly). Far enough below the range `Instant`
/// can represent that `Instant::now() + interval` can never overflow;
/// `Instant::now() + Duration::from_secs(u64::MAX)` panics outright, which
/// would crash the app at startup before a single frame draws.
pub const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60 * 60 * 24 * 365 * 10);

/// Clamp `interval` into `[1s, MAX_POLL_INTERVAL]`, whatever its source,
/// before it is used to build the poll timer.
pub fn clamp_interval(interval: Duration) -> Duration {
    interval.clamp(Duration::from_secs(1), MAX_POLL_INTERVAL)
}

/// A fetch is a network round trip rather than a local status read, so it
/// gets more slack than the probe's own timeout before the poll gives up on
/// one repo and leaves it for the next cycle.
const POLL_TIMEOUT: Duration = Duration::from_secs(20);

/// A repo is auto-updatable only if a fast-forward is guaranteed to be a
/// no-op on anything the user has touched. Anything else is left alone and
/// reported, not fixed.
pub fn can_fast_forward(s: &RepoState) -> bool {
    s.present
        && s.upstream.is_some()
        && s.behind > 0
        && s.ahead == 0 // diverged: a merge, not a fast-forward
        && s.changed == 0 // dirty: even a clean ff can surprise you mid-edit
}

/// Fetch, then read state back exactly the way a plain probe would. A fetch
/// that fails (offline, no remote, auth) must not stop the status read that
/// follows, since a stale local view is still worth showing, and the result
/// carries whether this repo's own fetch succeeded so it can't inherit the
/// freshness of another repo polled in the same cycle.
async fn poll_one(index: usize, path: &Path) -> RepoState {
    let fetched = if path.is_dir() {
        matches!(
            Command::new("git")
                .args(["fetch", "--quiet"])
                .current_dir(path)
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await,
            Ok(status) if status.success()
        )
    } else {
        false
    };
    let mut state = probe::probe_one(index, path).await;
    state.fetched = fetched;
    state
}

/// Fetch then probe every repo in `which`, bounded by `max_jobs`: the same
/// job limit a probe uses, so a poll can't compete with a live run for the
/// network.
pub fn spawn_poll(
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
            let state = match tokio::time::timeout(POLL_TIMEOUT, poll_one(index, &path)).await {
                Ok(state) => state,
                Err(_) => RepoState::timeout(index),
            };
            let _ = tx.send(state);
        });
    }
}

/// Like [`spawn_poll`], tagged with a probe generation so its results share
/// the probe's own staleness handling.
pub fn spawn_poll_generation(
    repos: &[Repo],
    which: Vec<usize>,
    max_jobs: usize,
    generation: u64,
    tx: mpsc::UnboundedSender<Probed>,
) {
    spawn_poll(repos, which, max_jobs, generation_tagged(generation, tx));
}

/// One repo's outcome from an auto-update pass, tagged with the cycle it
/// belongs to so a result arriving after a later cycle has started is
/// dropped rather than counted against it; the same generation scheme the
/// probe uses.
pub struct AutoUpdateResult {
    pub index: usize,
    pub generation: u64,
    pub outcome: AutoUpdateOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoUpdateOutcome {
    FastForwarded,
    Failed(String),
}

/// `git merge --ff-only` on every repo in `which`, bounded by `max_jobs`.
/// Callers filter `which` through [`can_fast_forward`] first, but time
/// passes before a repo's turn at the semaphore and `merge --ff-only`
/// succeeds even on a dirty tree when the incoming change doesn't conflict,
/// so each task re-probes and re-checks immediately before merging rather
/// than trusting the poll's snapshot.
///
/// That re-probe narrows the window to milliseconds but cannot close it: the
/// probe and the merge are two `git` invocations, and an edit made outside
/// mrx can still land between them. Hence auto-update only ever
/// fast-forwards, never merges or rebases anything a conflict could touch,
/// and stays off until turned on.
pub fn spawn_auto_update(
    repos: &[Repo],
    which: Vec<usize>,
    max_jobs: usize,
    generation: u64,
    tx: mpsc::UnboundedSender<AutoUpdateResult>,
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
            let fresh = probe::probe_one(index, &path).await;
            let outcome = if !can_fast_forward(&fresh) {
                AutoUpdateOutcome::Failed("no longer fast-forwardable".into())
            } else {
                match Command::new("git")
                    .args(["merge", "--ff-only"])
                    .current_dir(&path)
                    .env("GIT_TERMINAL_PROMPT", "0")
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .await
                {
                    Ok(o) if o.status.success() => AutoUpdateOutcome::FastForwarded,
                    Ok(o) => AutoUpdateOutcome::Failed(
                        String::from_utf8_lossy(&o.stderr).trim().to_string(),
                    ),
                    Err(e) => AutoUpdateOutcome::Failed(e.to_string()),
                }
            };
            let _ = tx.send(AutoUpdateResult {
                index,
                generation,
                outcome,
            });
        });
    }
}

/// The header's `poll 5m` / `poll 5m · auto` text: minutes when the interval
/// divides evenly, seconds otherwise, so an unusual persisted value reads
/// exactly rather than rounding away.
pub fn format_interval(interval: Duration) -> String {
    let secs = interval.as_secs();
    if secs > 0 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// How long ago something happened, rolled up to the largest unit that fits:
/// seconds under a minute, minutes under an hour, hours beyond. The unit on
/// screen is also the rate the number moves at, so a figure that has not
/// changed since the last frame is one that is still true.
pub fn format_ago(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..60 => format!("{secs}s ago"),
        60..3600 => format!("{}m ago", secs / 60),
        _ => format!("{}h ago", secs / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(upstream: bool, ahead: u32, behind: u32, changed: usize, present: bool) -> RepoState {
        RepoState {
            index: 0,
            branch: Some("main".into()),
            upstream: upstream.then(|| "origin/main".into()),
            ahead,
            behind,
            changed,
            changes: Default::default(),
            present,
            timed_out: false,
            fetched: false,
            fetch_head: None,
        }
    }

    #[test]
    fn clean_and_behind_can_fast_forward() {
        assert!(can_fast_forward(&state(true, 0, 3, 0, true)));
    }

    #[test]
    fn a_dirty_tree_is_left_alone() {
        assert!(!can_fast_forward(&state(true, 0, 3, 1, true)));
    }

    #[test]
    fn a_repo_with_only_local_commits_is_left_alone() {
        assert!(!can_fast_forward(&state(true, 2, 0, 0, true)));
    }

    #[test]
    fn a_diverged_repo_is_left_alone() {
        assert!(!can_fast_forward(&state(true, 2, 3, 0, true)));
    }

    #[test]
    fn a_repo_with_no_upstream_is_left_alone() {
        assert!(!can_fast_forward(&state(false, 0, 3, 0, true)));
    }

    #[test]
    fn a_repo_not_checked_out_is_left_alone() {
        assert!(!can_fast_forward(&state(true, 0, 3, 0, false)));
    }

    #[test]
    fn ago_rolls_up_to_the_largest_unit_that_fits() {
        let ago = |secs| format_ago(Duration::from_secs(secs));
        assert_eq!(ago(0), "0s ago");
        assert_eq!(ago(59), "59s ago");
        assert_eq!(ago(60), "1m ago");
        assert_eq!(ago(3599), "59m ago");
        assert_eq!(ago(3600), "1h ago");
        assert_eq!(ago(60 * 60 * 26), "26h ago");
    }

    #[test]
    fn format_interval_prefers_minutes_when_they_divide_evenly() {
        assert_eq!(format_interval(Duration::from_secs(300)), "5m");
        assert_eq!(format_interval(Duration::from_secs(90)), "90s");
        assert_eq!(format_interval(Duration::from_secs(0)), "0s");
    }

    #[test]
    fn clamp_interval_leaves_a_sane_value_untouched() {
        assert_eq!(
            clamp_interval(Duration::from_secs(300)),
            Duration::from_secs(300)
        );
    }

    #[test]
    fn clamp_interval_caps_a_value_that_would_overflow_an_instant() {
        let clamped = clamp_interval(Duration::from_secs(u64::MAX));
        assert_eq!(clamped, MAX_POLL_INTERVAL);
        // The actual failure mode this guards against: adding the clamped
        // interval to `now` must not panic.
        let _ = tokio::time::Instant::now() + clamped;
    }

    #[test]
    fn clamp_interval_floors_a_zero_value() {
        assert_eq!(
            clamp_interval(Duration::from_secs(0)),
            Duration::from_secs(1)
        );
    }

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git must be on PATH to run this test");
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    fn git_output(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git must be on PATH to run this test");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `merge --ff-only` succeeds on a dirty tree whenever the incoming
    /// commit doesn't touch the dirtied file, so eligibility decided from an
    /// earlier probe snapshot has to be re-checked at merge time.
    #[tokio::test]
    async fn auto_update_skips_a_repo_that_went_dirty_after_it_was_marked_eligible() {
        let origin = tempfile::tempdir().unwrap();
        run_git(origin.path(), &["init", "--quiet"]);
        run_git(origin.path(), &["config", "user.email", "t@example.com"]);
        run_git(origin.path(), &["config", "user.name", "t"]);
        std::fs::write(origin.path().join("a.txt"), "one\n").unwrap();
        run_git(origin.path(), &["add", "a.txt"]);
        run_git(origin.path(), &["commit", "--quiet", "-m", "one"]);

        let clone_dir = tempfile::tempdir().unwrap();
        let clone_path = clone_dir.path().join("clone");
        run_git(
            clone_dir.path(),
            &[
                "clone",
                "--quiet",
                origin.path().to_str().unwrap(),
                clone_path.to_str().unwrap(),
            ],
        );
        run_git(&clone_path, &["config", "user.email", "t@example.com"]);
        run_git(&clone_path, &["config", "user.name", "t"]);

        // Origin moves ahead, and the clone learns about it, exactly as a
        // poll cycle would: the clone is clean and behind, eligible.
        std::fs::write(origin.path().join("a.txt"), "two\n").unwrap();
        run_git(origin.path(), &["commit", "--quiet", "-am", "two"]);
        run_git(&clone_path, &["fetch", "--quiet"]);

        // An uncommitted edit on a file the incoming commit never touches,
        // so a plain `merge --ff-only` would still succeed here.
        std::fs::write(clone_path.join("b.txt"), "local edit\n").unwrap();

        let repo = Repo {
            name: "clone".into(),
            path: clone_path.clone(),
            clone_url: None,
            keys: Default::default(),
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_auto_update(&[repo], vec![0], 1, 1, tx);
        let result = rx.recv().await.expect("a result");

        assert_eq!(
            result.outcome,
            AutoUpdateOutcome::Failed("no longer fast-forwardable".into()),
            "a repo dirtied after eligibility was decided must not be merged"
        );
        let clone_head = git_output(&clone_path, &["rev-parse", "HEAD"]);
        let origin_head = git_output(origin.path(), &["rev-parse", "HEAD"]);
        assert_ne!(clone_head, origin_head, "the merge must not have happened");
    }

    /// `poll_one` must carry a failed fetch into `RepoState.fetched` rather
    /// than reporting the status read that follows as if the fetch had
    /// succeeded.
    #[tokio::test]
    async fn a_fetch_that_fails_leaves_fetched_false_on_the_resulting_repo_state() {
        let dir = tempfile::tempdir().unwrap();
        run_git(dir.path(), &["init", "--quiet"]);
        run_git(dir.path(), &["config", "user.email", "t@example.com"]);
        run_git(dir.path(), &["config", "user.name", "t"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        run_git(dir.path(), &["add", "a.txt"]);
        run_git(dir.path(), &["commit", "--quiet", "-m", "one"]);
        // A remote that can never be reached: `git fetch` fails immediately
        // (no such repository) rather than hanging on a real network.
        run_git(
            dir.path(),
            &["remote", "add", "origin", "/nonexistent-remote-xyz"],
        );

        let state = poll_one(0, dir.path()).await;

        assert!(
            !state.fetched,
            "a fetch against an unreachable remote must leave fetched false"
        );
    }
}
