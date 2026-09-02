# Flash picker re-entry stacks a picker over the picker

- **Date:** 2026-09-02
- **Status:** Fixed

## Symptom

With the flash picker open (`prefix+s` on a source pane), pressing `prefix+s` again launched a
second picker. Its snapshot was the first picker's *rendered frame* — dimmed text, label
characters, the status row — so every "hit" was decorated garbage, and each repeat press nested
another level. Exiting unwound tab by tab instead of returning to the source pane.

## Why it happened

The action entrypoint (`flash`) resolves its target from the focused pane. Once the picker tab has
focus, the focused pane *is* the picker pane (or an `idle` sibling in the replicated split tree).
Nothing distinguished "a pane showing a picker" from "a pane worth picking from": the Herdr layout
response carries pane ids and rects but neither pane commands nor tab labels, so the launch path
happily snapshotted the picker's own output.

The action process and the picker process are separate programs — the `flash` process exits right
after launching the tab — so no in-process state could carry "a picker is already open".

## Plausible but incomplete approaches

- **Detect the picker by tab label or pane command** — not present in the `pane.layout` response.
- **A global or per-workspace "picker running" lock** — wrong scope: it would also block a
  legitimate picker in another workspace, and pid-liveness alone cannot say *which* tab the live
  picker owns.
- **Treat the repeat press as "close the picker"** — requires signalling the pick process; without
  a signal handler the raw-mode/cleanup path never runs and the temporary tab leaks.

## Invariant the fix establishes

*A live `pick` process owns its temporary tab, and no picker may be launched over any pane of an
owned tab.*

Mechanics (`src/herdr/lock.rs`):

- `pick` publishes `herdr-flash-picker-<tab_id>.pid` (its pid) in the temp dir on startup and
  removes it **before** `cleanup_session` closes the tab — Herdr may kill the process together
  with its tab, so a post-close `Drop` is only a fallback.
- `flash` checks the lock right after `pane.layout`: if the target's tab has a live owner, the
  keypress is a no-op (`Ok`), because the picker the user is looking at is already the correct
  result. Tab scoping means `idle` sibling panes are covered too.
- Staleness self-heals: a lock is honored only while its pid is alive **and** still a
  `herdr-flash` binary (`ps -o comm=`); anything else is deleted on sight. Pid liveness alone is
  not enough: a Herdr crash or restart leaves locks behind (Drop never runs on signal death, and
  the per-user temp dir survives everything short of an OS reboot), tab ids are re-allocated from
  scratch, and a recycled pid squatting on a matching stale lock would silently wedge the action
  on that tab until the squatter exits.

Known remaining window: two presses within the launch handshake (before `pick` starts) can still
race; focus is still on the source tab then, so it cannot produce the picker-on-picker stack.

## Regression tests

- `herdr::lock::tests` — lock roundtrip, dead-pid / recycled-pid / garbage-pid staleness, path
  safety.
- `herdr::executor::tests::repeat_invocation_on_a_live_picker_tab_is_a_no_op` — launch stops at
  `pane.layout` when the target tab has a live owner.

## End-to-end verification

1. `just build`, then in a Herdr pane press `prefix+s` — picker opens.
2. Press `prefix+s` again — nothing changes; stderr logs "picker already open in tab …".
3. Escape, press `prefix+s` — a fresh picker opens (lock released).
4. `kill -9` a running `herdr-flash pick`, then `prefix+s` on the source pane — a picker opens
   (stale lock self-cleared).
