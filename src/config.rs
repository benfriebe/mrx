use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
    pub clone_url: Option<String>,
    pub keys: BTreeMap<String, String>,
}

pub fn parse_config(config_path: &Path, base_dir: &Path) -> (Vec<Repo>, BTreeMap<String, String>) {
    let content = match std::fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (Vec::new(), BTreeMap::new());
        }
        Err(e) => {
            eprintln!("error: cannot read {}: {}", config_path.display(), e);
            std::process::exit(1);
        }
    };

    let mut ini = configparser::ini::Ini::new();
    if let Err(e) = ini.read(content) {
        eprintln!("error: cannot parse {}: {}", config_path.display(), e);
        std::process::exit(1);
    }

    let mut repos: Vec<Repo> = Vec::new();
    let mut defaults: BTreeMap<String, String> = BTreeMap::new();

    for section in ini.sections() {
        let mut keys: BTreeMap<String, String> = BTreeMap::new();
        if let Some(map) = ini.get_map_ref().get(&section) {
            for (k, v) in map {
                if let Some(val) = v {
                    keys.insert(k.clone(), val.clone());
                }
            }
        }

        // configparser lowercases section names by default; [DEFAULT] becomes "default".
        if section.eq_ignore_ascii_case("default") {
            defaults = keys;
            continue;
        }

        let name = section.rsplit('/').next().unwrap_or(&section).to_string();
        let abs_path = base_dir.join(&section);
        let clone_url = keys.get("checkout").and_then(|cmd| extract_clone_url(cmd));

        repos.push(Repo {
            name,
            path: abs_path,
            clone_url,
            keys,
        });
    }

    repos.sort_by(|a, b| a.name.cmp(&b.name));
    (repos, defaults)
}

fn extract_clone_url(checkout_cmd: &str) -> Option<String> {
    let tokens: Vec<&str> = checkout_cmd.split_whitespace().collect();
    // find "clone" then take the next token as the URL
    for (i, tok) in tokens.iter().enumerate() {
        if *tok == "clone" {
            if let Some(url) = tokens.get(i + 1) {
                return Some(url.trim_matches('\'').trim_matches('"').to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_clone_url_https() {
        let cmd = "git clone 'https://github.com/mr-yum/bill-api' 'bill-api'";
        assert_eq!(
            extract_clone_url(cmd),
            Some("https://github.com/mr-yum/bill-api".to_string())
        );
    }

    #[test]
    fn test_extract_clone_url_ssh() {
        let cmd = "git clone 'git@github.com:mr-yum/cli.git' 'cli'";
        assert_eq!(
            extract_clone_url(cmd),
            Some("git@github.com:mr-yum/cli.git".to_string())
        );
    }

    #[test]
    fn test_parse_config_with_defaults_and_custom_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().join(".mrconfig");
        std::fs::write(
            &cfg,
            "[DEFAULT]\nupdate = git pull --rebase\n\n\
             [git/foo]\ncheckout = git clone 'git@github.com:example/foo.git' 'foo'\n\
             main = git checkout master\ninstall = npm install\n",
        )
        .unwrap();

        let (repos, defaults) = parse_config(&cfg, dir.path());

        assert_eq!(repos.len(), 1);
        assert_eq!(
            defaults.get("update").map(String::as_str),
            Some("git pull --rebase")
        );

        let foo = &repos[0];
        assert_eq!(foo.name, "foo");
        assert_eq!(
            foo.clone_url.as_deref(),
            Some("git@github.com:example/foo.git")
        );
        assert_eq!(
            foo.keys.get("main").map(String::as_str),
            Some("git checkout master")
        );
        assert_eq!(
            foo.keys.get("install").map(String::as_str),
            Some("npm install")
        );
        assert!(foo.keys.contains_key("checkout"));
    }
}
