use crate::cli::Command;

pub fn summarize(command: &Command, stdout: &str, stderr: &str, exit_code: i32) -> String {
    if exit_code != 0 {
        // Try to extract a useful error message
        let msg = first_meaningful_line(stderr)
            .or_else(|| first_meaningful_line(stdout))
            .unwrap_or_else(|| format!("exit code {}", exit_code));
        return msg;
    }

    match command {
        Command::Update | Command::Pull => summarize_pull(stdout, stderr),
        Command::Status => summarize_status(stdout),
        Command::Diff => summarize_diff(stdout),
        Command::Push => summarize_push(stdout, stderr),
        Command::Fetch => summarize_fetch(stdout, stderr),
        Command::Checkout | Command::Co => summarize_clone(stderr),
        Command::Run { .. } => summarize_run(stdout),
        Command::List | Command::Ls | Command::Register => String::new(),
    }
}

fn summarize_pull(stdout: &str, stderr: &str) -> String {
    let combined = format!("{}\n{}", stdout, stderr);
    if combined.contains("Already up to date") || combined.contains("Already up-to-date") {
        return "already up to date".into();
    }
    // Look for "X files changed" summary
    for line in stdout.lines().chain(stderr.lines()) {
        if line.contains("files changed")
            || line.contains("file changed")
            || line.contains("insertions")
            || line.contains("deletions")
        {
            return line.trim().to_string();
        }
    }
    if stdout.trim().is_empty() && stderr.trim().is_empty() {
        "done".into()
    } else {
        first_meaningful_line(stdout).unwrap_or_else(|| "done".into())
    }
}

fn summarize_status(stdout: &str) -> String {
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return "clean".into();
    }
    let modified = lines
        .iter()
        .filter(|l| l.starts_with(" M") || l.starts_with("M "))
        .count();
    let added = lines
        .iter()
        .filter(|l| l.starts_with("A ") || l.starts_with("??"))
        .count();
    let deleted = lines
        .iter()
        .filter(|l| l.starts_with(" D") || l.starts_with("D "))
        .count();
    let mut parts = Vec::new();
    if modified > 0 {
        parts.push(format!("{} modified", modified));
    }
    if added > 0 {
        parts.push(format!("{} untracked", added));
    }
    if deleted > 0 {
        parts.push(format!("{} deleted", deleted));
    }
    if parts.is_empty() {
        format!("{} changed", lines.len())
    } else {
        parts.join(", ")
    }
}

fn summarize_diff(stdout: &str) -> String {
    if stdout.trim().is_empty() {
        return "no changes".into();
    }
    let plus = stdout
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .count();
    let minus = stdout
        .lines()
        .filter(|l| l.starts_with('-') && !l.starts_with("---"))
        .count();
    let files: std::collections::HashSet<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("diff --git"))
        .collect();
    format!("{} files, +{} -{}", files.len(), plus, minus)
}

fn summarize_push(stdout: &str, stderr: &str) -> String {
    let combined = format!("{}\n{}", stdout, stderr);
    if combined.contains("Everything up-to-date") {
        return "up to date".into();
    }
    for line in stderr.lines().chain(stdout.lines()) {
        if line.contains("->") {
            return line.trim().to_string();
        }
    }
    "done".into()
}

fn summarize_fetch(stdout: &str, stderr: &str) -> String {
    if stdout.trim().is_empty() && stderr.trim().is_empty() {
        return "up to date".into();
    }
    let new_refs: Vec<&str> = stderr.lines().filter(|l| l.contains("->")).collect();
    if new_refs.is_empty() {
        "up to date".into()
    } else {
        format!("{} updated refs", new_refs.len())
    }
}

fn summarize_clone(stderr: &str) -> String {
    if stderr.contains("Cloning into") {
        "cloned".into()
    } else {
        "done".into()
    }
}

fn summarize_run(stdout: &str) -> String {
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    match lines.len() {
        0 => "done (no output)".into(),
        1 => lines[0].trim().to_string(),
        n => format!("{} ({}+ lines)", lines[0].trim(), n),
    }
}

fn first_meaningful_line(s: &str) -> Option<String> {
    s.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| {
            if l.len() > 80 {
                format!("{}...", &l[..77])
            } else {
                l.to_string()
            }
        })
}
