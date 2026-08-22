# mrx

mrx runs git operations across every repo in a set, in parallel, with a compact TUI. A faster replacement for [myrepos](https://myrepos.branchable.com/) (`mr`).

It reads your `~/.mrconfig`, fans one command out across every repo at once, and reports live per-repo progress instead of a wall of scrollback. Two views cover the two ways you use it: a one-shot run view that reports a single command and then exits, and `mrx ui`, a table that stays open across runs.

> Rust, built on [ratatui](https://ratatui.rs/) and crossterm. The config format is `mr`'s, so an existing `~/.mrconfig` works untouched.

## Highlights

- **Parallel by default.** Every repo in the set runs at once, bounded by `-j` (default: `min(cpus, 8)`).
- **Two views.** A one-shot run view for `mrx status` and friends, and [ui mode](#ui-mode) for a table that stays open across runs.
- **Drop-in `.mrconfig`.** The same INI format and section-path convention as `mr`, so an existing setup needs no migration.
- **Custom actions.** Any key in the config becomes a subcommand, resolved per repo and falling back to `[DEFAULT]`.
- **Named repo sets.** Unrelated repo lists share one binary through `-s work`, `-s oss`, or `$MRX_SET`.
- **Live repo state.** ui mode fills in each repo's branch, working tree and ahead/behind counts from a background probe as the table paints.
- **Honest non-interactive mode.** Off a terminal mrx prints one line per repo, and exits 0 only if every repo succeeded.

## Prerequisites

- **Rust** and cargo, to build from source (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`). The crate declares no minimum version, so current stable is the safe answer.
- **Git** installed and on `PATH`.
- **`~/.mrconfig`**, an INI-style file listing your repos (same format as [myrepos](https://myrepos.branchable.com/)). See [Config](#config).
- SSH keys or HTTPS credentials configured for your git remotes.

## Install

```bash
git clone git@github.com:paulchiu/mrx.git
cd mrx
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
| `mrx ui` | Open [ui mode](#ui-mode): browse, select, and filter repos, and run actions against them |
| `mrx <action>` | Run an action defined in the config (see [Custom actions](#custom-actions)) |

### Options

| Flag | Description |
|------|-------------|
| `-j <N>` | Max parallel jobs (default: `[DEFAULT] jobs`, else min(cpus, 8)) |
| `-c <file>` | Config file. Overrides `-s` |
| `-s <name>` | Named repo set (see [Repo sets](#repo-sets)) |
| `-d <dir>` | Working directory (default: `[DEFAULT] base`, else config file's parent) |
| `-f` | ui mode only: skip the confirmation before running on a dirty or unprobed selection |
| `--exit-on-done` | Quit the TUI once every repo has finished, instead of waiting for `q`. Ignored by `ui`, which has no single run to wait on |
| `--plain` | Never use the TUI, even on a terminal |
| `--result-ttl <d>` | How long ui mode keeps a run's result on its row: `6m` (default), `90s`, `off` |

## Examples

```
mrx status              # quick overview of all repos
mrx fetch -j 32         # fetch all 32 repos at once
mrx run "git log -1"    # last commit in each repo
mrx list                # print repos without TUI
mrx -s work update      # update the repos in ~/.config/mrx/work.mrconfig
mrx update --exit-on-done   # unattended: quit when the last repo lands
mrx status | tee log    # not a terminal, so one line per repo instead of a TUI
```

## The run view

One command, one line per repo, with live spinners for whatever is still going. It exits when you do.

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

- `↑`/`↓` or `j`/`k` move between repos; `g`/`G` and `Home`/`End` jump to the first or last.
- `Enter` expands the cursor row's full output in a bordered panel.
- With a panel open, `↑`/`↓` or `j`/`k` scroll it and `Esc` or `Enter` collapses it again.
- `r` re-runs the same command across every repo, once the previous run has finished.
- `q` or `Ctrl-C` quits and prints a summary.

## ui mode

`mrx ui` is a different shape from the run view: it stays open across runs instead of exiting after one, so a set stays on screen and actions run against a selection you keep. Branch and working-tree state fill in from a background probe as soon as the table paints, and any action from `.mrconfig` can run without leaving the screen.

```
  mrx · work                                  update 1/2 · 1 failed

     REPO               BRANCH  STATE       SYNC     RESULT
────────────────────────────────────────────────────────────────────────
 ▸ ● bill-api           main    clean                already up to date
   ⠹ crew-db-schema     main    2 modified  ↑2       git pull
     loyalty-db-schema  main    clean           ↓3   ·
────────────────────────────────────────────────────────────────────────
  j/k move  space select  / filter  enter output  u update  …  ? help
```

The footer shows as many keys as the terminal is wide enough for, whole ones only, with `…` standing in for the rest. `? help` is budgeted first and drawn last, so it survives every width, and `?` opens the full keymap.

- **Watch.** A row waiting on anything, a background probe or a running action alike, spins in the cell its selection dot normally occupies, the same cue in the same place the run view puts it.
- **Move.** `j`/`k` or the arrow keys walk the table, `Ctrl-D`/`Ctrl-U` move half a page, `g`/`G` jump to the first or last row.
- **Select.** `space` toggles the cursor row and moves on, `a` takes every row the filter shows, `A` the whole set regardless, `c` clears, `i` inverts. **An empty selection means every repo on screen**, so `u` with nothing selected updates the lot; the header only ever counts a selection you actually made.
- **Filter.** `/` starts an incremental filter on repo name and the table narrows as you type. `Esc` drops it, `Enter` keeps it, `/` again starts over from the full list. Filtering never touches the selection, so selecting, filtering, then selecting again adds to what was already picked.
- **Run.** `u` runs `update` on the selection; `s`, `f` and `d` run `status`, `fetch` and `diff`. The built-in `status` reports the branch and its ahead/behind alongside the working tree, so one run answers both "what have I changed here" and "is there anything to push or pull".
- **Any action.** `:` opens the action palette, a filtered list of every runnable action for the set, each shown with where it is defined and how many repos actually have it: `deploy  per-repo, 3 of 42`. It also carries the selection commands, each showing how many repos it would leave selected.
- **Confirm.** Running on a selection the last probe found dirty, or has not probed yet, asks first and says how many. `-f`/`--force` skips the prompt.
- **Read output.** `Enter` opens the detail view for the cursor row, streaming each step's output as it is produced rather than at the end, so a long update can be read while it runs. `y` copies the visible step, `o` opens the whole transcript in `$EDITOR`.
- **Sort.** `S` opens a one-key menu of the columns: `r` REPO, `b` BRANCH, `s` STATE, `u` SYNC, `l` RESULT. The sorted column's header carries `↑` or `↓`, and choosing the same column again flips it, so `S s S s` goes from dirtiest-first to cleanest-first. STATE, SYNC and RESULT open worst-first, which is the reason to order by them: RESULT leads with anything that failed, then the repos a run actually changed, so a set where everything reads `up to date` still brings the handful that moved to the top; SYNC leads with the repos furthest behind. Unprobed rows stay at the end either way. The order is remembered across restarts.
- **Run anything.** `r` opens a prompt for a command to run against the selection, `Ctrl-D` to run it. The body goes to `sh` whole, so it can be several lines.
- **Re-probe.** `R` re-reads the selection's state (or everything, with nothing selected).
- **Escape hatch.** `!` drops to `$SHELL` (`sh` if unset) in the cursor row's repo, for whatever no action covers. It is a key rather than an action because an action runs unattended across a selection, and a shell is the opposite of both.

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

The split is one frame divided, not two windows: the panes rule off their headers on the same row, a rule runs between them, and one key line sits under both. `tab` hands the keys from one pane to the other, marked by the `▌` in the margin and the brighter title, so `j`/`k` either walk the repo list with the output following or scroll the output with the cursor staying put. Clicking a pane focuses it as well.

SYNC carries a row's distance from its upstream, `↑` unpushed and `↓` unpulled, each arrow in a fixed field so the counts line up down the table however many digits the row beside it needs. A set with nothing to report drops the column entirely. The counts only ever reflect the last fetch, so a repo that is ↓3 behind shows no ↓ at all until something updates the remote-tracking ref. An absent count is "nobody has asked", which is not the same claim as ↓0 and so is never drawn as one. Anything that fetches counts, not just mrx: the probe reads `FETCH_HEAD`'s timestamp, so pulling a repo in another terminal settles its count on the next probe.

`Esc` cancels a live run, but only as far as it honestly can. Everything still queued behind the job limit is skipped; a repo already past its slot keeps running to completion, because `Command::output` has no kill. The status line says exactly that: `cancelled, 2 queued skipped, 1 still finishing`.

`F` turns a freshness poll on and off, and `Ctrl-A` layers an auto-update on top of it, running the set's own `update` over whatever a cycle finds behind and is safe to touch unattended. `Ctrl-A` turns the poll on with it, auto-update being the poll plus what it does with the result; `F` is what turns the poll back off, and takes auto-update with it. The header always says which of the three states it is in (`poll off`, `poll 6m`, `poll 6m · auto`), under a live run included, since a mode that touches working trees on a timer has no business being invisible. Once a cycle has run, the header also says when: `checked 40s ago`, rolling up to minutes and then hours as it ages.

A set can ask for the poll itself with `[DEFAULT] auto_fetch`, so opening it never needs the keystroke. A set opened with the poll on and sync answers older than one interval fetches once as soon as the opening probe lands, rather than leaving every `↓` on the numbers a previous session left behind until the first interval elapses. `F` still overrides it, off included, and a set switch re-reads whatever the set being opened asks for.

See [docs/ui-keys.md](docs/ui-keys.md) for every binding, by input mode. See [docs/ui-mode.md](docs/ui-mode.md) for colour handling, result lifetime, set switching, cancellation, freshness polling, and the persisted session.

## Config

mrx reads the same `~/.mrconfig` format as `mr`:

```ini
[repos/a-repo]
checkout = git clone 'https://github.com/my-account/a-repo' 'a-repo'

[repos/another-repo]
checkout = git clone 'git@github.com:my-account/another-repo.git' 'cli'
```

- Section names are relative paths from the base directory, and keep their case. Keys are lowercased, so `Branch` and `branch` are one key. Both HTTPS and SSH clone URLs are parsed for the built-in clone.
- Whole-line `#` and `;` comments are supported. They are *not* stripped mid-line, so a command body can contain either character.
- Repos are listed in name order, not config order.

### Recognised keys

| Key | Scope | Meaning |
|-----|-------|---------|
| `checkout` | section | Clone command. The URL is parsed out of it for the built-in clone |
| `base` | `[DEFAULT]` | Directory section paths resolve against. Supports `~`. Default: the config file's parent |
| `jobs` | `[DEFAULT]` | Max parallel jobs for this set, overridden by `-j`. Must be at least 1; anything else is a config error |
| `auto_fetch` | `[DEFAULT]` | Fetch this set on a timer in ui mode: `on` (every 6m), an interval (`90s`, `10m`, `1h`), or `off`. Anything else is a config error |
| `skip` | section | `true`, `yes` or `1` leaves the section in the file but out of every operation. `false`, `no` and `0` are the default; anything else also reads as the default |
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

- `mrx install` runs `npm ci` in `site` and `yarn install --frozen-lockfile` everywhere else.
- Resolution is section first, then `[DEFAULT]`, then the built-in.
- An action defined nowhere exits 2 rather than silently skipping every repo.
- Trailing arguments become positional parameters, so `mrx install --offline` reaches the body as `$1`.

### Environment

Every action runs through `sh -e -c` with the repo as its working directory and:

```
MR_REPO      /Users/me/dev/api     # absolute path, also the cwd
MR_REPONAME  api                   # section basename
MR_CONFIG    /Users/me/.config/mrx/work.mrconfig
MR_ACTION    update                # which action is running
MR_<KEY>     ...                   # every config key visible to this repo,
                                   # except the reserved `base` and `skip`
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

mrx builds an action out of up to three steps and stops at the first that exits non-zero:

- The clone, if the repo isn't on disk yet.
- The `<action>` body.
- The `post_<action>` body.

A failure names the step it came from, so a row reads `post_update: npm error Missing script: "build"` rather than leaving you to guess.

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

- A named set is looked for at `$XDG_CONFIG_HOME/mrx/<name>.mrconfig` (falling back to `~/.config/mrx/`) first, then `~/.mrconfig-<name>`. A name in both is used and listed once, at the first of those.
- `$MRX_SET` names a set without a flag, `-s` overrides it, and `-c` overrides both.
- `mrx sets` lists what is on disk.
- With neither `-s` nor `$MRX_SET`, mrx looks for a `default` set and falls back to `~/.mrconfig`, so an existing setup keeps working untouched.
- A set named explicitly but not found is an error listing the paths tried, not a silent fallback.

Because a set lives in `~/.config/mrx/`, its sections would otherwise resolve
against that directory, which is why `[DEFAULT] base` exists:

```ini
[DEFAULT]
base = ~/dev
```

## Unattended runs

The TUI waits for `q` by design: that's when you expand the repo that failed and read its output. Two ways out:

- `--exit-on-done` quits once every repo has landed. Opt-in, so a bare `mrx status` is unchanged.
- When stdout isn't a terminal, the TUI is skipped entirely for one line per repo, with failures printing their captured output indented beneath. `--plain` forces that on a terminal too.

Either way the exit code is 0 only if every repo succeeded.

## Docs

- [docs/ui-keys.md](docs/ui-keys.md): every ui mode binding, by input mode, plus the mouse and the behaviour notes the `?` overlay carries.
- [docs/ui-mode.md](docs/ui-mode.md): how ui mode behaves beyond its keymap, in the order you hit it.
