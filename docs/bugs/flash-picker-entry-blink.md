# Flash picker entry blink and geometry drift

- **Status:** fixed
- **Recorded:** 2026-08-27
- **Observed with:** Herdr 0.8.2
- **Trigger:** opening Flash from a focused, zoomed pane in a split tab

## Symptom

Opening Flash briefly exposed a blank or partially sized temporary tab before the source text was
drawn. Removing the blank frame revealed a related layout error: the redrawn content touched the
top and left edges while all recovered space accumulated below or to the right.

This was not one rendering bug. It was a race between tab focus, asynchronous PTY resize, first
paint, and zoom reconstruction, followed by an incorrect assumption that the source pane and the
temporary picker have the same content rectangle.

## Failed sequence

The old launch path effectively did this:

```text
create layout and focus its tab
    -> picker PTY starts at an intermediate size
    -> source tab is no longer visible
    -> picker clears or paints its first frame
    -> zoom/resize reaches the final geometry
```

Any interval between the first two steps and the final paint was visible. Folding a clear into a
single terminal write reduced one source of blinking, but it could not fix focus happening before
the picker was ready. Applying zoom after focus introduced another visible resize.

## Root causes

### Focus was also the reveal operation

`layout.apply` used to expose the new tab before its picker process had reached final PTY geometry
or emitted frame one. Process startup and resize propagation are asynchronous, so command order
alone did not establish a visual ordering.

### A zoomed source is not geometrically identical to a single picker pane

The captured viewport belongs to the source pane. The process paints into the temporary picker
pane. Borders and gutters make those rectangles different even when both occupy the same terminal
area.

In the reproduced 80x24 area:

| Geometry | Width | Height | Used for |
|----------|------:|-------:|----------|
| Zoomed source content | 77 | 22 | captured rows, wrapping, match and selection coordinates |
| Single-pane picker content | 79 | 24 | PTY readiness and final frame bounds |

Using picker dimensions to interpret captured text breaks coordinates. Waiting for source
dimensions in the picker can accept an intermediate resize or wait for a size that will never be
the final one. These are separate domain values and must remain separate in `PickerSnapshot`.

### Recovered border cells need deliberate placement

The single-pane picker recovers one row and two columns after accounting for the bottom status row.
Generic bottom/right filling placed every recovered cell on one side. The intended composition is:

- one spare row above the captured content;
- one spare column on the left and one on the right, including the status row;
- no visual padding when the live pane lacks the full required slack.

The last rule matters: padding must never hide an additional searchable row or column in a tight
pane.

## Fix and required ordering

The temporary layout is created hidden and the source tab remains visible until the first complete
picker frame exists:

```text
action process                         picker process
--------------                         --------------
capture source geometry and text
write snapshot
layout.apply(focus=false)  ----------> start in hidden pane
                                       wait for final picker PTY size
                                       paint a complete entry preview
                         <------------ atomically publish painted marker
wait for painted marker
focus picker pane
atomically publish ready marker ------> enter the interactive input loop
```

The markers represent different facts and must not be merged:

- `painted` means the tab is safe to reveal;
- `ready` means focus has moved and the picker may read interactive input.

Marker waits are bounded and preview failure is best-effort so a rendering problem does not make
the action permanently unusable. Marker publication uses a temporary file plus rename so waiters
never observe a partial marker.

For a focused, zoomed source, the hidden layout contains one picker pane. Recreating the hidden
split tree and calling `pane.zoom` after focus would reintroduce the visible resize.

## Invariants

Changes to picker launch or rendering must preserve all of these:

1. A temporary picker tab is not focused until a complete frame has been painted at final PTY
   geometry.
2. Source content geometry drives viewport reconstruction and selection coordinates; picker
   content geometry drives PTY readiness.
3. A zoomed source launches a single hidden picker pane and does not zoom after focus.
4. Incremental Flash frames cover every live terminal cell without a standalone clear write.
5. Recovered space becomes padding only when it fits without cropping captured content.
6. Cleanup returns to the explicit source tab and never closes it as the temporary tab.

## Regression coverage

The main guards are intentionally spread across the orchestration and rendering layers:

| Test | Protects |
|------|----------|
| `picker_layout_is_created_without_stealing_focus` | hidden layout application |
| `launch_waits_for_painted_frame_before_focus_and_ready_barrier` | painted -> focus -> ready ordering |
| `zoomed_source_uses_one_hidden_picker_pane` | zoom path and distinct 77x22 / 79x24 geometry |
| `the_entry_preview_paints_the_initial_search_frame` | useful frame one without a standalone clear |
| `frames_are_clipped_and_padded_to_the_live_column_count` | exact overwrite width after resize |
| `spare_picker_height_becomes_one_top_padding_row` | vertical padding without row loss |
| `spare_picker_width_becomes_one_padding_column_on_each_side` | symmetric horizontal padding without column loss |
| `a_one_shot_emit_folds_the_clear_into_the_synchronized_frame` | no separately composited blank clear frame |

Run the repository gate after changing this path:

```bash
just verify
just test-no-color
just build
```

`NO_COLOR` is relevant because terminal styling must not change cell coverage or frame ordering.

## End-to-end check

Unit tests cannot prove what a compositor displays between protocol events. Test from a zoomed pane
inside a split tab:

```bash
herdr plugin action invoke flash --plugin youguanxinqing.herdr-flash
```

Verify all of the following:

- the source remains visible until the completed Flash frame replaces it;
- there is no blank tab and no second resize;
- the first row is blank and content begins after one blank column;
- the status row is pinned to the bottom and has the same left/right inset;
- Escape returns focus to the original pane and removes the temporary tab;
- plugin stderr contains no preview, focus, or cleanup error.

When debugging a recurrence, find the temporary pane and inspect its exact leading cells and ANSI
spans:

```bash
herdr pane list
herdr pane read <pane-id> --source visible --format text --raw
```

The absence of `pane.zoom` between layout application and cleanup is also a required signal for
the zoomed path.

## Related code

- `src/herdr/executor.rs` — hidden layout orchestration, painted/focus/ready ordering, zoom path
- `src/herdr/mod.rs` — final PTY wait, entry preview, interactive picker start, cleanup
- `src/herdr/snapshot.rs` — snapshot geometry and atomic marker files
- `src/herdr/protocol.rs` — `layout.apply` request and picker identity
- `src/model.rs` — distinct source and picker geometry
- `src/picker/flash.rs` — complete frame composition and four-sided padding
- `src/renderer/terminal.rs` — synchronized terminal frame emission
