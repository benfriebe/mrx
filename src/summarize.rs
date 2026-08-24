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

/// Whether a step left the repo as it found it.
///
/// Decided by the summariser that phrases the step rather than read back off
/// the phrase, so the wording and the conclusion cannot disagree. Reading it
/// back filed `echo done`, or a linter printing `clean`, as having changed
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Quiet,
    Changed,
}

/// A step's summary and what it amounts to.
struct Summary {
    text: String,
    verdict: Verdict,
}

impl Summary {
    fn quiet(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            verdict: Verdict::Quiet,
        }
    }

    fn changed(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            verdict: Verdict::Changed,
        }
    }
}

/// Summarise one step's output into a short, shape-aware description.
///
/// `stdout`/`stderr` may carry ANSI escapes (forced on so ui mode can show
/// colour), so this strips them once up front. Everything below, the
/// shape-specific parsers included, assumes plain text.
pub fn summarize(shape: Shape, stdout: &str, stderr: &str, exit_code: i32) -> String {
    summarize_full(shape, stdout, stderr, exit_code).text
}

fn summarize_full(shape: Shape, stdout: &str, stderr: &str, exit_code: i32) -> Summary {
    let stdout = crate::ansi::strip(stdout);
    let stderr = crate::ansi::strip(stderr);
    let stdout = stdout.as_str();
    let stderr = stderr.as_str();

    if exit_code != 0 {
        // Only successful steps are ever asked for a verdict, so a failure
        // takes the one that keeps it out of the quiet bucket.
        let msg = error_line(stderr)
            .or_else(|| error_line(stdout))
            .or_else(|| first_meaningful_line(stderr))
            .or_else(|| first_meaningful_line(stdout))
            .unwrap_or_else(|| format!("exit code {exit_code}"));
        return Summary::changed(msg);
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
/// The chain stops at the first failing step, so the last step present decided
/// the outcome, and its shape and output are what the row shows.
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

/// Whether a finished run left everything as it found it, from the verdict the
/// step's own summariser reached. RESULT sorts on it, so a run that did
/// something ranks above the ones that had nothing to do.
pub fn changed_nothing(steps: &[StepResult], exit_code: i32) -> bool {
    let Some(last) = steps.last() else {
        return true;
    };
    summarize_full(last.shape, &last.stdout, &last.stderr, exit_code).verdict == Verdict::Quiet
}

fn summarize_pull(stdout: &str, stderr: &str) -> Summary {
    let combined = format!("{stdout}\n{stderr}");
    if combined.contains("Already up to date") || combined.contains("Already up-to-date") {
        return Summary::quiet("already up to date");
    }
    for line in stdout.lines().chain(stderr.lines()) {
        if line.contains("files changed")
            || line.contains("file changed")
            || line.contains("insertions")
            || line.contains("deletions")
        {
            return Summary::changed(line.trim());
        }
    }
    if stdout.trim().is_empty() && stderr.trim().is_empty() {
        Summary::quiet("done")
    } else {
        first_meaningful_line(stdout).map_or_else(|| Summary::quiet("done"), Summary::changed)
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

/// A working tree in words: `clean`, `2 modified, 1 untracked`, or a bare
/// total when everything in it falls outside the three buckets (unmerged,
/// ignored).
///
/// The STATE column shares this phrasing, counting the same changes off
/// porcelain v2 rather than `git status --short`. Only the parsing is written
/// twice, so the column and an `s` run's RESULT cannot word one repo
/// differently.
pub fn working_tree(modified: usize, untracked: usize, deleted: usize, total: usize) -> String {
    if total == 0 {
        return "clean".into();
    }
    let parts: Vec<String> = [
        (modified, "modified"),
        (untracked, "untracked"),
        (deleted, "deleted"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, label)| format!("{count} {label}"))
    .collect();
    if parts.is_empty() {
        format!("{total} changed")
    } else {
        parts.join(", ")
    }
}

fn summarize_status(stdout: &str) -> Summary {
    // `--branch` prepends `## main...origin/main`, which is not a file: left
    // in the count it reports every clean repo as "1 changed".
    let files: Vec<&str> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("##"))
        .collect();
    let count = |kind| {
        files
            .iter()
            .filter(|l| change_kind(l) == Some(kind))
            .count()
    };
    let text = working_tree(
        count(Change::Modified),
        count(Change::Untracked),
        count(Change::Deleted),
        files.len(),
    );
    if files.is_empty() {
        Summary::quiet(text)
    } else {
        Summary::changed(text)
    }
}

/// Which bucket a short-format line falls in, read from both status columns
/// (staged, then unstaged) so a file staged and then edited again (`MM`) counts
/// as modified. A line counts once, under the first column that says anything.
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

fn summarize_diff(stdout: &str) -> Summary {
    if stdout.trim().is_empty() {
        return Summary::quiet("no changes");
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
    Summary::changed(format!("{} files, +{} -{}", files.len(), plus, minus))
}

fn summarize_push(stdout: &str, stderr: &str) -> Summary {
    let combined = format!("{stdout}\n{stderr}");
    if combined.contains("Everything up-to-date") {
        return Summary::quiet("up to date");
    }
    for line in stderr.lines().chain(stdout.lines()) {
        if line.contains("->") {
            return Summary::changed(line.trim());
        }
    }
    Summary::quiet("done")
}

fn summarize_fetch(stdout: &str, stderr: &str) -> Summary {
    if stdout.trim().is_empty() && stderr.trim().is_empty() {
        return Summary::quiet("up to date");
    }
    let new_refs: Vec<&str> = stderr.lines().filter(|l| l.contains("->")).collect();
    if new_refs.is_empty() {
        Summary::quiet("up to date")
    } else {
        Summary::changed(format!("{} updated refs", new_refs.len()))
    }
}

fn summarize_clone(stderr: &str) -> Summary {
    if stderr.contains("Cloning into") {
        Summary::changed("cloned")
    } else {
        Summary::quiet("done")
    }
}

/// A config-defined body's own output. Only silence counts as quiet: mrx has
/// no idea what the body does, so a line it happens to have printed is not
/// evidence that nothing moved, whatever the line says.
fn summarize_generic(stdout: &str, stderr: &str) -> Summary {
    let lines: Vec<&str> = stdout
        .lines()
        .chain(stderr.lines())
        .filter(|l| !l.trim().is_empty())
        .collect();
    match lines.len() {
        0 => Summary::quiet("done"),
        1 => Summary::changed(lines[0].trim()),
        n => Summary::changed(format!("{} ({}+ lines)", lines[0].trim(), n)),
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

/// The line most likely to say why a step failed: one naming an error, never
/// one naming a warning. npm prints screenfuls of `npm warn ERESOLVE ...`
/// before `npm error Missing script: "build"`, so taking the first line would
/// report a warning as the cause.
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

    /// The RESULT column sorts on this, so every shape has to reach a verdict
    /// on its own no-op rather than only the ones anyone thought to check.
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

    /// mrx does not know what a config-defined body does, so a line it printed
    /// is not evidence that nothing moved. These read as no-ops while the
    /// verdict was recovered from the wording, and ranked with the repos that
    /// genuinely had nothing to do.
    #[test]
    fn a_body_printing_a_word_mrx_uses_has_still_done_something() {
        for output in ["done\n", "clean\n", "no changes\n", "up to date\n"] {
            let steps = vec![step("check", Shape::Generic, output, 0)];
            assert!(
                !changed_nothing(&steps, 0),
                "a body printing {output:?} was filed as having changed nothing"
            );
        }

        let silent = vec![step("check", Shape::Generic, "", 0)];
        assert!(
            changed_nothing(&silent, 0),
            "silence is the only thing a generic body says nothing with"
        );
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
