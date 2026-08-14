//! The freshness poll (`F`) and auto-update (`Ctrl-A`), section 02 and
//! section 07. A poll cycle is `git fetch --quiet` followed by the same
//! porcelain parse the probe uses, so its results land through the existing
//! `RepoState` path and generation counter; auto-update is a narrow
//! `git merge --ff-only` layered on top, run only where [`can_fast_forward`]
//! says it is safe, and only ever reported when it is not.

use super::probe::{self, Probed, RepoState};
use crate::config::Repo;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::{mpsc, Semaphore};

/// How often `F` fetches, unless a persisted session says otherwise
/// (section 09); the interval itself lives on `App`, not as a constant
/// baked into the timer, so it can vary per session (section 07).
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(300);

/// Upper bound on a poll interval from any external source (a hand-edited
/// or corrupted `ui.json`, chiefly). Comfortably larger than anyone would
/// actually want to wait between polls, but far enough below the range
/// `Instant` can represent that `Instant::now() + interval` can never
/// overflow: `tokio::time::Instant::now() + Duration::from_secs(u64::MAX)`
/// panics outright, and that panic would otherwise crash the app at
/// startup before a single frame draws.
pub const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60 * 60 * 24 * 365 * 10);

/// Clamp `interval` into `[1s, MAX_POLL_INTERVAL]`, the last line of
/// defense before it's used to build the poll timer, regardless of where it
/// came from.
pub fn clamp_interval(interval: Duration) -> Duration {
    interval.clamp(Duration::from_secs(1), MAX_POLL_INTERVAL)
}

/// A fetch is a network round trip rather than a local status read, so it
/// gets more slack than the probe's own five-second timeout before the poll
/// gives up on one repo and leaves it for the next cycle.
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

/// Fetch, then read state back exactly the way a plain probe would; a fetch
/// that fails (offline, no remote) leaves refs stale but must not stop the
/// status read that follows, since a stale local view is still worth
/// showing.
async fn poll_one(index: usize, path: &Path) -> RepoState {
    if path.is_dir() {
        let _ = Command::new("git")
            .args(["fetch", "--quiet"])
            .current_dir(path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    probe::probe_one(index, path).await
}

/// Fetch then probe every repo in `which`, bounded by `max_jobs`: the same
/// job limit a probe uses, so a poll can't compete with a live run for the
/// network even once the run itself has stopped queueing new work.
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
/// the probe's own staleness handling: "a poll is a probe with a fetch in
/// front of it" (section 02).
pub fn spawn_poll_generation(
    repos: &[Repo],
    which: Vec<usize>,
    max_jobs: usize,
    generation: u64,
    tx: mpsc::UnboundedSender<Probed>,
) {
    let (inner_tx, mut inner_rx) = mpsc::unbounded_channel();
    spawn_poll(repos, which, max_jobs, inner_tx);
    tokio::spawn(async move {
        while let Some(state) = inner_rx.recv().await {
            if tx.send(Probed { generation, state }).is_err() {
                break;
            }
        }
    });
}

/// One repo's outcome from an auto-update pass.
pub struct AutoUpdateResult {
    pub index: usize,
    pub outcome: AutoUpdateOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoUpdateOutcome {
    FastForwarded,
    Failed(String),
}

/// `git merge --ff-only` on every repo in `which`, bounded by `max_jobs`.
/// Callers are expected to have already filtered `which` through
/// [`can_fast_forward`]; this makes no safety decision of its own, it just
/// runs the merge and reports what happened.
pub fn spawn_auto_update(
    repos: &[Repo],
    which: Vec<usize>,
    max_jobs: usize,
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
            let outcome = match Command::new("git")
                .args(["merge", "--ff-only"])
                .current_dir(&path)
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
            {
                Ok(o) if o.status.success() => AutoUpdateOutcome::FastForwarded,
                Ok(o) => {
                    AutoUpdateOutcome::Failed(String::from_utf8_lossy(&o.stderr).trim().to_string())
                }
                Err(e) => AutoUpdateOutcome::Failed(e.to_string()),
            };
            let _ = tx.send(AutoUpdateResult { index, outcome });
        });
    }
}

/// The header's `poll 5m` / `poll 5m · auto` text: minutes when the
/// interval divides evenly, seconds otherwise, so an unusual persisted
/// value still reads exactly rather than rounding away.
pub fn format_interval(interval: Duration) -> String {
    let secs = interval.as_secs();
    if secs > 0 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
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
            present,
            timed_out: false,
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
}
