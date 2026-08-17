//! Real-pty coverage for the "wrecked terminal" cases: the terminal has to
//! come back exactly as it was after a plain quit, after suspending for
//! `$EDITOR`, and after a panic mid-run.
//!
//! `Command::output()`-style tests (see `ui_subcommand.rs`) give the child no
//! terminal, so `enable_raw_mode` has nothing to act on. BSD `script -q
//! /dev/null <command>` allocates a pty even when its own stdin isn't one;
//! its argument syntax is BSD-specific, hence the macOS-only gate.

#![cfg(target_os = "macos")]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `stty -a`'s `pendin` lflag flips on its own the first time a process on
/// the pty enters and leaves raw mode, so comparing it would fail a
/// before/after diff for a reason unrelated to what `mrx` restored. Every
/// other field is a real assertion.
fn normalize_stty(raw: &str) -> String {
    raw.split_whitespace()
        .filter(|tok| *tok != "pendin" && *tok != "-pendin")
        .collect::<Vec<_>>()
        .join(" ")
}

fn require_script() {
    let ok = Command::new("script")
        .args(["-q", "/dev/null", "true"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(
        ok,
        "macOS ships `script` as part of the base system; its absence here \
         is itself worth failing loudly on rather than silently skipping"
    );
}

fn write_minimal_config(dir: &Path) -> PathBuf {
    let config = dir.join("test.mrconfig");
    std::fs::write(
        &config,
        format!(
            "[DEFAULT]\nbase = {}\n\n[repos/x]\ncheckout = true\n",
            dir.display()
        ),
    )
    .expect("write throwaway config");
    config
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A driver script run under `script -q /dev/null bash <path>`: captures
/// `stty -a` either side of `command_line`, inside the same pty session the
/// command runs in. This harness's own process has no tty to compare against.
fn write_driver_script(dir: &Path, command_line: &str, before: &Path, after: &Path) -> PathBuf {
    let script_path = dir.join("driver.sh");
    let content = format!(
        "#!/bin/bash\nstty -a > {before} 2>&1\n{command_line}\nstatus=$?\nstty -a > {after} 2>&1\necho MRX_PTY_EXIT:$status\n",
        before = sh_quote(&before.display().to_string()),
        after = sh_quote(&after.display().to_string()),
    );
    std::fs::write(&script_path, content).expect("write driver script");
    script_path
}

struct PtySession {
    /// Raw pty transcript. The driven command's exit code arrives in here as
    /// `MRX_PTY_EXIT:<n>`; `script`'s own status is not it.
    output: Vec<u8>,
    timed_out: bool,
}

/// Run `driver_script` under a real pty via `script -q /dev/null`, sending
/// each of `keys` after a delay relative to the previous one, then waiting up
/// to `timeout` before killing the session, so a regression that breaks `q`
/// fails fast rather than hanging the suite.
fn run_in_pty(
    driver_script: &Path,
    envs: &[(&str, &str)],
    keys: &[(Duration, &[u8])],
    timeout: Duration,
) -> PtySession {
    let mut cmd = Command::new("script");
    cmd.args(["-q", "/dev/null", "bash"]).arg(driver_script);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn `script`");

    let mut stdin = child.stdin.take().expect("piped stdin");
    let key_writer = std::thread::spawn({
        let keys: Vec<(Duration, Vec<u8>)> = keys.iter().map(|(d, b)| (*d, b.to_vec())).collect();
        move || {
            for (delay, bytes) in keys {
                std::thread::sleep(delay);
                let _ = stdin.write_all(&bytes);
                let _ = stdin.flush();
            }
        }
    });

    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let _ = key_writer.join();
    let output = reader.join().unwrap_or_default();
    PtySession { output, timed_out }
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// Last occurrence of `needle`, for asserting an enable sequence is followed
/// by a later disable rather than merely co-occurring.
fn last_index(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .enumerate()
        .filter(|(_, w)| *w == needle)
        .map(|(i, _)| i)
        .next_back()
}

const MOUSE_ENABLE: &[u8] = b"\x1b[?1000h";
const MOUSE_DISABLE: &[u8] = b"\x1b[?1000l";
const ALT_SCREEN_ENTER: &[u8] = b"\x1b[?1049h";
const ALT_SCREEN_LEAVE: &[u8] = b"\x1b[?1049l";

#[test]
fn quitting_restores_the_terminal_over_a_real_pty() {
    require_script();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_minimal_config(dir.path());
    let bin = env!("CARGO_BIN_EXE_mrx");
    let command_line = format!(
        "{} ui -c {}",
        sh_quote(bin),
        sh_quote(&config.display().to_string())
    );
    let before = dir.path().join("before.txt");
    let after = dir.path().join("after.txt");
    let driver = write_driver_script(dir.path(), &command_line, &before, &after);

    let session = run_in_pty(
        &driver,
        &[],
        &[(Duration::from_millis(500), b"q")],
        Duration::from_secs(10),
    );

    assert!(
        !session.timed_out,
        "the session should quit on its own after `q`"
    );
    let out = String::from_utf8_lossy(&session.output);
    assert!(
        out.contains("MRX_PTY_EXIT:0"),
        "mrx should exit 0 on a plain quit, got: {out}"
    );

    let before_txt = std::fs::read_to_string(&before).unwrap_or_default();
    let after_txt = std::fs::read_to_string(&after).unwrap_or_default();
    assert_eq!(
        normalize_stty(&before_txt),
        normalize_stty(&after_txt),
        "stty -a must match before mrx ran and after it quit"
    );

    let last_enable = last_index(&session.output, MOUSE_ENABLE);
    let last_disable = last_index(&session.output, MOUSE_DISABLE);
    assert!(
        last_enable.is_some() && last_disable.is_some() && last_enable < last_disable,
        "mouse capture must end disabled: enable at {last_enable:?}, disable at {last_disable:?}"
    );
}

#[test]
fn dollar_editor_suspends_and_restores_the_terminal() {
    require_script();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_minimal_config(dir.path());
    let bin = env!("CARGO_BIN_EXE_mrx");
    let command_line = format!(
        "{} ui -c {}",
        sh_quote(bin),
        sh_quote(&config.display().to_string())
    );
    let before = dir.path().join("before.txt");
    let after = dir.path().join("after.txt");
    let driver = write_driver_script(dir.path(), &command_line, &before, &after);

    // `true` stands in for the editor: it exits at once, so `o` needs no
    // interactive process to drive.
    let session = run_in_pty(
        &driver,
        &[("EDITOR", "true")],
        &[
            (Duration::from_millis(500), b"o"),
            (Duration::from_millis(500), b"q"),
        ],
        Duration::from_secs(10),
    );

    assert!(
        !session.timed_out,
        "the session should quit on its own after `o` then `q`"
    );
    let out = String::from_utf8_lossy(&session.output);
    assert!(
        out.contains("MRX_PTY_EXIT:0"),
        "mrx should exit 0 after suspending for the editor and quitting, got: {out}"
    );

    let before_txt = std::fs::read_to_string(&before).unwrap_or_default();
    let after_txt = std::fs::read_to_string(&after).unwrap_or_default();
    assert_eq!(
        normalize_stty(&before_txt),
        normalize_stty(&after_txt),
        "stty -a must match before mrx ran and after it quit, even after a suspend in between"
    );

    // Entered at startup and again on resume; left to suspend and again on quit.
    assert!(
        count_occurrences(&session.output, ALT_SCREEN_ENTER) >= 2,
        "the alternate screen must be re-entered after the editor closes, got: {out}"
    );
    assert!(
        count_occurrences(&session.output, ALT_SCREEN_LEAVE) >= 2,
        "the alternate screen must be left both to suspend and to quit, got: {out}"
    );
    assert!(
        count_occurrences(&session.output, MOUSE_DISABLE) >= 2,
        "mouse capture must be dropped both to suspend and to quit, got: {out}"
    );
    assert!(
        count_occurrences(&session.output, MOUSE_ENABLE) >= 2,
        "mouse capture must be restored on resume from the editor, got: {out}"
    );
}

/// `!` hands the terminal over the way `o` does, and getting it back matters
/// most here: botched, the user is left typing blind into a raw-mode terminal
/// that no longer echoes.
#[test]
fn bang_drops_to_a_shell_and_takes_the_terminal_back() {
    require_script();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_minimal_config(dir.path());
    let bin = env!("CARGO_BIN_EXE_mrx");
    let command_line = format!(
        "{} ui -c {}",
        sh_quote(bin),
        sh_quote(&config.display().to_string())
    );
    let before = dir.path().join("before.txt");
    let after = dir.path().join("after.txt");
    let driver = write_driver_script(dir.path(), &command_line, &before, &after);

    // `true` stands in for the shell, exiting at once so `!` needs nothing to drive.
    let session = run_in_pty(
        &driver,
        &[("SHELL", "true")],
        &[
            (Duration::from_millis(500), b"!"),
            (Duration::from_millis(500), b"q"),
        ],
        Duration::from_secs(10),
    );

    assert!(
        !session.timed_out,
        "the session should quit on its own after `!` then `q`"
    );
    let out = String::from_utf8_lossy(&session.output);
    assert!(
        out.contains("MRX_PTY_EXIT:0"),
        "mrx should exit 0 after suspending for the shell and quitting, got: {out}"
    );

    let before_txt = std::fs::read_to_string(&before).unwrap_or_default();
    let after_txt = std::fs::read_to_string(&after).unwrap_or_default();
    assert_eq!(
        normalize_stty(&before_txt),
        normalize_stty(&after_txt),
        "stty -a must match before mrx ran and after it quit, even after a shell in between"
    );
    assert!(
        count_occurrences(&session.output, ALT_SCREEN_ENTER) >= 2,
        "the alternate screen must be re-entered after the shell exits, got: {out}"
    );
}

/// `TerminalGuard`'s `Drop` must restore the terminal on an early `Err`
/// return with no panic hook to fall back on. The fixture builds the guard
/// directly and never calls `ui::app::run`, so nothing here covers
/// `InputThreadGuard`;
/// `an_early_return_from_run_restores_the_terminal_and_stops_the_input_thread`
/// does that.
#[test]
fn an_early_return_restores_the_terminal_over_a_real_pty_without_panicking() {
    require_script();
    let build = Command::new("cargo")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "build",
            "--example",
            "early_return_while_running",
            "--quiet",
        ])
        .status()
        .expect("failed to build the early-return fixture");
    assert!(build.success(), "the early-return fixture must build");

    let example_bin = Path::new(env!("CARGO_BIN_EXE_mrx"))
        .parent()
        .expect("target/debug")
        .join("examples")
        .join("early_return_while_running");
    assert!(
        example_bin.is_file(),
        "expected the early-return fixture at {}",
        example_bin.display()
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let before = dir.path().join("before.txt");
    let after = dir.path().join("after.txt");
    let command_line = sh_quote(&example_bin.display().to_string());
    let driver = write_driver_script(dir.path(), &command_line, &before, &after);

    // The fixture returns `Err` on its own; there is nothing to send.
    let session = run_in_pty(&driver, &[], &[], Duration::from_secs(10));

    assert!(
        !session.timed_out,
        "a process that returns Err from main must still exit promptly"
    );
    let out = String::from_utf8_lossy(&session.output);
    assert!(
        out.contains("MRX_PTY_EXIT:1"),
        "an Err from main exits 1, got: {out}"
    );
    assert!(
        !out.contains("panicked at"),
        "this fixture must not panic, or it isn't testing the guard's Drop on its own: {out}"
    );

    let before_txt = std::fs::read_to_string(&before).unwrap_or_default();
    let after_txt = std::fs::read_to_string(&after).unwrap_or_default();
    assert_eq!(
        normalize_stty(&before_txt),
        normalize_stty(&after_txt),
        "stty -a must match before the fixture ran and after it returned Err"
    );

    let last_enable = last_index(&session.output, MOUSE_ENABLE);
    let last_disable = last_index(&session.output, MOUSE_DISABLE);
    assert!(
        last_enable.is_some() && last_disable.is_some() && last_enable < last_disable,
        "mouse capture must end disabled: enable at {last_enable:?}, disable at {last_disable:?}"
    );

    assert!(
        count_occurrences(&session.output, ALT_SCREEN_ENTER) >= 1
            && count_occurrences(&session.output, ALT_SCREEN_LEAVE) >= 1,
        "the alternate screen must be left again on the way out, got: {out}"
    );
}

/// Drives the real `ui::app::run`: the fixture breaks stdout once ui mode is
/// up, so the next ticker-driven `terminal.draw` fails and `run` takes the
/// early `?` return any draw failure in its select loop would, input reader
/// thread still polling the pty. Confirms the terminal comes back and the
/// process exits rather than hanging on `InputThreadGuard`'s join.
///
/// It can't prove the ordering between the two guards: the process exits too
/// quickly after `run` returns for either to be observed separately.
#[test]
fn an_early_return_from_run_restores_the_terminal_and_stops_the_input_thread() {
    require_script();
    let build = Command::new("cargo")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["build", "--example", "app_run_draw_failure", "--quiet"])
        .status()
        .expect("failed to build the app_run_draw_failure fixture");
    assert!(
        build.success(),
        "the app_run_draw_failure fixture must build"
    );

    let example_bin = Path::new(env!("CARGO_BIN_EXE_mrx"))
        .parent()
        .expect("target/debug")
        .join("examples")
        .join("app_run_draw_failure");
    assert!(
        example_bin.is_file(),
        "expected the fixture at {}",
        example_bin.display()
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let before = dir.path().join("before.txt");
    let after = dir.path().join("after.txt");
    let command_line = sh_quote(&example_bin.display().to_string());
    let driver = write_driver_script(dir.path(), &command_line, &before, &after);

    let session = run_in_pty(&driver, &[], &[], Duration::from_secs(10));

    assert!(
        !session.timed_out,
        "run() must return and the process must exit promptly once its forced draw failure \
         hits, rather than hang joining the input thread"
    );
    let out = String::from_utf8_lossy(&session.output);
    assert!(
        out.contains("run() returned Err as expected"),
        "the fixture must actually hit the draw failure it means to exercise, got: {out}"
    );

    let before_txt = std::fs::read_to_string(&before).unwrap_or_default();
    let after_txt = std::fs::read_to_string(&after).unwrap_or_default();
    assert_eq!(
        normalize_stty(&before_txt),
        normalize_stty(&after_txt),
        "raw mode must not survive an early Err return from inside run's real select loop, got: {out}"
    );
}

/// `setup_terminal` must roll back raw mode itself when a later step of its
/// own setup fails, since no guard exists yet to undo it.
#[test]
fn setup_terminal_rolls_back_raw_mode_when_a_later_step_fails() {
    require_script();
    let build = Command::new("cargo")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "build",
            "--example",
            "setup_terminal_partial_failure",
            "--quiet",
        ])
        .status()
        .expect("failed to build the setup_terminal fixture");
    assert!(build.success(), "the setup_terminal fixture must build");

    let example_bin = Path::new(env!("CARGO_BIN_EXE_mrx"))
        .parent()
        .expect("target/debug")
        .join("examples")
        .join("setup_terminal_partial_failure");
    assert!(
        example_bin.is_file(),
        "expected the fixture at {}",
        example_bin.display()
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let before = dir.path().join("before.txt");
    let after = dir.path().join("after.txt");
    let command_line = sh_quote(&example_bin.display().to_string());
    let driver = write_driver_script(dir.path(), &command_line, &before, &after);

    let session = run_in_pty(&driver, &[], &[], Duration::from_secs(10));

    assert!(
        !session.timed_out,
        "the fixture must return promptly once its own setup fails"
    );
    let out = String::from_utf8_lossy(&session.output);
    assert!(
        out.contains("setup_terminal failed as expected"),
        "the fixture must actually hit the failure it means to exercise, got: {out}"
    );

    let before_txt = std::fs::read_to_string(&before).unwrap_or_default();
    let after_txt = std::fs::read_to_string(&after).unwrap_or_default();
    assert_eq!(
        normalize_stty(&before_txt),
        normalize_stty(&after_txt),
        "raw mode must not survive a setup_terminal that itself returned Err, got: {out}"
    );
}

/// `run` (the one-shot progress view) must restore the terminal on an early
/// `Err` return from inside its draw loop, not just on the normal quit path.
#[test]
fn the_one_shot_view_restores_raw_mode_on_an_early_draw_failure() {
    require_script();
    let build = Command::new("cargo")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["build", "--example", "run_view_draw_failure", "--quiet"])
        .status()
        .expect("failed to build the run-view fixture");
    assert!(build.success(), "the run-view fixture must build");

    let example_bin = Path::new(env!("CARGO_BIN_EXE_mrx"))
        .parent()
        .expect("target/debug")
        .join("examples")
        .join("run_view_draw_failure");
    assert!(
        example_bin.is_file(),
        "expected the fixture at {}",
        example_bin.display()
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let before = dir.path().join("before.txt");
    let after = dir.path().join("after.txt");
    let command_line = sh_quote(&example_bin.display().to_string());
    let driver = write_driver_script(dir.path(), &command_line, &before, &after);

    let session = run_in_pty(&driver, &[], &[], Duration::from_secs(10));

    assert!(
        !session.timed_out,
        "the fixture must return promptly once its forced draw failure hits"
    );
    let out = String::from_utf8_lossy(&session.output);
    assert!(
        out.contains("run() returned Err as expected"),
        "the fixture must actually hit the draw failure it means to exercise, got: {out}"
    );

    let before_txt = std::fs::read_to_string(&before).unwrap_or_default();
    let after_txt = std::fs::read_to_string(&after).unwrap_or_default();
    assert_eq!(
        normalize_stty(&before_txt),
        normalize_stty(&after_txt),
        "raw mode must not survive an early Err return from inside run's draw loop, got: {out}"
    );
}

#[test]
fn a_panic_restores_the_terminal_over_a_real_pty() {
    require_script();
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let build = Command::new("cargo")
        .current_dir(manifest_dir)
        .args(["build", "--example", "panic_while_running", "--quiet"])
        .status()
        .expect("failed to build the panic fixture");
    assert!(build.success(), "the panic fixture must build");

    let example_bin = Path::new(env!("CARGO_BIN_EXE_mrx"))
        .parent()
        .expect("target/debug")
        .join("examples")
        .join("panic_while_running");
    assert!(
        example_bin.is_file(),
        "expected the panic fixture at {}",
        example_bin.display()
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let before = dir.path().join("before.txt");
    let after = dir.path().join("after.txt");
    let command_line = sh_quote(&example_bin.display().to_string());
    let driver = write_driver_script(dir.path(), &command_line, &before, &after);

    // The fixture panics on its own; there is nothing to send.
    let session = run_in_pty(&driver, &[], &[], Duration::from_secs(10));

    assert!(
        !session.timed_out,
        "a panicking process must still exit promptly"
    );
    let out = String::from_utf8_lossy(&session.output);
    assert!(
        out.contains("MRX_PTY_EXIT:101"),
        "a Rust panic exits 101, got: {out}"
    );
    assert!(
        out.contains("deliberate panic to exercise the installed teardown hook"),
        "the default panic message should still print after the hook restores the terminal, got: {out}"
    );

    let before_txt = std::fs::read_to_string(&before).unwrap_or_default();
    let after_txt = std::fs::read_to_string(&after).unwrap_or_default();
    assert_eq!(
        normalize_stty(&before_txt),
        normalize_stty(&after_txt),
        "stty -a must match before the fixture ran and after it panicked"
    );

    let last_enable = last_index(&session.output, MOUSE_ENABLE);
    let last_disable = last_index(&session.output, MOUSE_DISABLE);
    assert!(
        last_enable.is_some() && last_disable.is_some() && last_enable < last_disable,
        "the panic hook must disable mouse capture after the fixture enabled it: \
         enable at {last_enable:?}, disable at {last_disable:?}"
    );

    let alt_leave = last_index(&session.output, ALT_SCREEN_LEAVE);
    let panic_message_at = last_index(&session.output, b"deliberate panic");
    assert!(
        alt_leave.is_some() && panic_message_at.is_some() && alt_leave < panic_message_at,
        "the terminal must be restored before the panic message prints, not after"
    );
}
