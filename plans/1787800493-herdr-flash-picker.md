# Herdr Flash: search, select, yank

Retrospective design plan for the flash picker — the feature this repository exists for. Written as-built; the project began as a fork of rmarganti/herdr-pluck (see README credits) and reuses its pane snapshotting, picker plumbing, and renderer.

## Problem Statement

Pluck-style hint pickers answer one question well: "grab that token whose shape I recognize" (URL, SHA, path). They cannot answer the other question users hit constantly: "I can see the text I want, but it is not a token" — a fragment inside a URL, an arbitrary word, a span across two tokens, a whole line. Pattern lists never enumerate those shapes, so the workflow falls back to mouse selection or Herdr's copy mode.

flash.nvim solved this inside Neovim: incremental search narrows the visible matches, a label jump lands the cursor, and ordinary motions decide what to take. Herdr panes deserve the same flow.

## Solution

Add a `flash` action to the picker chassis. It captures the exact visible viewport of the focused pane, then runs a three-phase loop in an overlay pane:

1. **Search** — every typed character narrows the matches; a one-letter label renders just past each hit. Backspace widens. Enter targets the first match.
2. **Cursor** — a label (or Enter) lands a bare cursor on the first cell of that hit. The search answers *where*; nothing is selected yet.
3. **Select and yank** — `v`/`V` start a charwise/linewise selection, vim motions extend it, `y` or Enter copies it through the system clipboard chain and exits (or stays open when `exit_on_yank = false`, acknowledging the copy on the status row).

Escape and Ctrl-C exit from any phase. Backspace in cursor mode returns to the search with the query intact.

## Implementation Decisions

1. **Labels can never collide with search input.** A character that could extend any current match is withheld from the label alphabet, so every keystroke has exactly one meaning. Labels draw one cell past their hit — the hit's own cells hold the characters the user just typed and must stay visible.
2. **The cursor lands on the hit, not on a widened token.** Search decides where to look; what to take is the user's call. No auto-expansion to the surrounding word or pattern.
3. **Selection is a small vim.** A `Grid` over the visible rows supports `hjkl`, `w/b/e` with vim word classes, `0/$`, `t{char}/f{char}`, `o` to swap ends, and linewise mode. Escape always exits rather than unwinding one level; Backspace is the un-type key that hands back the search.
4. **Frames overwrite; they never clear.** Herdr's pane emulation drops DEC 2026 synchronized-update guards under its render timeout, so clear-then-repaint occasionally composites as a blank frame — visible flicker. Every frame is therefore a full-coverage repaint fitted to the live terminal size (rows pin the status line to the real bottom; every row is clipped/padded to the live column count), assembled in memory and handed to the pty as a single write. The only `Clear(All)` happens once at picker entry. DEC 2026 guards are still emitted as best-effort.
5. **The query is always visible.** The status row shows the flash chip, the query, and a mode legend in every phase; a keystroke consumed as a label jump must be discoverable, and Backspace recovers it.
6. **Config travels in the snapshot.** `[flash] exit_on_yank` is read in the action process (the picker pane does not inherit `HERDR_PLUGIN_CONFIG_DIR`) and carried inside the launch payload, the same way custom patterns travel.
7. **Flash is the only published action.** The pluck/open-url pickers remain functional in the binary for compatibility but are hidden from `--help` and absent from `herdr-plugin.toml`; upstream herdr-pluck covers that use case.

## Testing Decisions

- The picker loop is driven end-to-end through `InputSource`/`Clipboard` fakes writing to a `Vec<u8>`, asserting on outcomes and copied text rather than internals.
- The terminal emission protocol is pinned byte-exactly: sync guards, no mid-session `[2J`, per-span style resets. Those tests force crossterm's color output on so an ambient `NO_COLOR` cannot flip the assertions.
- Frame composition has direct tests for the live-size invariants: status pinned to the bottom row, every row clipped/padded to the live column count.

## Out of Scope

- Searching scrollback; the picker sees exactly the visible viewport.
- Regex or fuzzy queries; the search is a literal substring match.
- Multiple cursors or discontiguous selections.
- A NO_COLOR attribute theme (Reverse/Bold instead of colors).
- OSC52 clipboard, pasting into the pane, or opening a flash selection as a URL.

## Further Notes

### Technical References

- flash.nvim (folke) — the interaction model: https://github.com/folke/flash.nvim
- herdr-pluck (rmarganti) — the fork origin and picker chassis: https://github.com/rmarganti/herdr-pluck
- no-color.org — why the exact-escape tests pin color output explicitly.
