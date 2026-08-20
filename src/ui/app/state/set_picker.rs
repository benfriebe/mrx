//! Swapping the repo list out from under the app: set switching and config
//! reload, which share `reconcile_repos` and the same by-name carry-over rule.

use super::App;
use crate::config::{self, Repo};
use crate::sets;
use crate::ui::app::actions;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

/// One row in the set picker: a discovered set's name and the config path
/// it resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetEntry {
    pub name: String,
    pub path: PathBuf,
}

impl App {
    /// `tab`: open the set picker. Blocked by [`mutation_blocker`](Self::mutation_blocker),
    /// since switching the repo list out from under a live run's or
    /// auto-update's indices would attribute its results to the wrong rows.
    pub fn open_set_picker(&mut self) {
        if self.refuse_if_mutation_blocked("switch sets") {
            return;
        }
        let mut entries: Vec<SetEntry> = sets::discover()
            .into_iter()
            .map(|(name, path)| SetEntry { name, path })
            .collect();
        // `discover` only returns sets that exist on disk under a name; the
        // active config may be neither, e.g. an implicit default or `-c`.
        if !entries.iter().any(|e| e.path == self.config_path) {
            entries.push(SetEntry {
                name: "(unnamed)".into(),
                path: self.config_path.clone(),
            });
        }
        self.set_picker_cursor = entries
            .iter()
            .position(|e| e.path == self.config_path)
            .unwrap_or(0);
        self.set_entries = entries;
        self.set_picker_open = true;
    }

    pub fn close_set_picker(&mut self) {
        self.set_picker_open = false;
    }

    pub fn set_picker_move(&mut self, delta: isize) {
        let n = self.set_entries.len();
        if n == 0 {
            return;
        }
        let next = (self.set_picker_cursor as isize + delta).clamp(0, n as isize - 1) as usize;
        self.set_picker_cursor = next;
    }

    /// Confirm the highlighted set: load its config and switch to it, with a
    /// full re-probe.
    ///
    /// Re-checks [`mutation_blocker`](Self::mutation_blocker) rather than
    /// trusting the check [`open_set_picker`](Self::open_set_picker) made: a
    /// run or an auto-update pass can start while the picker sits open, and
    /// switching sets would hand its in-flight results indices belonging to
    /// the wrong repo list.
    ///
    /// Uses [`config::try_load`] rather than `config::load`, whose
    /// `std::process::exit` would skip teardown and the panic hook and leave
    /// the terminal in raw mode on the alternate screen. An unreadable or
    /// unparseable config keeps the set currently open and reports the error.
    pub fn confirm_set_picker(&mut self) {
        let Some(entry) = self.set_entries.get(self.set_picker_cursor).cloned() else {
            self.close_set_picker();
            return;
        };
        self.close_set_picker();
        if self.refuse_if_mutation_blocked("switch sets") {
            return;
        }
        match config::try_load(&entry.path, self.dir_override.as_deref()) {
            Ok(config::Config {
                repos,
                defaults,
                jobs,
                auto_fetch,
                ..
            }) => {
                self.set_label = entry.name;
                self.reconcile_repos(repos, defaults, (jobs, auto_fetch), entry.path);
            }
            Err(e) => {
                self.status_message = Some(format!("could not switch sets: {e}"));
            }
        }
    }

    /// `Ctrl-R`: re-read the active config from disk without changing which
    /// config is active. Blocked by
    /// [`mutation_blocker`](Self::mutation_blocker), and uses
    /// [`config::try_load`] for the reason
    /// [`confirm_set_picker`](Self::confirm_set_picker) gives.
    pub fn reload_config(&mut self) {
        if self.refuse_if_mutation_blocked("reload") {
            return;
        }
        match config::try_load(&self.config_path, self.dir_override.as_deref()) {
            Ok(config::Config {
                repos,
                defaults,
                jobs,
                auto_fetch,
                ..
            }) => {
                let config_path = self.config_path.clone();
                self.reconcile_repos(repos, defaults, (jobs, auto_fetch), config_path);
            }
            Err(e) => {
                self.status_message = Some(format!("could not reload config: {e}"));
            }
        }
    }

    /// Replace the repo list after a config reload or set switch, carrying
    /// the cursor and selection across by repo NAME rather than index: a
    /// repo added above the one you're on must not silently redirect a
    /// selection onto its neighbour, and a name the edit removed just drops
    /// out. Every index-keyed piece of state (probes, run results, detail
    /// scroll) starts over, since none of it means anything against a
    /// different repo list. `jobs` is re-resolved here too, since the config
    /// just read is entitled to its own answer.
    fn reconcile_repos(
        &mut self,
        repos: Vec<Repo>,
        defaults: BTreeMap<String, String>,
        (jobs, auto_fetch): (Option<usize>, Option<Duration>),
        config_path: PathBuf,
    ) {
        let cursor_name = self.repos.get(self.cursor).map(|r| r.name.clone());
        let selected_names: BTreeSet<String> = self
            .selected
            .iter()
            .filter_map(|&i| self.repos.get(i).map(|r| r.name.clone()))
            .collect();

        let n = repos.len();
        self.actions = actions::discover(&repos, &defaults);
        self.repos = repos;
        self.defaults = defaults;
        self.config_path = config_path;
        self.jobs = config::max_jobs(self.jobs_flag, jobs);

        self.probes = vec![None; n];
        self.probing.clear();
        self.fetched_repos.clear();
        self.fetch_baseline.clear();
        self.run_results = vec![None; n];
        self.detail_scroll.clear();

        self.selected = self.indices_matching(|n| selected_names.contains(n));

        self.cursor = cursor_name
            .and_then(|name| self.index_of_name(&name))
            .unwrap_or(0);
        self.clamp_cursor_to_visible();

        // The config just read is entitled to its own answer, and the session
        // that could outrank it belongs to the set being left behind.
        self.apply_auto_fetch(auto_fetch);
        self.arm_boot_fetch();

        self.full_reprobe_requested = true;
    }

    pub fn take_full_reprobe_request(&mut self) -> bool {
        std::mem::take(&mut self.full_reprobe_requested)
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::app;
    use super::*;

    fn write_config(path: &std::path::Path, body: &str) {
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn reload_config_keeps_the_selection_by_name_and_drops_removed_ones() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".mrconfig");
        write_config(&cfg, "[bar]\n[foo]\n[zzz]\n");

        let config::Config {
            repos, defaults, ..
        } = config::load(&cfg, None);
        let mut a = App::new(repos, "work".into(), 4, defaults, cfg.clone(), false, None);

        let foo_before = a.repos.iter().position(|r| r.name == "foo").unwrap();
        let zzz_before = a.repos.iter().position(|r| r.name == "zzz").unwrap();
        a.selected.insert(foo_before);
        a.selected.insert(zzz_before);
        a.cursor = foo_before;

        // "aab" sorts above every existing name, landing above "foo" in the
        // reloaded list; "zzz" is gone entirely.
        write_config(&cfg, "[aab]\n[bar]\n[foo]\n");
        a.reload_config();

        let names: Vec<&str> = a.repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["aab", "bar", "foo"]);

        let foo_after = a.repos.iter().position(|r| r.name == "foo").unwrap();
        assert_ne!(
            foo_before, foo_after,
            "the repo added above foo must actually have moved its index for this test to mean anything"
        );
        assert_eq!(
            a.selected,
            BTreeSet::from([foo_after]),
            "zzz drops out silently, foo is kept by name rather than by its old index"
        );
        assert_eq!(
            a.cursor, foo_after,
            "the cursor follows foo by name even though a repo was added above it"
        );
        assert!(a.full_reprobe_requested);
    }

    /// A set switch reads a different config, so its `jobs` has to take
    /// effect without a restart; `-j` given on the command line still wins.
    #[test]
    fn a_reload_picks_up_the_configs_jobs_unless_the_flag_set_it() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".mrconfig");
        write_config(&cfg, "[DEFAULT]\njobs = 4\n\n[foo]\n");
        let config::Config {
            repos, defaults, ..
        } = config::load(&cfg, None);
        let mut a = App::new(repos, "work".into(), 4, defaults, cfg.clone(), false, None);

        write_config(&cfg, "[DEFAULT]\njobs = 10\n\n[foo]\n");
        a.reload_config();
        assert_eq!(a.jobs, 10);

        a.jobs_flag = Some(2);
        a.reload_config();
        assert_eq!(a.jobs, 2, "-j outranks the config it just re-read");
    }

    /// The reload has to fall back the same way startup does, or dropping the
    /// key would leave the old value in place with nothing saying so.
    #[test]
    fn removing_the_jobs_key_returns_to_the_cpu_default_on_reload() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".mrconfig");
        write_config(&cfg, "[DEFAULT]\njobs = 10\n\n[foo]\n");
        let config::Config {
            repos, defaults, ..
        } = config::load(&cfg, None);
        let mut a = App::new(repos, "work".into(), 10, defaults, cfg.clone(), false, None);

        write_config(&cfg, "[foo]\n");
        a.reload_config();
        assert_eq!(a.jobs, config::max_jobs(None, None));
    }

    #[test]
    fn reload_config_is_a_no_op_while_a_run_is_live() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".mrconfig");
        write_config(&cfg, "[foo]\n");
        let config::Config {
            repos, defaults, ..
        } = config::load(&cfg, None);
        let mut a = App::new(repos, "work".into(), 4, defaults, cfg.clone(), false, None);
        a.begin_named_run("update".into(), vec![0]);

        write_config(&cfg, "[foo]\n[bar]\n");
        a.reload_config();

        assert_eq!(a.repos.len(), 1, "the reload must not have happened");
        assert!(a.status_message.is_some());
    }

    /// `config::load` calls `std::process::exit(1)` on a bad file, which
    /// would bypass teardown and the panic hook and leave the terminal
    /// wrecked. A bad edit must keep the loaded config and report the error.
    #[test]
    fn reload_config_keeps_the_current_config_when_the_edit_does_not_parse() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join(".mrconfig");
        write_config(&cfg, "[foo]\n[bar]\n");
        let config::Config {
            repos, defaults, ..
        } = config::load(&cfg, None);
        let mut a = App::new(repos, "work".into(), 4, defaults, cfg.clone(), false, None);

        // An unclosed section bracket: invalid INI, fails to parse.
        write_config(&cfg, "[baz\n");
        a.reload_config();

        assert_eq!(
            a.repos.len(),
            2,
            "the previous config must still be loaded, not process::exit(1)"
        );
        let names: Vec<&str> = a.repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["bar", "foo"]);
        assert!(
            a.status_message
                .as_deref()
                .is_some_and(|m| m.contains("reload")),
            "got {:?}",
            a.status_message
        );
    }

    /// Same as above but through the set-picker path.
    #[test]
    fn confirm_set_picker_keeps_the_current_config_when_the_target_does_not_parse() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.mrconfig");
        write_config(&good, "[foo]\n");
        let bad = dir.path().join("bad.mrconfig");
        write_config(&bad, "[baz\n");

        let config::Config {
            repos, defaults, ..
        } = config::load(&good, None);
        let mut a = App::new(repos, "work".into(), 4, defaults, good.clone(), false, None);

        a.set_entries = vec![SetEntry {
            name: "bad".into(),
            path: bad,
        }];
        a.set_picker_cursor = 0;
        a.set_picker_open = true;
        a.confirm_set_picker();

        assert!(!a.set_picker_open);
        assert_eq!(
            a.config_path, good,
            "must not have switched away from the config that parses"
        );
        assert_eq!(a.repos.len(), 1);
        assert!(
            a.status_message
                .as_deref()
                .is_some_and(|m| m.contains("switch")),
            "got {:?}",
            a.status_message
        );
    }

    #[test]
    fn the_set_picker_is_blocked_while_a_run_is_live() {
        let mut a = app(&["foo"]);
        a.begin_named_run("update".into(), vec![0]);
        a.open_set_picker();
        assert!(!a.set_picker_open);
        assert!(a.status_message.is_some());
    }

    /// A poll completing and starting a fast-forward cycle doesn't wait on a
    /// modal, so an auto-update pass can start while the picker sits open.
    #[test]
    fn confirm_set_picker_is_blocked_when_a_run_starts_after_the_picker_opened() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("current.mrconfig");
        write_config(&current, "[foo]\n");
        let other = dir.path().join("other.mrconfig");
        write_config(&other, "[bar]\n");

        let config::Config {
            repos, defaults, ..
        } = config::load(&current, None);
        let mut a = App::new(
            repos,
            "work".into(),
            4,
            defaults,
            current.clone(),
            false,
            None,
        );

        a.set_entries = vec![SetEntry {
            name: "other".into(),
            path: other,
        }];
        a.set_picker_cursor = 0;
        a.set_picker_open = true; // picker opened while nothing was blocking it

        a.begin_named_run("update".into(), vec![0]); // then a poll cycle started one
        a.confirm_set_picker();

        assert!(!a.set_picker_open);
        assert_eq!(
            a.config_path, current,
            "must not have switched sets out from under the live run"
        );
        assert_eq!(a.repos.len(), 1);
        assert!(a.status_message.is_some());
    }

    #[test]
    fn opening_the_set_picker_appends_the_active_config_as_unnamed_when_undiscovered() {
        let mut a = app(&["foo"]); // config_path is /dev/null, never a discovered set
        a.open_set_picker();
        assert!(a.set_picker_open);
        assert!(
            a.set_entries
                .iter()
                .any(|e| e.name == "(unnamed)" && e.path == a.config_path),
            "got {:?}",
            a.set_entries
        );
    }
}
