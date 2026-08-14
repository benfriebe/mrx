//! Real-pty coverage for the phase 6 "wrecked terminal" tests (section 10,
//! phase 6 row): the terminal has to come back exactly as it was after a
//! plain quit, after suspending for `$EDITOR`, and after a panic mid-run.
//!
//! `Command::output()`-style tests (see `ui_subcommand.rs`) never give the
//! child a real terminal, so there is nothing here for `enable_raw_mode`
//! to even act on. The only way to give `mrx ui` a real pty from a harness
//! that has none of its own is the BSD `script` utility (`script -q
//! /dev/null <command>`), which allocates one regardless of whether its own
//! stdin is a terminal. Its argument syntax is BSD-specific (Linux's
//! `script` takes different flags), so this file is macOS-only.

#![cfg(target_os = "macos")]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `stty -a`'s `pendin` (BSD: "pending special character") lflag flips on
/// its own the first time a process on the pty enters and leaves raw mode,
/// independent of anything `mrx` restores; comparing it would fail a
/// before/after diff for a reason that has nothing to do with whether raw
/// mode itself came back. Every other field is a real assertion.
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
/// `stty -a` before and after `command_line`, so the comparison happens
/// inside the same pty session the command itself ran in (a `stty -a` taken
/// from outside, in this harness's own non-tty process, would have nothing
/// to compare against).
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
    output: Vec<u8>,
    /// `script`'s own exit code, not the driven command's; the command's
    /// real exit code is read back out of `MRX_PTY_EXIT:<n>` in `output`.
    timed_out: bool,
}

/// Run `driver_script` under a real pty via `script -q /dev/null`, sending
/// each of `keys` (delayed relative to when the previous one was sent) into
/// it, then waiting up to `timeout` for the session to end on its own
/// before killing it, so a regression that breaks `q` fails fast rather
/// than hanging the suite.
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

/// The index of the last occurrence of `needle`, for asserting an enable
/// sequence is followed by a later disable rather than just co-occurring.
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

    // `true` stands in for a real editor: it exits immediately, so `o` can
    // be exercised without an interactive process to drive.
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

    // Entered once at startup and again on resume from the editor; left
    // once to suspend and again on the final quit.
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

/// Finding B2: an early `Err` return (no panic) between entering the
/// terminal's raw/alt-screen/mouse-capture state and the app's own teardown
/// used to skip cleanup entirely, since only the panic hook restored the
/// terminal. `TerminalGuard`'s `Drop` must do it on its own, independent of
/// that hook. Distinct from `a_panic_restores_the_terminal_over_a_real_pty`,
/// which covers the panic path.
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
