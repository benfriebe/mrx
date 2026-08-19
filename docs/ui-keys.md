# ui mode keys

Every binding `mrx ui` has, by input mode. The `?` overlay inside the app lists the
same set; if this file and the overlay ever disagree, the overlay is right.

## Why the footer shows less than this

The footer at the bottom of the screen advertises a handful of bindings, not all of
them:

- It shows only what fits the current width, and only whole bindings, with `…`
  standing in for what it dropped.
- `? help` is budgeted first and drawn last, so it survives every width. That is the
  route to everything else.
- Some bindings are never given footer room even when there is space. They are marked
  **overlay** in the tables below: bound, listed under `?`, and left out of a line
  that has better uses for the columns.
- `esc cancel` is appended only while a run is live, since a hint for a key that does
  nothing is worse than no hint.

## List mode

The default mode: the table of repos, no overlay open.

| Keys | Action | Footer |
|------|--------|--------|
| `j`/`k`, `↓`/`↑` | Move the cursor one row | hinted |
| `Ctrl-D`/`Ctrl-U` | Move half a page | overlay |
| `g`/`G` | Jump to the first or last row | overlay |
| `space` | Toggle the cursor row's selection, then move on | hinted |
| `a` | Select every row the filter currently shows | overlay |
| `A` | Select the whole set, regardless of the filter | overlay |
| `c` | Clear the selection | overlay |
| `i` | Invert the selection | overlay |
| `/` | Start an incremental filter on repo name | hinted |
| `u` | Run `update` on the selection | hinted |
| `s`/`f`/`d` | Run `status`, `fetch`, `diff` on the selection | overlay |
| `:` | Open the action palette | hinted |
| `r` | Open the run-command prompt | overlay |
| `R` | Re-probe the selection (or everything, with nothing selected) | overlay |
| `S` | Open the sort menu | overlay |
| `!` | Open `$SHELL` (`sh` if unset) in the cursor row's repo | overlay |
| `o` | Open the cursor row's repo in `$EDITOR` (`vi` if unset) | overlay |
| `Enter` | Open the detail view for the cursor row | hinted |
| `tab` | Open the set picker | hinted |
| `F` | Toggle the freshness poll | overlay |
| `Ctrl-A` | Toggle auto-update | overlay |
| `Ctrl-R` | Reload the active config in place | overlay |
| `m` | Toggle mouse capture | overlay |
| `?` | Open the keymap overlay | hinted |
| `Esc` | Cancel a live run; a no-op with nothing running | appended |
| `q`, `Ctrl-C` | Quit, asking first if a run is live | hinted |

Two things about modifiers:

- A held `Ctrl` that matches nothing in this mode is swallowed rather than falling
  through to the plain-letter shortcut of the same letter. `Ctrl-U` is a common
  readline chord and must never reach `u`, which would start an update.
- `Ctrl-C` is handled ahead of every mode below, so it always quits (or confirms a
  quit already being asked about) rather than being captured as text.

## Detail view

Opened with `Enter` on a row. The list collapses to a sidebar beside the output, or
takes the whole screen below about 100 columns.

| Keys | Action | Footer |
|------|--------|--------|
| `tab` | Hand the keys to the other pane | hinted |
| `j`/`k`, `↓`/`↑` | Move the cursor with the list focused, scroll the output with the output focused | hinted |
| `Ctrl-D`/`Ctrl-U` | Scroll the output half a page | hinted |
| `Enter` | Hand the keys to the output pane | overlay |
| `y` | Copy the visible step's output | hinted |
| `o` | Open the whole transcript in `$EDITOR` | overlay |
| `!` | Open `$SHELL` in the cursor row's repo | overlay |
| `m` | Toggle mouse capture | overlay |
| `Ctrl-R` | Reload the active config in place | overlay |
| `?` | Open the keymap overlay | hinted |
| `Esc` | Close the detail view, back to the full-width list | hinted |
| `q`, `Ctrl-C` | Quit | hinted |

There is one footer under the split rather than one per pane, and its keys are the
detail view's: with the split open, those are what every keystroke reaches, whichever
pane the pointer happens to be over.

## Filter capture

While `/` is capturing, everything except the three keys below is literal filter
text, including letters that are shortcuts in list mode. That is why there is no
`j`/`k` here: they would be typed into the filter.

| Keys | Action |
|------|--------|
| `Esc` | Drop the filter and leave capture |
| `Enter` | Keep the filter and leave capture |
| `Backspace` | Delete the last character |
| anything else | Appended to the filter |

`Esc` in list mode is a deliberate no-op on the filter, so a committed filter has no
key of its own that drops it. `/` starts a fresh search from the full list instead,
which is how you clear one you kept.

## Action palette

Opened with `:`. Same shape as filter capture: only navigation and the exits are
special, everything else is text.

| Keys | Action |
|------|--------|
| `Esc` | Close the palette |
| `Enter` | Run the highlighted entry |
| `↑`/`↓` | Move the highlight |
| `Backspace` | Delete the last character of the palette filter |
| anything else | Appended to the palette filter |

The palette navigates with the arrow keys rather than `j`/`k` for the same reason the
filter has no `j`/`k`: letters are text here.

## Set picker

Opened with `tab`. No text capture, since the list of sets is short enough to scan.

| Keys | Action |
|------|--------|
| `j`/`k`, `↓`/`↑` | Move the highlight |
| `Enter` | Switch to the highlighted set |
| `Esc` | Close without switching |

## Sort menu

Opened with `S`. One key deep: it takes a column and closes.

| Keys | Action |
|------|--------|
| `r` | Order by REPO, the name order the table opens in |
| `b` | Order by BRANCH |
| `s` | Order by STATE, most uncommitted work first |
| `u` | Order by SYNC, furthest behind its upstream first |
| `l` | Order by RESULT, failures first, then the repos that changed |
| anything else | Close without changing the order |

Choosing the column already sorted flips its direction; choosing a different one opens
it at its own natural direction rather than carrying the last column's over. The sorted
column's header carries `↑` or `↓`, and the header line names the order (`sort STATE ↓`)
whenever it is not the name order the table opens in, so a pane too narrow for the
sorted column still says which way the rows are running.

The menu swallows every key it does not bind, `s` included: behind the menu that means
"order by STATE", never "run status".

RESULT orders by how much a row is asking for attention rather than by its text:
failed, then succeeded having changed something, then succeeded having changed nothing,
then still running, skipped, and never run. The split matters in the ordinary case where
nothing failed, since a whole set reading `up to date` would otherwise be one flat tie
with the handful that actually moved scattered through it in name order.

## Confirmation prompts

Two prompts can be up, and they answer differently. Both swallow everything they do
not bind.

Running on a dirty or unprobed selection:

| Keys | Action |
|------|--------|
| `y`, `Enter` | Run on the whole selection |
| `c` | Narrow to the cursor row alone. Offered only when the run covers everything because nothing was selected |
| `n`, `Esc` | Cancel the run |

Quitting while a run is live:

| Keys | Action |
|------|--------|
| `y`, `Enter`, a second `Ctrl-C` | Quit |
| anything else | Stay open |

## Keymap overlay

`?` opens it. Any key dismisses it, and that key does nothing else: hunting for the
one exit key is the annoyance a help screen is least entitled to. `Ctrl-C` is the
exception, and still quits.

## Mouse

| Gesture | Action |
|---------|--------|
| Click a row | Move the cursor to it |
| Click the row already under the cursor | Open its detail view |
| Click a row in the detail sidebar | Move the cursor, without reopening anything |
| Wheel | Scroll whichever region is under the pointer: the list, or the output once the detail view is open |
| Drag down the output pane | Select the lines it covers, copied when the button comes up |
| Click with no drag in the output pane | Clear that selection again |
| Drag anywhere else | Ignored, with a one-time hint about getting native selection back |

Clicks have no target while a modal is up (the palette, the set picker, either
confirmation prompt) and none inside the output pane itself, where a press starts a
text selection rather than hitting anything.

## Behaviour the keymap cannot show

These three notes ride along with the `?` overlay, because a table of keys has
nowhere to put them:

- An empty selection acts on every repo on screen.
- An empty SYNC cell means nothing has fetched that repo yet. `u`, `f`, `F` or a
  pull elsewhere all settle the distance.
- Mouse capture takes the terminal's own selection away. Hold Option/Shift, or press
  `m`, to get it back.
