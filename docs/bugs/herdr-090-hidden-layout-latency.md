# Herdr 0.9.0 leaves the hidden picker at its estimated PTY size

## Symptom and measured cause

After upgrading both the client and server to 0.9.0, `prefix+s` acquired a noticeable pause.
A socket timing proxy around the real release binary reproduced a 394 ms launch:

| Operation | Start (ms) | Duration (ms) |
|-----------|------------|---------------|
| `pane.layout` | 9.7 | 1.2 |
| `pane.read` | 12.2 | 0.8 |
| `layout.apply` | 13.7 | 10.9 |
| `pane.focus` | 329.7 | 53.4 |

The picker expected 206×52 but its PTY remained 207×52. It exhausted its 250 ms geometry
wait; the launcher also exhausted the 300 ms painted-marker wait. This is not evidence that
206 should be changed to 207: the latter is the initial estimate, before pane chrome is applied.

In the [0.9.0 server geometry dispatch](https://github.com/herdrdev/herdr/blob/v0.9.0/src/server/headless/client_views.rs),
`public_request_may_change_geometry` includes `PaneResize` but omits `LayoutApply`.
Consequently the hidden tab can retain the size assigned by `estimate_pane_size` rather than
receiving its final size after the split tree is created. Focusing the pane eventually triggers
geometry reconciliation, but that occurs after our pre-focus wait.

## Fix and compatibility

After successfully parsing `layout.apply`, send `pane.resize` to the returned picker pane with
`direction="right"` and **explicit `amount=0.0`**. Omitting `amount` would resize a split by 5%.
The request triggers Herdr's geometry reconciliation without changing the split ratios or focus.
Then preserve the existing painted → focus → ready ordering and all timeout/cleanup behavior.
Do not replace the painted barrier with an earlier focus or accept an arbitrary PTY size.

The zero-delta semantics were checked in the tagged sources for
[0.7.4](https://github.com/herdrdev/herdr/blob/v0.7.4/src/app/api/panes.rs),
[0.8.2](https://github.com/herdrdev/herdr/blob/v0.8.2/src/app/api/panes.rs), and
[0.9.0](https://github.com/herdrdev/herdr/blob/v0.9.0/src/app/api/panes.rs).
No version subprocess or new minimum version is needed. If a server rejects the optional refresh,
report it and retain the successfully created tab/pane IDs so the legacy bounded preview wait,
focus, and cleanup can proceed. Never turn this optional refresh failure into a leaked hidden tab.

The workaround relies on the server reconciling background tabs. Herdr 0.9.0 does this for a
single app client; with multiple app clients, it only reconciles tabs with geometry controllers.
The existing bounded fallback still applies to an unviewed tab in that case. The measurements
below do not establish multi-client performance or compositor-level absence of flicker.

## Regression coverage and verification

- `hidden_layout_refreshes_geometry_before_waiting_for_first_frame` drives the real socket client
  and launcher against a server that withholds `painted` until the geometry refresh. Before the
  fix it fails after the 300 ms wait; after the fix it paints before focus.
- `unsupported_geometry_refresh_preserves_legacy_launch` exercises an `unknown_method` response
  and verifies that launch still completes through the legacy painted/ready barriers.
- Existing executor tests retain focus failure compensation and zoomed-source behavior.

Run `just verify`, `just test-no-color`, and `just build`. The linked plugin uses the staged
`bin/herdr-flash` immediately on its next invocation.

Live checks on 2026-09-16, using the staged release binary against the running 0.9.0 server:

- Three warm launches: **28.0 / 29.2 / 31.3 ms**, without preview errors.
- An isolated two-pane source: **154.5 ms**, two mirror panes, Escape restored the source pane
  and removed the picker tab.
- The same source zoomed: **51.9 ms**, one picker pane, Escape restored and cleaned up correctly.
- The first execution after restaging took 516 ms, with 458 ms **before the first socket request**.
  Treat that cold process-start cost separately from the fixed geometry-barrier delay.

For a live recheck, record monotonic timestamps around the action process and the four original
socket requests plus `pane.resize`; inspect both action stderr and picker preview diagnostics.
Use a temporary source tab for split/zoom cases, retain returned IDs, and restore the original
focus and close only test tabs in a finally block. Do not infer latency from CLI timing alone.
