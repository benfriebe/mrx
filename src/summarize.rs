use crate::executor::StepResult;

/// How a step's output should be read: `operations::plan` decides this, since
/// it is the only place that knows whether a step is a built-in git call, a
/// config-defined body, or a `post_` hook, rather than it being guessed here
/// from the action's name (which is what used to make a custom `status`
/// action get parsed as if it were `git status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Pull,
    Status,
    Diff,
    Push,
    Fetch,
    Clone,
    Generic,
}

pub fn summarize(shape: Shape, stdout: &str, stderr: &str, exit_code: i32) -> String {
    if exit_code != 0 {
        let msg = error_line(stderr)
            .or_else(|| error_line(stdout))
            .or_else(|| first_meaningful_line(stderr))
            .or_else(|| first_meaningful_line(stdout))
            .unwrap_or_else(|| format!("exit code {}", exit_code));
        return msg;
    }

    match shape {
        Shape::Pull => summarize_pull(stdout, stderr),
        Shape::Status => summarize_status(stdout),
        Shape::Diff => summarize_diff(stdout),
        Shape::Push => summarize_push(stdout, stderr),
        Shape::Fetch => summarize_fetch(stdout, stderr),
        Shape::Clone => summarize_clone(stderr),
        Shape::Generic => summarize_generic(stdout, stderr),
    }
}

/// Summarise a finished run from its steps.
///
/// The chain stops at the first failing step, so the last step present is
/// always the one that decided the outcome, whichever way it went: its shape
/// and output are what the row shows. Selecting by shape rather than parsing
/// a concatenation of every step's output is what keeps a `post_update` that
/// runs long after a no-op pull from being summarised as "already up to
/// date".
pub fn summarize_steps(steps: &[StepResult], exit_code: i32) -> String {
    let Some(last) = steps.last() else {
        return "done".into();
    };
    let label = (steps.len() > 1).then_some(last.label.as_str());
    with_step(
        label,
        summarize(last.shape, &last.stdout, &last.stderr, exit_code),
    )
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
    fn a_failure_reports_its_error_whatever_the_shape() {
        assert_eq!(
            summarize(Shape::Status, "", "fatal: not a repo\n", 128),
            "fatal: not a repo"
        );
        assert_eq!(summarize(Shape::Generic, "", "", 3), "exit code 3");
    }

    #[test]
    fn a_failure_skips_the_warnings_that_came_first() {
        let stderr = "npm warn ERESOLVE overriding peer dependency\n\
                      npm warn deprecated inflight@1.0.6\n\
                      npm error Missing script: \"build\"\n";
        assert_eq!(
            summarize(Shape::Pull, "", stderr, 1),
            "npm error Missing script: \"build\""
        );
    }

    #[test]
    fn a_failure_with_nothing_error_shaped_still_says_something() {
        assert_eq!(
            summarize(
                Shape::Pull,
                "",
                "husky - install command is DEPRECATED\n",
                1
            ),
            "husky - install command is DEPRECATED"
        );
    }

    #[test]
    fn a_failure_reads_stdout_when_the_error_went_there() {
        assert_eq!(
            summarize(
                Shape::Pull,
                "npm error code ENOENT\n",
                "some warning\n",
                254
            ),
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
        let cut = summarize(Shape::Pull, "", &line, 1);
        assert_eq!(cut.chars().count(), 80);
        assert!(cut.ends_with("..."));
    }

    fn step(label: &str, shape: Shape, stdout: &str, code: i32) -> StepResult {
        StepResult {
            label: label.into(),
            shape,
            stdout: stdout.into(),
            stderr: String::new(),
            code,
        }
    }

    #[test]
    fn a_slow_post_update_after_an_up_to_date_pull_summarises_as_the_post_steps_result() {
        // The bug this fixes: concatenating every step's output made "Already up
        // to date" from the pull win the summary even though post_update, which
        // ran afterwards and actually did something, is what the row should say.
        let steps = vec![
            step("git pull", Shape::Pull, "Already up to date.\n", 0),
            step("post_update", Shape::Generic, "wrote 3 files\n", 0),
        ];
        assert_eq!(summarize_steps(&steps, 0), "post_update: wrote 3 files");
    }

    #[test]
    fn a_config_defined_status_body_keeps_its_own_output_instead_of_the_porcelain_parser() {
        let steps = vec![step("status", Shape::Generic, "OK: 3 services up\n", 0)];
        assert_eq!(summarize_steps(&steps, 0), "OK: 3 services up");

        // What used to happen when the shape was guessed from the action name:
        // one line of output that doesn't look like porcelain still gets read as
        // one changed file.
        assert_eq!(
            summarize(Shape::Status, "OK: 3 services up\n", "", 0),
            "1 changed"
        );
    }

    #[test]
    fn a_single_step_run_is_summarised_without_a_label() {
        let steps = vec![step("git pull", Shape::Pull, "Already up to date.\n", 0)];
        assert_eq!(summarize_steps(&steps, 0), "already up to date");
    }

    #[test]
    fn a_failing_step_is_named_the_same_way_a_finishing_one_is() {
        let steps = vec![
            step("git pull", Shape::Pull, "Already up to date.\n", 0),
            StepResult {
                label: "post_update".into(),
                shape: Shape::Generic,
                stdout: String::new(),
                stderr: "npm error code ENOENT\n".into(),
                code: 1,
            },
        ];
        assert_eq!(
            summarize_steps(&steps, 1),
            "post_update: npm error code ENOENT"
        );
    }
}
