# Hide tmprocs-managed background windows from choose-tree and the status line

## Resolution

The first attempt implemented "Suggested fix" section 1 as written — a
pane-level `@tmprocs_hidden` flag plus a session-wide conditional
`window-status-format`/`window-status-current-format`
(`#{?@tmprocs_hidden,,#I:#W#F}`). It looked correct under `list-windows -F`
and `display-message -t <pane>`, but **did not actually hide anything in the
real status bar** (verified live against tmux 3.5a).

Root cause, confirmed empirically: tmux's built-in `status-format[0]`
template references `window-status-format`/`-current-format` indirectly via
`#{T:window-status-format}`. That indirection correctly threads per-window
context for plain builtins (`#I`, `#W`), but loses it for anything nested
one level deeper — a conditional or pattern-match inside the option's value
always evaluates as if false, regardless of whether the flag was a pane
option or a window option. `list-windows -F`/`display-message` don't go
through this indirection, which is why they misleadingly showed the flag
"working."

Actual fix: drop the flag and the conditional entirely. Instead,
**directly override `window-status-format`/`-current-format` to the empty
string, per window**, on each background window tmprocs creates (in
`start_proc` and in `swap_proc_pane`'s `break-pane`-recreated window). A
static empty-string override has no nested expansion for the `#{T:...}`
indirection to lose context on, so it renders correctly. No session-wide
option is touched at all — only the windows tmprocs itself creates.

Deliberately **out of scope**: the global `choose-tree -f` key rebind
(section 3). Rebinding `s`/`w` in the prefix key table is global to the tmux
server, not scoped to tmprocs's session, and isn't restored on exit — too
invasive to apply automatically. Left as a config recipe for users who want
it; see section 3 below.

## Problem

`tmprocs` (`src/tmux.rs`) manages each subprocess as a tmux **window** (not a
session) inside the user's own session, named with a `_tp<pid>_<name>`
prefix. Only the currently-selected process's pane is actually visible — it
gets `join-pane`'d into the right half of the hub window. All other
managed processes sit as detached background windows in the same session.

The existing prefix scheme keeps these out of the **collapsed**
`choose-tree -s` (`ctrl+b,s`) view, per the comment in `tmux.rs`:

> Manages process windows as hidden windows in the user's own tmux session,
> using a per-instance prefix so they don't pollute the session chooser.

But they still leak through in two places:

1. **The status line** — the default `window-status-format` /
   `window-status-current-format` render every window in the session, so
   every background `_tp...` window shows up in the status bar.
2. **Expanded `choose-tree`** — expanding a session node (or using
   `choose-tree -w` / `ctrl+b,w`) walks every window/pane in the session, so
   the background windows show up there too.

## Verified against tmux source (this was checked against a local tmux build, not assumed)

- `choose-tree`/`choose-window` accept `-f <filter-format>`. The filter is
  evaluated **per pane** via `window_tree_filter_pane()` in
  `window-tree.c`, which calls `format_single(NULL, filter, NULL, s, wl,
  wp)` — i.e. it has the *pane* in scope, not just the session/window.
- If a single-pane window's only pane fails the filter,
  `window_tree_build_window()` drops the whole window node
  (`goto empty; ... mode_tree_remove(...)`). If every window in a session is
  dropped this way, `window_tree_build_session()` drops the session node
  too. So filtering at the pane level is sufficient to hide a window
  entirely from both collapsed and expanded views.
- Format lookup for a custom `@option` (`format_find()` in `format.c`)
  checks `ft->wp` (pane) before `ft->w` (window) before session/global.
  `format_defaults()` auto-fills `ft->wp` to the window's *active* pane
  when no explicit pane is given (this is what `window-status-format`
  uses). So a **pane-level** user option is visible both to `choose-tree
  -f` and to `window-status-format`, as long as it's set on the relevant
  pane.
- Crucially, **pane-level options survive `break-pane`/`join-pane`**, while
  window-level options do not (the window object that held them is
  destroyed/replaced). `tmux.rs` already relies on this fact for
  `remain-on-exit` — see `set_pane_remain_on_exit()`, with the comment
  "regardless of whichever window it currently lives in." The same
  mechanism should be used for the hidden flag, not a window-level option.
- `list-panes` and `list-windows` both accept `-f <filter>` as well, so
  "list the hidden ones" is a plain tmux command, no new tmux feature
  needed.

## Suggested fix

### 1. Add a pane-level flag, toggled at existing transition points

Add a helper next to `set_pane_remain_on_exit`:

```rust
/// Mark whether a pane should be hidden from choose-tree / the status line.
fn set_pane_hidden(pane_id: &str, hidden: bool) {
    let _ = Command::new("tmux")
        .args([
            "set-option", "-p", "-t", pane_id, "@tmprocs_hidden",
            if hidden { "1" } else { "0" },
        ])
        .status();
}
```

Then flip it at the same four places that already manage
`remain-on-exit`/window membership, so the flag always reflects "this pane
is currently a detached background window" vs. "this pane is currently
joined and visible":

- **`start_proc`** — the freshly created window is background-only at this
  point. After creating it, mark its pane hidden:
  `set_pane_hidden(&pane_id_of(&window_target), true)`.
  (Needs a way to get the pane id for a brand-new window — e.g. add `-P -F
  '#{pane_id}'` to the `new-window` call and capture stdout instead of
  discarding it, similar to how `join_pane_right` resolves
  `right_pane_id`.)
- **`join_pane_right`** — once `right_pane_id` is computed (the pane is now
  visible), call `set_pane_hidden(&right_pane_id, false)` right next to
  the existing `set_pane_remain_on_exit(&right_pane_id)` call.
- **`restart_shown_proc_pane`** — same as above: after resolving the new
  `right_pane_id`, clear the flag (`false`) alongside
  `set_pane_remain_on_exit`.
- **`swap_proc_pane`** — two panes change roles in one call:
  - the pane being broken back into the background
    (`old_window_name`/`right_pane_id`) needs `set_pane_hidden(..., true)`
  - the newly joined pane (the resolved `right_pane_id` after the swap)
    needs `set_pane_hidden(..., false)`, alongside the existing
    `set_pane_remain_on_exit` call.

This mirrors exactly the existing `remain-on-exit` handling, so it's a
small, consistent diff rather than a new pattern.

### 2. tmux.conf: hide flagged windows from the status line

```tmux
set -g window-status-format          '#{?@tmprocs_hidden,,#I:#W#F}'
set -g window-status-current-format  '#{?@tmprocs_hidden,,#I:#W#F}'
```

Known rough edge: a hidden window still consumes a
`window-status-separator` slot, so you may see a stray double-space where a
hidden window used to be. Cosmetic only — not worth chasing unless it's
visually annoying in practice.

### 3. tmux.conf: filter hidden panes out of choose-tree

Rebind whatever currently invokes `choose-tree`/`choose-window` (`ctrl+b,s`,
`ctrl+b,w`, etc.) to add `-f`:

```tmux
bind-key s choose-tree -Zs -f '#{!=:#{@tmprocs_hidden},1}'
bind-key w choose-tree -Zw -f '#{!=:#{@tmprocs_hidden},1}'
```

(Adjust flags to match whatever the current bindings already use — the
only addition needed is the `-f '#{!=:#{@tmprocs_hidden},1}'` filter.)

### 4. A way to list the hidden ones

No tmux feature is missing for this — it's a plain filtered list command:

```tmux
bind-key H display-popup -E "tmux list-panes -a -f '#{==:#{@tmprocs_hidden},1}' \
  -F '#{session_name}:#{window_name} #{pane_id}  #{pane_current_command}'"
```

or as a plain shell alias/script if a popup isn't wanted:

```sh
tmux list-panes -a -f '#{==:#{@tmprocs_hidden},1}' \
  -F '#{session_name}:#{window_name} #{pane_id}  #{pane_current_command}'
```

## Tradeoffs

- This is entirely config + a small, mechanical Rust diff (one helper
  function, four call sites already touched for `remain-on-exit`/window
  membership) — no tmux core changes required.
- Correctness depends on flipping the flag at all four transition points
  consistently. If a future code path joins/breaks a pane without going
  through `join_pane_right`/`restart_shown_proc_pane`/`swap_proc_pane`, it
  will need the same treatment or the flag will go stale (pane shown when
  it shouldn't be, or vice versa).
- Minor cosmetic artifact in the status line separator noted above.
