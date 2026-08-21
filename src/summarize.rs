use crate::executor::StepResult;

/// How a step's output should be read. `operations::plan` decides this, since
/// it is the only place that knows whether a step is a built-in git call, a
/// config-defined body, or a `post_` hook. Do not re-derive it here from the
/// action's name: that parses a custom `status` body as `git status` output.
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

/// Summarise one step's output into a short, shape-aware description.
///
/// `stdout`/`stderr` may carry ANSI escapes (forced on so ui mode can show
/// colour), so this strips them once, up front. Every prefix match, substring
/// match, and char-counted truncation below assumes plain text; the
/// shape-specific parsers rely on that rather than stripping again.
pub fn summarize(shape: Shape, stdout: &str, stderr: &str, exit_code: i32) -> String {
    let stdout = crate::ansi::strip(stdout);
    let stderr = crate::ansi::strip(stderr);
    let stdout = stdout.as_str();
    let stderr = stderr.as_str();

    if exit_code != 0 {
        let msg = error_line(stderr)
            .or_else(|| error_line(stdout))
            .or_else(|| first_meaningful_line(stderr))
            .or_else(|| first_meaningful_line(stdout))
            .unwrap_or_else(|| format!("exit code {exit_code}"));
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
/// The chain stops at the first failing step, so the last step present always
/// decided the outcome: its shape and output are what the row shows.
/// Concatenating every step's output instead would summarise a `post_update`
/// that ran after a no-op pull as "already up to date".
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

/// Whether a finished run left everything as it found it, read from the
/// literals the summarisers below return for exactly that case. It lives beside
/// them because it is the only thing that has to stay in step with their
/// wording: RESULT sorts on it, and a phrase reworded above would otherwise
/// quietly stop being recognised here.
pub fn changed_nothing(steps: &[StepResult], exit_code: i32) -> bool {
    let Some(last) = steps.last() else {
        return true;
    };
    matches!(
        summarize(last.shape, &last.stdout, &last.stderr, exit_code).as_str(),
        "already up to date" | "clean" | "no changes" | "up to date" | "done"
    )
}

fn summarize_pull(stdout: &str, stderr: &str) -> String {
    let combined = format!("{stdout}\n{stderr}");
    if combined.contains("Already up to date") || combined.contains("Already up-to-date") {
        return "already up to date".into();
    }
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

/// What the buckets in a status summary count. `Untracked` also covers a
/// staged add, which is the same thing one `git add` later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    Modified,
    Untracked,
    Deleted,
}

fn summarize_status(stdout: &str) -> String {
    // `--branch` prepends `## main...origin/main`, which is not a file: left
    // in the count it reports every clean repo as "1 changed".
    let files: Vec<&str> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("##"))
        .collect();
    if files.is_empty() {
        return "clean".into();
    }
    let parts: Vec<String> = [
        (Change::Modified, "modified"),
        (Change::Untracked, "untracked"),
        (Change::Deleted, "deleted"),
    ]
    .into_iter()
    .filter_map(|(kind, label)| {
        match files
            .iter()
            .filter(|l| change_kind(l) == Some(kind))
            .count()
        {
            0 => None,
            n => Some(format!("{n} {label}")),
        }
    })
    .collect();
    if parts.is_empty() {
        format!("{} changed", files.len())
    } else {
        parts.join(", ")
    }
}

/// Which bucket a short-format line falls in, read from both status columns
/// (staged, then unstaged) rather than one fixed prefix, so a file that was
/// staged and then edited again (`MM`) counts as modified instead of falling
/// through to the untyped "changed" total. A line counts once, under the
/// first column that says anything.
fn change_kind(line: &str) -> Option<Change> {
    let mut columns = line.chars();
    let (staged, unstaged) = (columns.next()?, columns.next()?);
    if (staged, unstaged) == ('?', '?') {
        return Some(Change::Untracked);
    }
    [staged, unstaged].into_iter().find_map(|c| match c {
        'M' | 'R' | 'C' => Some(Change::Modified),
        'A' => Some(Change::Untracked),
        'D' => Some(Change::Deleted),
        _ => None,
    })
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
    let combined = format!("{stdout}\n{stderr}");
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
        Some(step) => format!("{step}: {summary}"),
        None => summary,
    }
}

/// The line most likely to say why a step failed.
///
/// Tools that fail loudly still warn first: npm prints screenfuls of
/// `npm warn ERESOLVE ...` before `npm error Missing script: "build"`, so the
/// first line reports a warning as the cause. Prefer a line naming an error,
/// never one naming a warning.
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
    format!("{head}...")
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
    fn coloured_status_output_still_parses_the_porcelain_markers() {
        // SGR codes around the markers, as forced-colour `git status --short` emits them.
        let stdout = "\u{1b}[31m M\u{1b}[0m file.txt\n\u{1b}[32m??\u{1b}[0m new.txt\n";
        assert_eq!(
            summarize(Shape::Status, stdout, "", 0),
            "1 modified, 1 untracked"
        );
    }

    #[test]
    fn the_branch_header_is_not_counted_as_a_changed_file() {
        assert_eq!(
            summarize(Shape::Status, "## main...origin/main\n", "", 0),
            "clean"
        );
        assert_eq!(
            summarize(Shape::Status, "## main...origin/main\n M file.txt\n", "", 0),
            "1 modified"
        );
    }

    #[test]
    fn a_file_both_staged_and_edited_counts_as_modified() {
        assert_eq!(
            summarize(Shape::Status, "MM file.txt\n", "", 0),
            "1 modified"
        );
        assert_eq!(
            summarize(Shape::Status, "AM new.txt\n", "", 0),
            "1 untracked"
        );
    }

    #[test]
    fn coloured_pull_output_still_matches_the_up_to_date_phrase() {
        let stdout = "\u{1b}[32mAlready up to date.\u{1b}[0m\n";
        assert_eq!(summarize(Shape::Pull, stdout, "", 0), "already up to date");
    }

    #[test]
    fn coloured_failure_output_still_matches_the_error_phrase() {
        let stderr = "\u{1b}[31mnpm error code ENOENT\u{1b}[0m\n";
        assert_eq!(
            summarize(Shape::Pull, "", stderr, 254),
            "npm error code ENOENT"
        );
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

    /// The RESULT column sorts on this, so each summariser's own "nothing
    /// happened" wording has to keep being recognised. A phrase reworded above
    /// without this list following it fails here rather than quietly ranking
    /// every repo as having changed.
    #[test]
    fn a_run_that_left_everything_alone_is_recognised_whatever_shape_said_so() {
        let quiet = [
            (Shape::Pull, "Already up to date.\n"),
            (Shape::Status, ""),
            (Shape::Diff, ""),
            (Shape::Fetch, ""),
            (Shape::Generic, ""),
        ];
        for (shape, stdout) in quiet {
            let steps = vec![step("git", shape, stdout, 0)];
            assert!(
                changed_nothing(&steps, 0),
                "{shape:?} summarised as {:?}",
                summarize_steps(&steps, 0)
            );
        }

        assert!(
            changed_nothing(&[], 0),
            "a run with no steps did nothing by definition"
        );

        let moved = vec![step("git", Shape::Status, " M src/main.rs\n", 0)];
        assert!(!changed_nothing(&moved, 0));
    }

    #[test]
    fn a_slow_post_update_after_an_up_to_date_pull_summarises_as_the_post_steps_result() {
        // Concatenating every step's output let the pull's "Already up to date"
        // win over the post_update that ran afterwards and actually did something.
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

        // Guessing the shape from the action name instead reads one line of
        // non-porcelain output as one changed file.
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
