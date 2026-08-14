//! Discovering runnable actions from a config, so a palette can turn
//! `.mrconfig` into a menu instead of the CLI's "only if you already know
//! the name" (section 08).

use crate::config::Repo;
use std::collections::{BTreeMap, BTreeSet};

/// Where an action is defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// One of mrx's own verbs: update, status, diff, push, fetch, checkout.
    Builtin,
    /// A `[DEFAULT]` key, so every repo in the set can run it.
    Default,
    /// A key defined on one or more repo sections but not `[DEFAULT]`.
    PerRepo,
}

/// One runnable action: its name, where it comes from, and how many repos in
/// the current set actually define it. Showing `repos` alongside `source` is
/// what makes an unfamiliar name trustworthy: "deploy, per-repo, 3 of 42"
/// says up front that running it against the full selection skips 39 repos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub name: String,
    pub source: Source,
    pub repos: usize,
}

/// mrx's own verbs, runnable against a selection the same way a custom
/// action is. `checkout` is here rather than discovered from the config so a
/// repo's own `checkout` key isn't listed twice (section 08).
const BUILTIN_VERBS: &[&str] = &["update", "status", "diff", "push", "fetch", "checkout"];

/// Every runnable action for a config: the built-in verbs, plus every key
/// visible in `[DEFAULT]` or any repo section, minus `post_` hooks (run as
/// part of the action they follow, not on their own) and the built-ins
/// themselves. `RESERVED_KEYS` (`base`, `skip`) never reach here: `config::load`
/// already strips them.
pub fn discover(repos: &[Repo], defaults: &BTreeMap<String, String>) -> Vec<Action> {
    let mut actions: Vec<Action> = BUILTIN_VERBS
        .iter()
        .map(|&name| Action {
            name: name.to_string(),
            source: Source::Builtin,
            repos: repos.len(),
        })
        .collect();

    let mut names: BTreeSet<&str> = BTreeSet::new();
    names.extend(defaults.keys().map(String::as_str));
    for repo in repos {
        names.extend(repo.keys.keys().map(String::as_str));
    }

    for name in names {
        if name.starts_with("post_") || BUILTIN_VERBS.contains(&name) {
            continue;
        }
        if defaults.contains_key(name) {
            actions.push(Action {
                name: name.to_string(),
                source: Source::Default,
                repos: repos.len(),
            });
        } else {
            let repos = repos.iter().filter(|r| r.keys.contains_key(name)).count();
            actions.push(Action {
                name: name.to_string(),
                source: Source::PerRepo,
                repos,
            });
        }
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn repo(name: &str, keys: &[(&str, &str)]) -> Repo {
        Repo {
            name: name.to_string(),
            path: PathBuf::from(format!("/nonexistent/{name}")),
            clone_url: None,
            keys: keys
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn find<'a>(actions: &'a [Action], name: &str) -> &'a Action {
        actions
            .iter()
            .find(|a| a.name == name)
            .unwrap_or_else(|| panic!("no action named {name}"))
    }

    #[test]
    fn builtin_verbs_are_always_present() {
        let actions = discover(&[], &BTreeMap::new());
        for verb in BUILTIN_VERBS {
            let action = find(&actions, verb);
            assert_eq!(action.source, Source::Builtin);
        }
    }

    #[test]
    fn a_default_key_is_runnable_across_every_repo() {
        let repos = vec![repo("a", &[]), repo("b", &[])];
        let defaults = BTreeMap::from([("lint".to_string(), "eslint .".to_string())]);

        let actions = discover(&repos, &defaults);
        let action = find(&actions, "lint");
        assert_eq!(action.source, Source::Default);
        assert_eq!(action.repos, 2);
    }

    #[test]
    fn a_per_repo_key_counts_only_the_repos_that_define_it() {
        let repos = vec![
            repo("a", &[("deploy", "./deploy.sh")]),
            repo("b", &[]),
            repo("c", &[("deploy", "./deploy.sh")]),
        ];

        let actions = discover(&repos, &BTreeMap::new());
        let action = find(&actions, "deploy");
        assert_eq!(action.source, Source::PerRepo);
        assert_eq!(action.repos, 2);
    }

    #[test]
    fn post_hooks_are_not_actions_of_their_own() {
        let repos = vec![repo("a", &[("post_update", "./setup.sh")])];
        let actions = discover(&repos, &BTreeMap::new());
        assert!(actions.iter().all(|a| a.name != "post_update"));
    }

    #[test]
    fn a_checkout_body_does_not_duplicate_the_builtin_verb() {
        let repos = vec![repo("a", &[("checkout", "git clone x a")])];
        let actions = discover(&repos, &BTreeMap::new());
        assert_eq!(actions.iter().filter(|a| a.name == "checkout").count(), 1);
    }
}
