# mrx — Multi Repo eXtreme

Parallel multi-repo git operations with a compact TUI. A faster replacement for [myrepos](https://myrepos.branchable.com/) (`mr`).

mrx reads your `~/.mrconfig`, runs git commands across all repos in parallel, and shows live progress with per-repo status summaries. Expand any repo to see its full output.

## Prerequisites

- **Rust** 1.87+ (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- **Git** installed and on PATH
- **`~/.mrconfig`** — an INI-style config file listing your repos (same format as [myrepos](https://myrepos.branchable.com/))
- SSH keys or HTTPS credentials configured for your git remotes

## Install

```
cargo install --path . --locked
```

## Usage

```
mrx <command> [options]
```

### Commands

| Command | Description |
|---------|-------------|
| `mrx update` / `pull` | Pull latest changes (clone if repo is missing) |
| `mrx status` | Show working tree status |
| `mrx diff` | Show diffs |
| `mrx push` | Push commits |
| `mrx fetch` | Fetch from remotes |
| `mrx checkout` / `co` | Clone repos (skip if already exists) |
| `mrx run <cmd>` | Run an arbitrary shell command in each repo |
| `mrx register` | Register current repo in `~/.mrconfig` |
| `mrx list` / `ls` | List configured repos (no TUI) |
| `mrx sets` | List named repo sets (no TUI) |
| `mrx ui` | Open the resident app: browse, select, and filter repos (see [The resident app](#the-resident-app)) |
| `mrx <action>` | Run an action defined in the config (see [Custom actions](#custom-actions)) |

### Options

| Flag | Description |
|------|-------------|
| `-j <N>` | Max parallel jobs (default: min(cpus, 8)) |
| `-c <file>` | Config file. Overrides `-s` |
| `-s <name>` | Named repo set (see [Repo sets](#repo-sets)). Also `$MRX_SET` |
| `-d <dir>` | Working directory (default: `[DEFAULT] base`, else config file's parent) |
| `--exit-on-done` | Quit the TUI once every repo has finished, instead of waiting for `q` |
| `--plain` | Never use the TUI, even on a terminal |
| `--result-ttl <d>` | How long `ui` keeps a run's result on its row: `6m` (default), `90s`, `off` |
| `-v` | Verbose output |
| `-n` | No recurse |
| `-f` | Force |

### Examples

```
mrx status              # quick overview of all repos
mrx fetch -j 32         # fetch all 32 repos at once
mrx run "git log -1"    # last commit in each repo
mrx list                # print repos without TUI
mrx -s work update      # update the repos in ~/.config/mrx/work.mrconfig
mrx update --exit-on-done   # unattended: quit when the last repo lands
mrx status | tee log    # not a terminal, so one line per repo instead of a TUI
```

## TUI

The TUI shows a compact one-line-per-repo view with live spinners for in-progress operations:

```
  mrx status                                          28/32 done
 ────────────────────────────────────────────────────────────────
  ✓ first-repo              clean
  ✓ second-repo             clean
  ⠹ third-repo              checking...
  ✓ fourth-repo             2 modified, 1 untracked
  - fifth-repo              not checked out
  ⠙ sixth-repo              checking...
  ✗ seventh-repo            merge conflict!
 ────────────────────────────────────────────────────────────────
  [↑↓/jk] navigate  [enter] expand  [r] re-run  [q] quit
```

Press **Enter** on a repo to expand its full output in a bordered panel. Arrow keys scroll within the panel. **Esc** collapses it. Press **r** once the run has finished to re-run the same command across all repos without leaving the screen. **q** quits and prints a summary.

## The resident app

`mrx ui` is a different shape from the TUI above: it stays open across runs instead of
exiting after one. Branch and working-tree state fill in from a background probe as
soon as the table paints, and any action from `.mrconfig` can run against the
selection without leaving the screen.

```
  mrx · work                                  update 1/2 · 1 failed

     REPO               BRANCH  STATE       RESULT
────────────────────────────────────────────────────────────────────────
 ▸ ● bill-api           main    clean       already up to date
   ● crew-db-schema     main    2 modified  git pull
     loyalty-db-schema  main    clean  ↓3   ·
────────────────────────────────────────────────────────────────────────
  j/k move  space select  / filter  enter output  u update  …  ? help
```

The footer shows as many keys as the terminal is wide enough for, whole ones only,
with `…` standing in for the rest. `? help` is budgeted first and drawn last, so it
survives every width. **`?` opens the full keymap**, listing the keys the footer left
out along with the detail view's.

`j`/`k` (or the arrow keys) move the cursor, `Ctrl-D`/`Ctrl-U` move half a page, and
`g`/`G` jump to the first or last row.
`space` toggles the cursor row's selection and moves on, `a` selects every row the
filter currently shows, `A` selects the whole set regardless of the filter, `c` clears
the selection, and `i` inverts it. **An empty selection means every repo on screen**,
so `u` with nothing selected updates the lot; the header only counts a selection you
actually made, and says nothing when there isn't one.

`/` starts an incremental filter on repo name; keep typing and the table narrows live.
`Esc` drops the filter, `Enter` keeps it. Filtering narrows what's on screen but never
touches the selection, so selecting some repos, then filtering, then selecting again
adds to what was already picked.

`!` drops to `$SHELL` in the cursor row's repo for whatever no action covers, the same
suspend-and-restore the editor gets. It's a key rather than an action because an action
runs unattended across a selection, and a shell is the opposite of both.

`u` runs `update` on the selection; `s`, `f`, `d` run `status`, `fetch`, `diff`. The
built-in `status` reports the branch and its ahead/behind alongside the working tree,
so one run answers both "what have I changed here" and "is there anything to push or
pull". `:` opens the action palette, a filtered list of every runnable action for the set
(built-in and custom alike), each shown with where it's defined and how many repos
actually have it: `deploy  per-repo, 3 of 42`. If the selection includes a repo the
last probe found dirty, running anything asks for confirmation first, showing how
many; pass `-f`/`--force` to skip that. `r` re-probes the selection (or everything,
with nothing selected).

`Enter` opens the detail view for the cursor row: the table collapses to a sidebar
(full-width below about 100 columns). Output arrives **as it is produced**, so a long
update can be read while it runs rather than waited out; a step still going is marked
`…` instead of a tick, and the view follows the tail until you scroll. Each step is its
own labelled section rather than one scrollback. `Ctrl-D`/`Ctrl-U` scroll half a page,
kept per repo; `y` copies the visible step's output and `o` opens the whole transcript
in `$EDITOR`, both falling back to a temp file when there's no clipboard binary on
`PATH`. `Esc` goes back to the full-width list.

```
▌ mrx · work                     │  guest-gateway · update

   REPO                STATE     │  2 steps · exit 0
─────────────────────────────────┼──────────────────────────────────────────────────────────────────
   crew-frontend       clean     │  $ git pull  ✓
 ▸ guest-gateway       clean     │  Updating b31d942..5013b52
   integration-config  clean     │  Fast-forward
─────────────────────────────────┴──────────────────────────────────────────────────────────────────
  tab focus  j/k move  ^d/^u scroll  y copy  esc back  q quit  ? help
```

The split is one frame divided, not two windows: the panes rule off their headers on
the same row, a rule runs between them, and one key line sits under both. `tab` hands
the keys from one pane to the other, marked by the `▌` in the margin and the brighter
title, so `j`/`k` either walk the repo list with the output following or scroll the
output with the cursor staying put. The line under the detail title says how the run
ended and, when the output is longer than the pane, which slice of it you're looking
at.

A run's result stays on its row for six minutes and then goes back to `·`, so a table
left open all afternoon isn't still reporting this morning. `--result-ttl` changes it:
`--result-ttl 30m`, `--result-ttl 90s`, or `--result-ttl off` to keep every result
until the next run replaces it.

`o` opens the cursor row's repo in `$EDITOR` (`vi` if it's unset), from either the
plain list or the detail view. The app suspends properly to do it: raw mode, the
alternate screen, and mouse capture all come off first, so the editor gets a normal
terminal, and all three come back once it exits.

Clicking a row moves the cursor to it; clicking the row already under the cursor
opens its detail view. The wheel scrolls whichever region is under the pointer. `m`
toggles mouse capture off and on, since capture disables the terminal's own text
selection; holding Option/Shift while dragging still selects natively without it.

`tab` opens a picker over every set `mrx sets` would list, plus the active config
labelled `(unnamed)` if it isn't one of them; confirming reloads that config and
restarts the probe from scratch. `Ctrl-R` re-reads the active config without changing
which one is active, keeping the cursor and selection by repo NAME (an edit that adds
a repo above the one you're on doesn't silently redirect the selection onto its
neighbour, and a name the edit removed just drops out). Both are blocked while a run
is live, since re-numbering the repo list out from under an in-flight run's indices
would attribute its results to the wrong row.

`Esc` cancels a live run: everything still queued behind the job limit is skipped,
but a repo already past its slot keeps running to completion (`Command::output`
has no kill), and the status line says exactly that: `cancelled, 2 queued skipped, 1
still finishing`. `q`/`Ctrl-C` quit immediately with nothing running; with a run
live they ask first, since losing sight of an in-flight action isn't something to
do by reflex.

The ahead/behind counts only ever reflect the last fetch, so a repo that's ↓3 behind
shows no ↓ at all until something updates the remote-tracking ref: an absent count is
"nobody has asked", which is not the same claim as ↓0 and so is never drawn as one.
Anything that fetches counts, not just mrx: the probe reads `FETCH_HEAD`'s timestamp,
so running `update` on a repo, or pulling it in another terminal, settles its count on
the next probe. `F` toggles a
freshness poll, `git fetch --quiet` across the set on an interval (5 minutes by
default), suspended rather than queued while a run is live; `Ctrl-A` layers a
narrow, opt-in auto-update on top, fast-forwarding whatever a poll finds behind
on a repo that's clean, not ahead, and tracking an upstream, and simply leaving
everything else alone. Both are off by default and both show in the header the
moment either is on (`poll 5m`, `poll 5m · auto`), since a mode that touches
working trees on a timer has no business being invisible. `Ctrl-A` refuses to
turn on while the poll itself is off, since it has nothing to act on without one.

The set, filter, selection, cursor, and both poll settings are written to
`$XDG_STATE_HOME/mrx/ui.json` (`~/.local/state/mrx/ui.json` by default) as they
change and restored the next time `mrx ui` opens, so reopening puts you back
where you left off; a restored filter shows in the header with its match count
(`4 of 42 repos · filter`) rather than only in the status bar, so it doesn't
look like the config broke. `-s` on the command line always wins over whichever
set was stored, and a name the file remembers that the set no longer has (a
repo, or the set itself) is dropped silently rather than treated as an error.
Deleting the file is a supported way to reset back to defaults.

Needs a real terminal: `mrx ui` with stdout piped, or combined with `--plain`, exits 2
with a pointer at `mrx status` or another non-interactive subcommand instead.

## Config

mrx reads the same `~/.mrconfig` format as `mr`:

```ini
[repos/a-repo]
checkout = git clone 'https://github.com/my-account/a-repo' 'a-repo'

[repos/another-repo]
checkout = git clone 'git@github.com:my-account/another-repo.git' 'cli'
```

Section names are relative paths from the base directory. Both HTTPS and SSH clone URLs are supported.

Whole-line `#` and `;` comments are supported. They are *not* stripped mid-line, so a
command body can contain either character.

### Recognised keys

| Key | Scope | Meaning |
|-----|-------|---------|
| `checkout` | section | Clone command. The URL is parsed out of it for the built-in clone |
| `base` | `[DEFAULT]` | Directory section paths resolve against. Supports `~`. Default: the config file's parent |
| `skip` | section | `true` leaves the section in the file but out of every operation |
| `<action>` | both | Shell body replacing a built-in (`update`, `status`, `diff`, `push`, `fetch`, `checkout`), or defining a new one |
| `post_<action>` | both | Shell body appended after a successful `<action>` |

Any other key is yours. It has no meaning to mrx beyond being exported to the shell
(below), so `branch = master` is just a value your own command reads.

### Custom actions

Any key that isn't a built-in defines a subcommand:

```ini
[DEFAULT]
base = ~/dev
install = yarn install --frozen-lockfile

[dev/api]
checkout = git clone 'git@github.com:me/api.git' 'api'

[dev/site]
checkout = git clone 'git@github.com:me/site.git' 'site'
install = npm ci
```

`mrx install` then runs `npm ci` in `site` and `yarn install --frozen-lockfile`
everywhere else. Resolution is section first, then `[DEFAULT]`, then the built-in.
An action defined nowhere exits 2 rather than silently skipping every repo.

Trailing arguments become positional parameters, so `mrx install --offline` reaches
the body as `$1`.

### Environment

Every action runs through `sh -c` with the repo as its working directory and:

```
MR_REPO      /Users/me/dev/api     # absolute path, also the cwd
MR_REPONAME  api                   # section basename
MR_CONFIG    /Users/me/.config/mrx/work.mrconfig
MR_ACTION    update                # which action is running
MR_<KEY>     ...                   # every config key visible to this repo
```

That last line is what makes per-repo exceptions work without mrx knowing anything
about them:

```ini
[DEFAULT]
base = ~/dev
update = ~/bin/sync-repo.sh

[dev/monorepo]
branch = master
reset  = false
```

`sync-repo.sh` reads `$MR_BRANCH` and `$MR_RESET`. They are passed as environment
rather than interpolated into the command string, so a value can never break out of
the shell word it sits in.

### What counts as a step

mrx builds an action out of up to three steps, and stops at the first that exits
non-zero: the clone (if the repo isn't on disk yet), the `<action>` body, then the
`post_<action>` body. A failure names the step it came from, so a row reads
`post_update: npm error Missing script: "build"` rather than leaving you to guess.

Everything *inside* one body is a single `sh -e -c` invocation, so a body that lists
several commands stops at the first one that fails:

```ini
[DEFAULT]
update = git pull --rebase
         npm ci
         npm run build
```

A failed `git pull --rebase` there ends the body and the repo reports the pull error.
Without `-e` only `npm run build`'s exit code would survive, so a repo that failed to
pull but built fine would report success.

Append `|| true` to a command that is allowed to fail:

```ini
[DEFAULT]
update = git pull --rebase
         ./optional-hook.sh || true
         npm ci
```

## Repo sets

A set is a config file with a name, so unrelated repo lists can share one binary:

```
~/.config/mrx/work.mrconfig     ->  mrx -s work status
~/.config/mrx/oss.mrconfig      ->  mrx -s oss status
```

`~/.mrconfig-<name>` works too. `$MRX_SET` sets one without a flag, and `-c`
overrides both. `mrx sets` lists what's on disk.

Because a set lives in `~/.config/mrx/`, its sections would otherwise resolve
against that directory, which is why `[DEFAULT] base` exists:

```ini
[DEFAULT]
base = ~/dev
```

With no `-s` and no `$MRX_SET`, mrx looks for a `default` set and falls back to
`~/.mrconfig`, so an existing setup keeps working untouched. A set named explicitly
but not found is an error listing the paths tried, not a silent fallback.

## Unattended runs

The TUI waits for `q` by design: that's when you expand the repo that failed and
read its output. Two ways out:

- `--exit-on-done` quits once every repo has landed. Opt-in, so a bare `mrx status`
  is unchanged.
- When stdout isn't a terminal, the TUI is skipped entirely for one line per repo,
  with failures printing their captured output indented beneath. `--plain` forces
  that on a terminal too.

Either way the exit code is 0 only if every repo succeeded.
