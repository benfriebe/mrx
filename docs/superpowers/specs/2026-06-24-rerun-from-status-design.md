# Re-run current command from the results screen

## Problem

After `mrx update` (or any command) finishes, the TUI shows per-repo results. To
run the command again — e.g. to re-pull after fixing a credential or to refresh
state — the user must quit and relaunch `mrx`. There is no way to re-trigger
execution from within the screen.

## Goal

Add a keybinding that re-runs the command the TUI is currently showing, across
all repos, without leaving the screen.

## Behaviour

- Press `r` on the results screen to re-run the current command.
- Because the command is re-planned from scratch, an `update` re-pulls (and
  clones any newly-missing repos); a `status` re-checks; etc.
- Each repo resets to `waiting…` and the run proceeds exactly like a fresh
  launch.

### Guard

`r` only fires when the current run has fully finished (`all_done`). Mid-run it
is a no-op. This prevents two execution batches from racing to write the same
per-repo status slots. `q`, `ctrl-c`, and navigation remain available mid-run as
today.

## Changes by file

1. **`src/tui/mod.rs`** — `run()` gains a `jobs: usize` parameter and makes `rx`
   mutable. A new `r` handler in normal mode only:
   - re-plans operations from `state.repos` + `command` via `operations::plan`
   - swaps in a fresh receiver from `executor::execute_all(&state.repos, ops, jobs)`
   - calls `state.reset_for_rerun()`

   Guarded by `state.all_done`, so no in-flight tasks are orphaned.

2. **`src/tui/state.rs`** — extract branch detection into a
   `compute_branches(&[Repo])` helper (used by `new()`), and add
   `reset_for_rerun(&mut self)`:
   - statuses → all `Pending`
   - recompute branches (an update may have created new clones / switched branches)
   - clear `expanded` / `scroll_offset`
   - `all_done = false`
   - clamp `selected` to the repo count

3. **`src/main.rs`** — pass `jobs` into `tui::run`.

4. **`src/tui/render.rs`** — normal-mode footer becomes
   `[↑↓/jk] navigate  [enter] expand  [r] re-run  [q] quit`.

## Testing

- Unit test for `reset_for_rerun` in `state.rs`: statuses cleared to `Pending`,
  `all_done` reset, `expanded`/`scroll_offset` cleared, `selected` clamped.
- Execution/keybinding wiring is verified by building and running the binary;
  it is not unit-testable without a TUI harness.

## Out of scope

- An "always update" key or a dual-key (`u` update / `r` refresh) variant.
- Re-running only the failed repos.
- Aborting an in-flight run.
