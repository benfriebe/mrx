pub fn summarize(action: &str, stdout: &str, stderr: &str, exit_code: i32) -> String {
    if exit_code != 0 {
        let msg = error_line(stderr)
            .or_else(|| error_line(stdout))
            .or_else(|| first_meaningful_line(stderr))
            .or_else(|| first_meaningful_line(stdout))
            .unwrap_or_else(|| format!("exit code {}", exit_code));
        return msg;
    }

    match Shape::of(action) {
        Shape::Pull => summarize_pull(stdout, stderr),
        Shape::Status => summarize_status(stdout),
        Shape::Diff => summarize_diff(stdout),
        Shape::Push => summarize_push(stdout, stderr),
        Shape::Fetch => summarize_fetch(stdout, stderr),
        Shape::Clone => summarize_clone(stderr),
        Shape::Silent => String::new(),
        Shape::Generic => summarize_generic(stdout, stderr),
    }
}

/// How an action's output should be read.
///
/// Action names are an open set because `.mrconfig` can define its own, so the
/// name is narrowed here, once. Consumers match on the shape and stay exhaustive,
/// which is what the old `match command` gave us before custom actions existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Pull,
    Status,
    Diff,
    Push,
    Fetch,
    Clone,
    Silent,
    Generic,
}

impl Shape {
    pub fn of(action: &str) -> Self {
        match action {
            "update" => Shape::Pull,
            "status" => Shape::Status,
            "diff" => Shape::Diff,
            "push" => Shape::Push,
            "fetch" => Shape::Fetch,
            "checkout" => Shape::Clone,
            "list" | "register" => Shape::Silent,
            _ => Shape::Generic,
        }
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

fn summarize_generic(stdout: &str, stderr: &str) -> String {
    let lines: Vec<&str> = stdout
        .lines()
        .chain(stderr.lines())
        .filter(|l| !l.trim().is_empty())
        .collect();
    match lines.len() {
        0 => "done".into(),
        1 => lines[0].trim().to_string(),
        n => format!("{} ({}+ lines)", lines[0].trim(), n),
    }
}

/// Name the step whose output the summary came from, when there was more than one.
///
/// A row reading `npm error Missing script: "build"` does not say whether `update`
/// or `post_update` produced it, and those are fixed in different places.
pub fn with_step(step: Option<&str>, summary: String) -> String {
    match step.filter(|s| !s.is_empty()) {
        Some(step) => format!("{}: {}", step, summary),
        None => summary,
    }
}

/// The line most likely to say why a step failed.
///
/// Tools that fail loudly still warn first: npm prints screenfuls of
/// `npm warn ERESOLVE ...` before `npm error Missing script: "build"`, so taking
/// the first line reports a warning as the cause. Prefer a line that names an
/// error, and never one that names a warning.
fn error_line(s: &str) -> Option<String> {
    const ERROR: [&str; 5] = ["error", "fatal:", "err!", "cannot ", "failed"];
    const WARNING: [&str; 3] = ["warn", "deprecat", "notice"];

    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .find(|l| {
            let lower = l.to_ascii_lowercase();
            ERROR.iter().any(|m| lower.contains(m)) && !WARNING.iter().any(|m| lower.contains(m))
        })
        .map(truncate)
}

fn first_meaningful_line(s: &str) -> Option<String> {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(truncate)
}

/// Counted in chars, not bytes: tool output is arbitrary UTF-8 and slicing it at a
/// byte offset panics mid-codepoint.
fn truncate(line: &str) -> String {
    const MAX: usize = 80;
    if line.chars().count() <= MAX {
        return line.to_string();
    }
    let head: String = line.chars().take(MAX - 3).collect();
    format!("{}...", head)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_actions_map_to_their_own_shape() {
        assert_eq!(Shape::of("update"), Shape::Pull);
        assert_eq!(Shape::of("status"), Shape::Status);
        assert_eq!(Shape::of("diff"), Shape::Diff);
        assert_eq!(Shape::of("push"), Shape::Push);
        assert_eq!(Shape::of("fetch"), Shape::Fetch);
        assert_eq!(Shape::of("checkout"), Shape::Clone);
        assert_eq!(Shape::of("list"), Shape::Silent);
        assert_eq!(Shape::of("register"), Shape::Silent);
    }

    #[test]
    fn unknown_actions_fall_back_to_generic() {
        assert_eq!(Shape::of("deploy"), Shape::Generic);
        assert_eq!(Shape::of("run"), Shape::Generic);
        assert_eq!(Shape::of(""), Shape::Generic);
    }

    #[test]
    fn aliases_are_normalised_before_they_get_here() {
        // cli::Command::display_name maps pull -> update, co -> checkout, ls -> list,
        // so the bare alias is not a name summarize ever sees.
        assert_eq!(Shape::of("pull"), Shape::Generic);
    }

    #[test]
    fn a_failure_reports_its_error_whatever_the_shape() {
        assert_eq!(
            summarize("status", "", "fatal: not a repo\n", 128),
            "fatal: not a repo"
        );
        assert_eq!(summarize("deploy", "", "", 3), "exit code 3");
    }

    #[test]
    fn a_failure_skips_the_warnings_that_came_first() {
        let stderr = "npm warn ERESOLVE overriding peer dependency\n\
                      npm warn deprecated inflight@1.0.6\n\
                      npm error Missing script: \"build\"\n";
        assert_eq!(
            summarize("update", "", stderr, 1),
            "npm error Missing script: \"build\""
        );
    }

    #[test]
    fn a_failure_with_nothing_error_shaped_still_says_something() {
        assert_eq!(
            summarize("update", "", "husky - install command is DEPRECATED\n", 1),
            "husky - install command is DEPRECATED"
        );
    }

    #[test]
    fn a_failure_reads_stdout_when_the_error_went_there() {
        assert_eq!(
            summarize("update", "npm error code ENOENT\n", "some warning\n", 254),
            "npm error code ENOENT"
        );
    }

    #[test]
    fn only_a_chain_names_the_step_that_broke() {
        assert_eq!(
            with_step(Some("post_update"), "npm error code ENOENT".into()),
            "post_update: npm error code ENOENT"
        );
        assert_eq!(with_step(None, "done".into()), "done");
        assert_eq!(with_step(Some(""), "done".into()), "done");
    }

    #[test]
    fn a_long_line_is_cut_on_a_char_boundary() {
        let line = "✗".repeat(200);
        let cut = summarize("update", "", &line, 1);
        assert_eq!(cut.chars().count(), 80);
        assert!(cut.ends_with("..."));
    }
}
