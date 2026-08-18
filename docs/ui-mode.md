# ui mode in depth

What `mrx ui` does beyond its keymap, roughly in the order you meet it. For the
bindings themselves see [ui-keys.md](ui-keys.md).

## Output and colour

Tools keep their own colours here. mrx runs each step through a pipe, which normally
makes a tool turn colour off, so it forces it back on and renders the escape
sequences it gets back:

- `CLICOLOR_FORCE=1` and `FORCE_COLOR=1` cover most modern CLIs.
- git needs its own config-through-environment protocol, so `color.ui=always` is
  passed that way rather than as a flag.
- A `color.ui` the user set themselves loses to that, because the point is to have
  something to render.

Lines that carry no colour of their own still read by severity, but only on `stderr`,
and only from the marker a line leads with (`npm warn ...`, `fatal: ...`,
`[error] ...`, matched against the first three words):

- `warn` and `warning` draw yellow.
- `err`, `error`, `fatal` and `panicked` draw red.
- Everything else on `stderr` draws grey. `stderr` is the "not the data" channel
  rather than an error channel, and git's fetch progress arrives there too, so
  painting the whole stream red would say every run failed.

Copies, saved transcripts and the plain and one-shot output are all stripped back to
plain text.

## The detail split

The split is one frame divided, not two windows: the panes rule off their headers on
the same row, a rule runs between them, and one key line sits under both. Below about
100 columns the sidebar has nowhere left to shrink, so the detail view takes the whole
screen instead and `Esc` is the only way back.

- `tab` hands the keys from one pane to the other, marked by the `▌` in the margin
  and the brighter title. `j`/`k` then either walk the repo list with the output
  following, or scroll the output with the cursor staying put.
- `Enter` on a row whose output is already on screen hands the keys to the output too,
  since the row is no longer the question.
- The line under the detail title says how the run ended (`2 steps · exit 0`,
  `running git pull`, `skipped`) and, when the output is longer than the pane, which
  slice of it you are looking at (`41-60 of 312`). There is no scrollbar, so without
  that line a long transcript gives no clue how much is above or below.
- Output arrives as it is produced, so a long update can be read while it runs. A step
  still going is marked `…` instead of a tick, and the view follows the tail until you
  scroll. Each step is its own labelled section rather than one scrollback, and the
  scroll position is kept per repo.
- `y` copies the visible step's output through `pbcopy`, `xclip` or `wl-copy`,
  whichever is on `PATH`, and falls back to a temp file when none is.

## Result lifetime

A run's result stays on its row for six minutes and then goes back to `·`, so a table
left open all afternoon is not still reporting this morning. `--result-ttl` changes
it: `--result-ttl 30m`, `--result-ttl 90s`, or `--result-ttl off` to keep every result
until the next run replaces it.

## Suspending for an editor or a shell

`o` opens the cursor row's repo in `$EDITOR`, or the whole transcript when the detail
view is open. `!` opens `$SHELL` in the cursor row's repo. `$EDITOR` falls back to
`vi` and `$SHELL` falls back to `sh`.

The app suspends properly to do either: raw mode, the alternate screen and mouse
capture all come off first, so the program gets a normal terminal, and all three come
back once it exits.

## Switching and reloading sets

- `tab` opens a picker over every set `mrx sets` would list, plus the active config
  labelled `(unnamed)` if it is not one of them. Confirming reloads that config and
  restarts the probe from scratch.
- `Ctrl-R` re-reads the active config without changing which one is active, keeping
  the cursor and selection by repo name. An edit that adds a repo above the one you
  are on does not silently redirect the selection onto its neighbour, and a name the
  edit removed just drops out.
- Both are blocked while a run is live: re-numbering the repo list out from under an
  in-flight run's indices would attribute its results to the wrong row.

## Cancelling a run

`Esc` cancels a live run as far as it honestly can:

- Everything still queued behind the job limit is skipped.
- A repo already past its slot keeps running to completion, because `Command::output`
  has no kill.
- The status line says exactly that: `cancelled, 2 queued skipped, 1 still finishing`.

`q` and `Ctrl-C` quit immediately with nothing running. With a run live they ask
first, since losing sight of an in-flight action is not something to do by reflex.

## Freshness and auto-update

The ahead/behind counts only ever reflect the last fetch, so a repo that is ↓3 behind
shows no ↓ at all until something updates the remote-tracking ref. An absent count is
"nobody has asked", which is a different claim from ↓0 and so is never drawn as one.

Anything that fetches counts, not just mrx. The probe reads `FETCH_HEAD`'s timestamp,
so running `update` on a repo, or pulling it in another terminal, settles its count on
the next probe.

- `F` toggles a freshness poll: `git fetch --quiet` across the set on an interval, five
  minutes by default, suspended rather than queued while a run is live.
- `Ctrl-A` layers a narrow, opt-in auto-update on top, fast-forwarding what a poll
  finds behind. It acts only on a repo that is checked out, tracks an upstream, is
  behind, is not ahead, and has no working-tree changes. Anything else is left alone
  and reported rather than fixed.
- `Ctrl-A` refuses to turn on while the poll itself is off, since it has nothing to
  act on without one.
- Both are off by default, and both show in the header the moment either is on
  (`poll 5m`, `poll 5m · auto`). A mode that touches working trees on a timer has no
  business being invisible.

## The persisted session

The set, filter, selection, cursor, sort order, both poll settings, and which repos have
been seen to fetch (so a `↓` count survives a restart) are written to `$XDG_STATE_HOME/mrx/ui.json`
(`~/.local/state/mrx/ui.json` by default) as they change, and restored the next time
`mrx ui` opens.

- A restored filter shows in the header with its match count (`4 of 42 repos · filter`)
  rather than only in the status bar, so it does not look like the config broke.
- `-s` on the command line always wins over whichever set was stored, and a stored set
  that no longer resolves falls back to the ordinary default.
- A name the file remembers that the set no longer has is dropped silently rather than
  treated as an error, because a config edit is not an error.
- The sorted column and its direction are stored separately, since a column reversed in
  place is not the same view as that column freshly chosen. A column this build no longer
  has, or a direction it cannot read, falls back a field at a time rather than together.
- A missing or unparseable file reads exactly like no file at all, so deleting it is a
  supported reset and a crash mid-write can never lock the app out.

## It needs a real terminal

Two invocations exit 2 before anything reads the config:

- `mrx ui` with stdout piped, pointing you at `mrx status` or another non-interactive
  subcommand instead.
- `mrx ui --plain`, since `--plain` disables the interactive view that `ui` opens.
