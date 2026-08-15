use crate::config::Repo;
use crate::ui::app::probe::{self, RepoState};

/// Shown for a repo whose branch the probe hasn't reported back for yet.
const PROBING: &str = "…";

#[derive(Debug, Clone, PartialEq)]
pub enum RepoStatus {
    Pending,
    Running,
    Done {
        summary: String,
        stdout: String,
        stderr: String,
        exit_code: i32,
    },
    Skipped {
        reason: String,
    },
}

impl RepoStatus {
    pub fn is_done(&self) -> bool {
        matches!(self, RepoStatus::Done { .. } | RepoStatus::Skipped { .. })
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, RepoStatus::Done { exit_code, .. } if *exit_code != 0)
    }
}

pub struct AppState {
    pub repos: Vec<Repo>,
    pub statuses: Vec<RepoStatus>,
    /// Branch label per repo, resolved from the background probe. Reads
    /// [`PROBING`] until that repo's first result arrives.
    pub branch_labels: Vec<String>,
    pub selected: usize,
    pub expanded: Option<usize>,
    pub scroll_offset: usize,
    pub tick: usize,
    pub command_name: String,
    pub all_done: bool,
    probe_generation: u64,
}

impl AppState {
    pub fn new(repos: Vec<Repo>, command_name: &str) -> Self {
        let n = repos.len();
        Self {
            repos,
            statuses: vec![RepoStatus::Pending; n],
            branch_labels: vec![PROBING.to_string(); n],
            selected: 0,
            expanded: None,
            scroll_offset: 0,
            tick: 0,
            command_name: command_name.to_string(),
            all_done: false,
            probe_generation: 0,
        }
    }

    /// Bump the probe generation and return it, so the caller can tag the
    /// probe it is about to spawn; [`on_probe`](Self::on_probe) drops a
    /// result tagged with an older one.
    pub fn begin_probe(&mut self) -> u64 {
        self.probe_generation += 1;
        self.probe_generation
    }

    /// Apply one probe result, unless a later probe has since superseded it.
    pub fn on_probe(&mut self, generation: u64, state: RepoState) {
        if generation < self.probe_generation {
            return;
        }
        if let Some(slot) = self.branch_labels.get_mut(state.index) {
            *slot = probe::branch_text(&state);
        }
    }

    pub fn done_count(&self) -> usize {
        self.statuses.iter().filter(|s| s.is_done()).count()
    }

    pub fn failed_count(&self) -> usize {
        self.statuses.iter().filter(|s| s.is_failed()).count()
    }

    pub fn total(&self) -> usize {
        self.repos.len()
    }

    /// Reset all per-repo state so the command can be executed again. Statuses
    /// return to `Pending`, branch labels go back to [`PROBING`] until the
    /// caller's fresh probe fills them in (an update may have created new
    /// clones or switched branches), and any expanded view is collapsed.
    pub fn reset_for_rerun(&mut self) {
        let n = self.repos.len();
        self.statuses = vec![RepoStatus::Pending; n];
        self.branch_labels = vec![PROBING.to_string(); n];
        self.expanded = None;
        self.scroll_offset = 0;
        self.all_done = false;
        if n > 0 {
            self.selected = self.selected.min(n - 1);
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.total() {
            self.selected += 1;
        }
    }

    pub fn toggle_expand(&mut self) {
        self.expanded = (self.expanded != Some(self.selected)).then_some(self.selected);
        self.scroll_offset = 0;
    }

    pub fn collapse(&mut self) {
        self.expanded = None;
        self.scroll_offset = 0;
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    pub fn scroll_down(&mut self, max_lines: usize) {
        if self.scroll_offset + 1 < max_lines {
            self.scroll_offset += 1;
        }
    }

    pub fn expanded_content(&self) -> Option<String> {
        let idx = self.expanded?;
        match &self.statuses[idx] {
            RepoStatus::Done { stdout, stderr, .. } => {
                let mut content = String::new();
                if !stdout.is_empty() {
                    content.push_str(stdout);
                }
                if !stderr.is_empty() {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str(stderr);
                }
                if content.is_empty() {
                    content.push_str("(no output)");
                }
                Some(content)
            }
            RepoStatus::Running => Some("(still running...)".into()),
            RepoStatus::Pending => Some("(pending...)".into()),
            RepoStatus::Skipped { reason } => Some(format!("(skipped: {})", reason)),
        }
    }

    pub fn branch_label(&self, idx: usize) -> &str {
        self.branch_labels
            .get(idx)
            .map(String::as_str)
            .unwrap_or("-")
    }

    pub fn summary_line(&self) -> String {
        let failed = self.failed_count();
        let done = self.done_count();
        let total = self.total();
        if failed > 0 {
            format!("{}/{} done, {} failed", done, total, failed)
        } else {
            format!("{}/{} done", done, total)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn repo(name: &str) -> Repo {
        Repo {
            name: name.to_string(),
            path: PathBuf::from(format!("/nonexistent/{}", name)),
            clone_url: None,
            keys: Default::default(),
        }
    }

    fn done(exit_code: i32) -> RepoStatus {
        RepoStatus::Done {
            summary: "done".into(),
            stdout: "out".into(),
            stderr: String::new(),
            exit_code,
        }
    }

    #[test]
    fn reset_for_rerun_clears_state() {
        let mut state = AppState::new(vec![repo("a"), repo("b"), repo("c")], "update");
        state.statuses = vec![done(0), done(1), RepoStatus::Skipped { reason: "x".into() }];
        state.selected = 2;
        state.expanded = Some(2);
        state.scroll_offset = 5;
        state.all_done = true;

        state.reset_for_rerun();

        assert!(
            state.statuses.iter().all(|s| *s == RepoStatus::Pending),
            "statuses should all reset to Pending"
        );
        assert_eq!(state.statuses.len(), 3);
        assert_eq!(state.expanded, None);
        assert_eq!(state.scroll_offset, 0);
        assert!(!state.all_done);
        // selection stays put when still in range
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn reset_for_rerun_clamps_out_of_range_selection() {
        let mut state = AppState::new(vec![repo("a"), repo("b")], "status");
        state.selected = 5; // somehow out of range
        state.reset_for_rerun();
        assert_eq!(state.selected, 1); // clamped to last index
    }

    #[test]
    fn a_repo_shows_a_placeholder_branch_until_the_probe_reports_back() {
        let state = AppState::new(vec![repo("a")], "status");
        assert_eq!(state.branch_label(0), PROBING);
    }

    fn probed(index: usize, branch: &str) -> RepoState {
        RepoState {
            index,
            branch: Some(branch.to_string()),
            upstream: None,
            ahead: 0,
            behind: 0,
            changed: 0,
            present: true,
            timed_out: false,
            fetched: false,
            fetch_head: None,
        }
    }

    #[test]
    fn a_probe_result_for_the_current_generation_sets_the_branch_label() {
        let mut state = AppState::new(vec![repo("a")], "status");
        let generation = state.begin_probe();
        state.on_probe(generation, probed(0, "main"));
        assert_eq!(state.branch_label(0), "main");
    }

    #[test]
    fn a_stale_probe_result_is_dropped() {
        let mut state = AppState::new(vec![repo("a")], "status");
        state.begin_probe(); // generation 1
        state.begin_probe(); // generation 2 supersedes it
        state.on_probe(1, probed(0, "stale-branch"));
        assert_eq!(
            state.branch_label(0),
            PROBING,
            "a result from a superseded generation must be dropped"
        );
    }

    #[test]
    fn reset_for_rerun_goes_back_to_the_probing_placeholder() {
        let mut state = AppState::new(vec![repo("a")], "status");
        let generation = state.begin_probe();
        state.on_probe(generation, probed(0, "main"));
        state.reset_for_rerun();
        assert_eq!(state.branch_label(0), PROBING);
    }
}
