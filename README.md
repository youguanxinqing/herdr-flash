# Herdr Flash

**English** · [简体中文](README.zh-CN.md) · [日本語](README.ja.md)

Herdr Flash is a [Herdr](https://herdr.dev) plugin that brings the [flash.nvim](https://github.com/folke/flash.nvim) copy flow to terminal panes: search the visible text, jump to a match by label, select with vim motions, and yank to your system clipboard.

Where hint pickers decide *for* you what a token is, Flash separates the two questions: the search answers **where to look**, and the motions answer **what to take**. Type a few characters, land the cursor on the hit, then pull exactly the text you want — a word, half a URL, three lines.

<table>
<tr>
<td width="33%" align="center" valign="top">
<a href="docs/images/01-search.png"><img src="docs/images/01-search.png" width="280" alt="Search phase: every match of the query mi is highlighted, with a one-key label past each hit"></a>
<br><sub><b>1 · Search</b><br>Typing <code>mi</code> lights every hit in the pane and drops a one-key label just past each one; the status row keeps the count.</sub>
</td>
<td width="33%" align="center" valign="top">
<a href="docs/images/02-char-jump.png"><img src="docs/images/02-char-jump.png" width="280" alt="Character jump: f v labels every v ahead of the cursor as a jump target"></a>
<br><sub><b>2 · Jump by character</b><br>With the cursor placed, <code>f v</code> turns every <code>v</code> ahead of it into a labeled target — one key lands there, any other key cancels.</sub>
</td>
<td width="33%" align="center" valign="top">
<a href="docs/images/03-select-yank.png"><img src="docs/images/03-select-yank.png" width="280" alt="Selection phase: a charwise selection spanning several lines, ready to yank"></a>
<br><sub><b>3 · Select and yank</b><br><code>v</code> opens a charwise selection that vim motions extend across lines; <code>y</code> copies it to the system clipboard.</sub>
</td>
</tr>
</table>

## The flow

1. **Search** — invoke the action on a focused pane and start typing. Every match highlights, and a one-letter label appears just past each hit. Backspace widens the search; Enter jumps to the first match.
2. **Cursor** — press a match's label (or Enter) and a cursor lands on the first character of that hit. Nothing is selected yet.
3. **Select and yank** — `v` starts a charwise selection, `V` a linewise one. Extend with vim motions, then `y` (or Enter) copies the selection to the system clipboard.

Escape or Ctrl-C exits from any phase.

### Keys

| Phase | Key | Action |
|-------|-----|--------|
| search | any character | extend the query (labels are consumed as jumps, never as query text) |
| search | Backspace | shrink the query |
| search | Enter | jump to the first match |
| cursor / select | `h j k l` | move by cell |
| cursor / select | `w b e` | word forward / back / end |
| cursor / select | `0 $` | line start / end |
| cursor / select | `t{char}` `f{char}` | till / find a character, forward across lines |
| cursor / select | `T{char}` `F{char}` | the same, searching backward from the cursor |
| cursor / select | `v` / `V` | start (or drop) a charwise / linewise selection |
| select | `o` | swap the cursor and the anchor |
| select | `y` or Enter | yank the selection (inert while no selection is active) |
| cursor / select | Backspace | back to search, query intact |
| anywhere | Esc / Ctrl-C | exit without copying |

A `t`/`f`/`T`/`F` with one hit jumps straight there. Several hits get one-key labels — the label picks one, any other key cancels — and when the hits outnumber the label alphabet, `Space` pages the labels onto the hits still uncovered, wrapping around.

A keystroke that happens to be a live label jumps instead of extending the query; one Backspace returns to the search with the query still there.

## Configuration

By default a yank closes the picker. To stay open for several grabs in a row (Escape to leave):

```bash
CONFIG_DIR="$(herdr plugin config-dir youguanxinqing.herdr-flash)"
$EDITOR "$CONFIG_DIR/config.toml"
```

```toml
[flash]
exit_on_yank = false
```

Each copy is acknowledged on the status row and the search resets for the next grab.

### Colors

The default palette is the flash.nvim look. Five styles cover everything the picker draws:

| Style | What it paints | Default |
|-------|----------------|---------|
| `unmatched` | the pane text around your matches, dimmed so hits stand out | grey `#7a8294` |
| `match` | every hit of the current query, and the query on the status row | white on blue `#3e68d7` |
| `label` | the jump key drawn just past each hit, and the `flash` chip | white on magenta `#ff007c`, bold |
| `selection` | the body of an active `v`/`V` selection | dusk `#4d3a4a` background |
| `cursor` | the movable end of the cursor/selection | black on white |

Override any of them under `[colors]` in the same config file. Each style takes `fg` and `bg` as `"#rrggbb"` hex or `"none"` to clear that channel back to the terminal default, plus a `bold` boolean:

```toml
[colors]
unmatched = { fg = "#6f7788" }               # dim the backdrop further
label = { bg = "#e91e63" }                   # a softer pink; fg and bold keep their defaults
match = { bg = "none", fg = "#e5c07b" }      # no fill — plain yellow text instead
```

Omitted styles and omitted keys keep their defaults; an invalid value is ignored with a warning on stderr rather than breaking the picker. Colors are 24-bit, which Herdr and every modern terminal support.

## Requirements

- Herdr 0.7.4 or newer
- Rust/Cargo (installs currently build from source; no prebuilt release assets yet)
- A system clipboard command:
    - macOS: `pbcopy`
    - Linux Wayland: `wl-copy`
    - Linux X11: `xclip` or `xsel`

## Install

```bash
herdr plugin install youguanxinqing/herdr-flash
```

To install a specific branch, tag, or commit, pass `--ref`. From a local checkout:

```bash
herdr plugin link .
```

Verify Herdr can see the action:

```bash
herdr plugin action list --plugin youguanxinqing.herdr-flash
```

To remove it again: `herdr plugin uninstall youguanxinqing.herdr-flash` (or `herdr plugin unlink youguanxinqing.herdr-flash` for a linked checkout).

## Keybinding

Add a `plugin_action` binding to your Herdr config, then `herdr server reload-config`:

```toml
[[keys.command]]
key = "prefix+s"
type = "plugin_action"
command = "youguanxinqing.herdr-flash.flash"
description = "flash: search visible text, then select and yank"
```

## Engineering notes

Complex regressions and the invariants that prevent them are recorded in
[`docs/bugs/`](docs/bugs/README.md). In particular, the
[Flash picker entry blink](docs/bugs/flash-picker-entry-blink.md) documents the hidden-tab paint
handshake and the source-versus-picker geometry split.

## Credits

- [flash.nvim](https://github.com/folke/flash.nvim) by folke — the interaction model this plugin is an homage to: search-driven navigation with labeled jumps, where landing the cursor and choosing the text are separate acts.
- [herdr-pluck](https://github.com/rmarganti/herdr-pluck) by rmarganti — Herdr Flash began as a fork of it, and the pane snapshotting, picker plumbing, and renderer still stand on that work. If you want one-keystroke grabbing of pattern-shaped tokens (URLs, SHAs, paths), pluck is the right tool; Flash is for when you know the content but not the shape.

## License

MIT — see [LICENSE](LICENSE).
