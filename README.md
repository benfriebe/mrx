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
| `mrx list` / `ls` | List configured repos (no TUI) |

### Options

| Flag | Description |
|------|-------------|
| `-j <N>` | Max parallel jobs (default: min(cpus, 8)) |
| `-c <file>` | Config file (default: `~/.mrconfig`) |
| `-d <dir>` | Working directory (default: config file's parent) |
| `-v` | Verbose output |
| `-n` | No recurse |
| `-f` | Force |

### Examples

```
mrx status              # quick overview of all repos
mrx fetch -j 32         # fetch all 32 repos at once
mrx run "git log -1"    # last commit in each repo
mrx list                # print repos without TUI
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

Section names are relative paths from the config file's parent directory. Both HTTPS and SSH clone URLs are supported.
