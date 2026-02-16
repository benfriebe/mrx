use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
    pub clone_url: Option<String>,
}

pub fn parse_config(config_path: &Path, base_dir: &Path) -> Vec<Repo> {
    let content = match std::fs::read_to_string(config_path) {
        Ok(c) => c,
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

    for section in ini.sections() {
        let name = section.rsplit('/').next().unwrap_or(&section).to_string();

        let abs_path = base_dir.join(&section);

        let clone_url = ini
            .get(&section, "checkout")
            .and_then(|cmd| extract_clone_url(&cmd));

        repos.push(Repo {
            name,
            path: abs_path,
            clone_url,
        });
    }

    repos.sort_by(|a, b| a.name.cmp(&b.name));
    repos
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
}
