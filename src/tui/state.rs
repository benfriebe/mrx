use crate::config::Repo;

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
    pub selected: usize,
    pub expanded: Option<usize>,
    pub scroll_offset: usize,
    pub tick: usize,
    pub command_name: String,
    pub all_done: bool,
}

impl AppState {
    pub fn new(repos: Vec<Repo>, command_name: &str) -> Self {
        let n = repos.len();
        Self {
            repos,
            statuses: vec![RepoStatus::Pending; n],
            selected: 0,
            expanded: None,
            scroll_offset: 0,
            tick: 0,
            command_name: command_name.to_string(),
            all_done: false,
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
        if self.expanded == Some(self.selected) {
            self.expanded = None;
            self.scroll_offset = 0;
        } else {
            self.expanded = Some(self.selected);
            self.scroll_offset = 0;
        }
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
