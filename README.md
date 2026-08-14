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
  [↑↓/jk] navigate  [enter] expand  [q] quit
```

Press **Enter** on a repo to expand its full output in a bordered panel. Arrow keys scroll within the panel. **Esc** collapses it. **q** quits and prints a summary.

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

Everything *inside* one body is a single `sh -c` invocation, and ordinary shell rules
apply. `update = git pull; npm ci` runs `npm ci` even when the pull failed, and
reports only `npm ci`'s exit code. Use `&&` between commands you want to stop at the
first failure, or split them across `update` and `post_update` to see which one broke:

```ini
[DEFAULT]
update      = git pull --rebase && npm ci
post_update = npm run build
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
