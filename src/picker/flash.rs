use crate::clipboard::{Clipboard, SystemClipboard};
use crate::flash::{assign_labels, find_query_matches, FlashCandidate, FlashLabel};
use crate::model::{
    PickerOutcome, PickerSnapshot, RenderLine, RenderSpan, RenderStyle, VisibleViewport,
};
use crate::picker::copy::copy_selected_text;
use crate::picker::input::{
    CrosstermInputSource, CursorGuard, InputSource, PickerInputEvent, RawModeGuard,
};
use crate::renderer::{render_flash_labels, render_selection, terminal, token_cell_bounds};
use crate::select::{Grid, Motion, Pos, Selection};
use crate::viewport::map_visible_viewport;
use anyhow::Result;
use std::io::{self, Write};

/// Runs the incremental-search picker against a captured pane snapshot.
pub fn run_flash_picker(snapshot: &PickerSnapshot) -> Result<PickerOutcome> {
    let mut stdout = io::stdout();
    let mut input = CrosstermInputSource;
    let clipboard = SystemClipboard;
    let _raw_mode = RawModeGuard::enable()?;
    let _cursor = CursorGuard::hide()?;
    run_flash_with(snapshot, &mut input, &clipboard, &mut stdout)
}

pub(crate) fn run_flash_with<I, C, W>(
    snapshot: &PickerSnapshot,
    input: &mut I,
    clipboard: &C,
    output: &mut W,
) -> Result<PickerOutcome>
where
    I: InputSource,
    C: Clipboard,
    W: Write,
{
    let width = snapshot.source.target_content_width;
    let height = snapshot.source.target_content_height;
    // Production snapshots always carry the exact viewport; rebuilding one keeps the unit tests and
    // any future capture mode from having to special-case flash.
    let viewport = snapshot.source.visible_viewport.clone().unwrap_or_else(|| {
        map_visible_viewport(snapshot.source.logical_lines.clone(), width, height)
    });

    let grid = Grid::new(&viewport.rows);
    let mut query = String::new();
    let mut selection: Option<Selection> = None;
    let mut pending_char: Option<CharMotion> = None;
    let mut notice: Option<String> = None;

    // No entry clear: every frame is fitted to the live terminal size and paints every cell, so
    // the first frame already covers whatever the fresh pane held. A separate clear reaches the
    // pty as its own write, and the bare cleared screen composites as a visible blank blink.
    loop {
        let candidates = find_query_matches(&viewport.logical_lines, &query);
        let labels = assign_labels(&viewport.logical_lines, &candidates);

        // The snapshot's size and the pane's live size can disagree — resizes propagate
        // asynchronously — so ask the terminal each frame. Rows pin the status to the real bottom
        // row; columns bound every emitted row, because frames overwrite in place instead of
        // clearing: a row wider than the pane would wrap and corrupt the frame, and one narrower
        // would leave stale cells at the right edge.
        let (cols, rows) = crossterm::terminal::size()
            .map_or((width as usize, height as usize), |(cols, rows)| {
                (cols as usize, rows as usize)
            });
        let (cols, rows) = (cols.max(1), rows.max(1));
        let content = match &selection {
            Some(active) => render_selection(&viewport, active, width, height),
            None => render_flash_labels(&viewport, &labels, width, height),
        };
        let status = match &selection {
            Some(active) => legend_line(active, &query, cols),
            None => prompt_line(&labels, &query, notice.as_deref(), cols),
        };
        let lines = compose_frame(content, status, cols, rows);
        terminal::emit_render_lines(output, &lines, &snapshot.palette, false)?;
        output.flush()?;

        let event = input.read_event()?;

        if let Some(active) = selection.as_mut() {
            let step = select_step(active, &grid, &mut pending_char, event);
            match step {
                SelectStep::Continue => {}
                SelectStep::Cancel => return Ok(PickerOutcome::Cancelled),
                SelectStep::BackToSearch => {
                    selection = None;
                    pending_char = None;
                }
                SelectStep::Yank => {
                    // `y` before a selection exists is inert, the way it is in vim's normal mode.
                    if let Some(text) = active.text(&grid) {
                        copy_selected_text(clipboard, &text)?;
                        if snapshot.flash_exit_on_yank {
                            return Ok(PickerOutcome::Copied { text });
                        }
                        // Stay open for the next grab: back to an empty search, with a note on
                        // the status row so the copy is visibly acknowledged.
                        notice = Some(format!(
                            "copied · {}",
                            clip_to_width(text.lines().next().unwrap_or(""), 32)
                        ));
                        query.clear();
                        selection = None;
                        pending_char = None;
                    }
                }
            }
            continue;
        }

        match event {
            PickerInputEvent::Escape | PickerInputEvent::CtrlC => {
                return Ok(PickerOutcome::Cancelled)
            }
            PickerInputEvent::Backspace => {
                notice = None;
                query.pop();
            }
            PickerInputEvent::Enter => {
                if let Some(first) = labels.first() {
                    selection = cursor_at(&viewport, &first.candidate);
                }
            }
            PickerInputEvent::Char(ch) => {
                notice = None;
                match labels.iter().find(|label| label.label == ch) {
                    Some(hit) => selection = cursor_at(&viewport, &hit.candidate),
                    // A label character can never also extend a match, so anything unlabeled is a
                    // search keystroke.
                    None => query.push(ch),
                }
            }
            PickerInputEvent::Other => continue,
        }
    }
}

enum SelectStep {
    Continue,
    Cancel,
    BackToSearch,
    Yank,
}

/// A `t`/`f` waiting for its target character.
#[derive(Clone, Copy)]
enum CharMotion {
    Till,
    Find,
}

/// Lands a bare cursor on the first cell of a match, selecting nothing.
///
/// The search only says where to look; what to take is the user's call, so the picker hands over a
/// cursor and waits for `v`.
fn cursor_at(viewport: &VisibleViewport, hit: &FlashCandidate) -> Option<Selection> {
    let (start, _end) = token_cell_bounds(viewport, hit.line, hit.start, hit.end)?;
    Some(Selection::new(Pos {
        row: start.0,
        col: start.1,
    }))
}

fn select_step(
    selection: &mut Selection,
    grid: &Grid,
    pending: &mut Option<CharMotion>,
    event: PickerInputEvent,
) -> SelectStep {
    // A pending t/f consumes the next character as its target, vim-style. Anything that is not a
    // character abandons the pending motion and is handled normally below.
    if let Some(kind) = pending.take() {
        if let PickerInputEvent::Char(target) = event {
            let motion = match kind {
                CharMotion::Till => Motion::TillChar(target),
                CharMotion::Find => Motion::FindChar(target),
            };
            selection.apply(motion, grid);
            return SelectStep::Continue;
        }
    }

    let motion = match event {
        // Escape means the same thing in every mode: leave. Unwinding one level at a time read
        // as an undo, which is not what the key is for.
        PickerInputEvent::Escape | PickerInputEvent::CtrlC => return SelectStep::Cancel,
        PickerInputEvent::Enter => return SelectStep::Yank,
        // Backspace is the un-type key: a keystroke eaten as a label jump lands here one key
        // later, and this hands back the search with the query intact.
        PickerInputEvent::Backspace => return SelectStep::BackToSearch,
        PickerInputEvent::Other => return SelectStep::Continue,
        PickerInputEvent::Char('y') => return SelectStep::Yank,
        PickerInputEvent::Char('v') => {
            selection.begin(false);
            return SelectStep::Continue;
        }
        PickerInputEvent::Char('V') => {
            selection.begin(true);
            return SelectStep::Continue;
        }
        PickerInputEvent::Char('t') => {
            *pending = Some(CharMotion::Till);
            return SelectStep::Continue;
        }
        PickerInputEvent::Char('f') => {
            *pending = Some(CharMotion::Find);
            return SelectStep::Continue;
        }
        PickerInputEvent::Char('h') => Motion::Left,
        PickerInputEvent::Char('j') => Motion::Down,
        PickerInputEvent::Char('k') => Motion::Up,
        PickerInputEvent::Char('l') => Motion::Right,
        PickerInputEvent::Char('w') => Motion::WordForward,
        PickerInputEvent::Char('b') => Motion::WordBack,
        PickerInputEvent::Char('e') => Motion::WordEnd,
        PickerInputEvent::Char('0') => Motion::LineStart,
        PickerInputEvent::Char('$') => Motion::LineEnd,
        PickerInputEvent::Char('o') => Motion::SwapEnds,
        PickerInputEvent::Char(_) => return SelectStep::Continue,
    };
    selection.apply(motion, grid);
    SelectStep::Continue
}

/// Lays the frame out against the live terminal: every row is clipped or padded to exactly
/// `cols`, the body is padded or trimmed to fill all but the last of `rows`, and the status row
/// is pinned to the bottom. Exact coverage is what lets frames overwrite without clearing.
fn compose_frame(
    content: Vec<RenderLine>,
    status: RenderLine,
    cols: usize,
    rows: usize,
) -> Vec<RenderLine> {
    let body = rows.saturating_sub(1);
    let mut lines: Vec<RenderLine> = content
        .into_iter()
        .take(body)
        .map(|line| fit_row(line, cols))
        .collect();
    while lines.len() < body {
        lines.push(blank_row(cols));
    }
    lines.push(fit_row(status, cols));
    lines
}

/// Re-fits an already-rendered row to the live column count, clipping or padding as needed.
fn fit_row(line: RenderLine, cols: usize) -> RenderLine {
    fill_row(
        line.spans
            .into_iter()
            .map(|span| (span.text, span.style))
            .collect(),
        cols,
    )
}

fn blank_row(width: usize) -> RenderLine {
    RenderLine {
        spans: vec![RenderSpan {
            text: " ".repeat(width),
            style: RenderStyle::Unmatched,
        }],
    }
}

fn legend_line(selection: &Selection, query: &str, width: usize) -> RenderLine {
    let keys = if selection.active() {
        let mode = if selection.linewise { "line" } else { "char" };
        format!("  select {mode} · hjkl/wbe/0$ · o/t/f · y yank · bksp search · esc exit")
    } else {
        "  cursor · hjkl/wbe/0$ · v/V select · t/f · bksp search · esc exit".to_string()
    };
    status_row(query, &keys, width)
}

fn prompt_line(
    labels: &[FlashLabel],
    query: &str,
    notice: Option<&str>,
    width: usize,
) -> RenderLine {
    let status = if query.is_empty() {
        notice.unwrap_or("type to search").to_string()
    } else if labels.is_empty() {
        "no match".to_string()
    } else {
        format!("{} match(es)", labels.len())
    };
    status_row(query, &format!("  {status}"), width)
}

/// The bottom row: the flash chip, whatever has been typed, then mode-specific text.
///
/// The query shows in every mode on purpose. A keystroke that happens to be a live label is
/// consumed as a selection rather than appended, and hiding the query behind a mode legend left no
/// way to see that the search had stopped growing.
fn status_row(query: &str, trailer: &str, width: usize) -> RenderLine {
    fill_row(
        vec![
            (" flash ".to_string(), RenderStyle::Hint),
            (format!(" {query}"), RenderStyle::Match),
            (trailer.to_string(), RenderStyle::Unmatched),
        ],
        width,
    )
}

/// Builds a status row, padding it out so it repaints the whole underlying line.
///
/// Overflow truncates the offending span rather than dropping it, so a long query degrades into a
/// clipped query instead of an empty bar.
fn fill_row(parts: Vec<(String, RenderStyle)>, width: usize) -> RenderLine {
    let mut used = 0;
    let mut kept = Vec::new();
    for (text, style) in parts {
        if used >= width {
            break;
        }
        let text = clip_to_width(&text, width - used);
        used += crate::hints::display_width(&text);
        kept.push(RenderSpan { text, style });
    }
    if used < width {
        kept.push(RenderSpan {
            text: " ".repeat(width - used),
            style: RenderStyle::Unmatched,
        });
    }
    RenderLine { spans: kept }
}

/// Longest prefix of `text` that fits in `width` display columns, never splitting a character.
fn clip_to_width(text: &str, width: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for ch in text.chars() {
        let ch_width = crate::hints::display_width(&ch.to_string());
        if used + ch_width > width {
            break;
        }
        used += ch_width;
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::{ClipboardError, CopySuccess};
    use crate::model::{
        PaneId, PaneTextCaptureMode, PickerAction, PickerReturnContext, SourcePaneSnapshot,
        StylePalette,
    };
    use anyhow::anyhow;
    use std::cell::RefCell;

    struct FakeInput {
        events: Vec<PickerInputEvent>,
    }

    impl FakeInput {
        fn new(events: Vec<PickerInputEvent>) -> Self {
            Self {
                events: events.into_iter().rev().collect(),
            }
        }
    }

    impl InputSource for FakeInput {
        fn read_event(&mut self) -> Result<PickerInputEvent> {
            self.events
                .pop()
                .ok_or_else(|| anyhow!("fake input exhausted"))
        }
    }

    #[derive(Default)]
    struct FakeClipboard {
        copied: RefCell<Vec<String>>,
    }

    impl Clipboard for FakeClipboard {
        fn copy(&self, text: &str) -> std::result::Result<CopySuccess, ClipboardError> {
            self.copied.borrow_mut().push(text.to_string());
            Ok(CopySuccess {
                tool: "fake".to_string(),
            })
        }
    }

    fn snapshot(lines: Vec<&str>, width: u16, height: u16) -> PickerSnapshot {
        PickerSnapshot {
            source: SourcePaneSnapshot {
                target_pane_id: PaneId::new("p1"),
                source_tab_id: "t1".to_string(),
                workspace_id: "w1".to_string(),
                source_panes: Vec::new(),
                target_content_width: width,
                target_content_height: height,
                logical_lines: lines.into_iter().map(str::to_string).collect(),
                visible_viewport: None,
                capture_mode: PaneTextCaptureMode::ExactVisibleUnwrapped,
            },
            session: PickerReturnContext {
                return_tab_id: "t1".to_string(),
                return_pane_id: PaneId::new("p1"),
                zoom_picker: false,
            },
            action: PickerAction::Flash,
            custom_patterns: Vec::new(),
            flash_exit_on_yank: true,
            palette: StylePalette::default(),
        }
    }

    fn run(events: Vec<PickerInputEvent>, lines: Vec<&str>) -> (PickerOutcome, Vec<String>) {
        let clipboard = FakeClipboard::default();
        let mut input = FakeInput::new(events);
        let mut output = Vec::new();
        let outcome =
            run_flash_with(&snapshot(lines, 80, 4), &mut input, &clipboard, &mut output).unwrap();
        let copied = clipboard.copied.borrow().clone();
        (outcome, copied)
    }

    #[test]
    fn the_label_only_places_a_cursor_and_the_user_picks_the_text() {
        // After "ex" the sole match continues with 'a', so 'a' is withheld from the alphabet and
        // the first offered label is 's'. The cursor lands on the hit itself; `v e` takes exactly
        // the word under it — vim word classes stop at the URL's punctuation, and nothing is
        // widened for the user.
        let (outcome, copied) = run(
            vec![
                PickerInputEvent::Char('e'),
                PickerInputEvent::Char('x'),
                PickerInputEvent::Char('s'),
                PickerInputEvent::Char('v'),
                PickerInputEvent::Char('e'),
                PickerInputEvent::Char('y'),
            ],
            vec!["visit https://example.com/path now"],
        );

        assert_eq!(
            outcome,
            PickerOutcome::Copied {
                text: "example".to_string()
            }
        );
        assert_eq!(copied, vec!["example".to_string()]);
    }

    #[test]
    fn till_grabs_a_whole_url_from_the_select_phase() {
        // t<space> is the vim way to take everything up to the next gap in one motion.
        let (outcome, _) = run(
            vec![
                PickerInputEvent::Char('h'),
                PickerInputEvent::Char('t'),
                PickerInputEvent::Enter,
                PickerInputEvent::Char('v'),
                PickerInputEvent::Char('t'),
                PickerInputEvent::Char(' '),
                PickerInputEvent::Char('y'),
            ],
            vec!["visit https://example.com/path now"],
        );

        assert_eq!(
            outcome,
            PickerOutcome::Copied {
                text: "https://example.com/path".to_string()
            }
        );
    }

    #[test]
    fn backspace_in_cursor_mode_returns_to_search_with_the_query_intact() {
        // A keystroke eaten as a label is recovered with one backspace: the query "ca" is still
        // there, and the search continues where it left off.
        let (outcome, _) = run(
            vec![
                PickerInputEvent::Char('c'),
                PickerInputEvent::Char('a'),
                PickerInputEvent::Enter,
                PickerInputEvent::Backspace,
                PickerInputEvent::Enter,
                PickerInputEvent::Char('v'),
                PickerInputEvent::Char('e'),
                PickerInputEvent::Char('y'),
            ],
            vec!["run cargo test"],
        );

        assert_eq!(
            outcome,
            PickerOutcome::Copied {
                text: "cargo".to_string()
            }
        );
    }

    #[test]
    fn yank_can_stay_open_for_multiple_grabs() {
        let mut snapshot = snapshot(vec!["run cargo test"], 80, 4);
        snapshot.flash_exit_on_yank = false;
        let clipboard = FakeClipboard::default();
        let mut input = FakeInput::new(vec![
            // First grab: "ca" → cursor on "cargo", v e y.
            PickerInputEvent::Char('c'),
            PickerInputEvent::Char('a'),
            PickerInputEvent::Enter,
            PickerInputEvent::Char('v'),
            PickerInputEvent::Char('e'),
            PickerInputEvent::Char('y'),
            // Back in an empty search: grab "test" the same way, then leave.
            PickerInputEvent::Char('t'),
            PickerInputEvent::Char('e'),
            PickerInputEvent::Enter,
            PickerInputEvent::Char('v'),
            PickerInputEvent::Char('e'),
            PickerInputEvent::Char('y'),
            PickerInputEvent::Escape,
        ]);
        let mut output = Vec::new();

        let outcome = run_flash_with(&snapshot, &mut input, &clipboard, &mut output).unwrap();

        assert_eq!(outcome, PickerOutcome::Cancelled);
        assert_eq!(
            clipboard.copied.borrow().as_slice(),
            &["cargo".to_string(), "test".to_string()]
        );
        // The copy is acknowledged on the status row while the picker stays open.
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("copied · cargo"));
    }

    #[test]
    fn the_status_row_is_pinned_to_the_live_bottom_row() {
        let status = fill_row(vec![("S".to_string(), RenderStyle::Hint)], 4);
        let content = vec![blank_row(4); 3];

        let taller = compose_frame(content.clone(), status.clone(), 4, 6);
        assert_eq!(taller.len(), 6);
        assert_eq!(taller[5].spans[0].text, "S");

        let shorter = compose_frame(content, status, 4, 2);
        assert_eq!(shorter.len(), 2);
        assert_eq!(shorter[1].spans[0].text, "S");
    }

    #[test]
    fn frames_are_clipped_and_padded_to_the_live_column_count() {
        let wide = fill_row(vec![("abcdef".to_string(), RenderStyle::Match)], 6);
        let narrow = fill_row(vec![("x".to_string(), RenderStyle::Hint)], 1);
        let status = fill_row(vec![("S".to_string(), RenderStyle::Hint)], 1);

        // Live pane is 4 columns: the wide row must clip, the narrow one must pad, or an
        // uncleared frame would wrap into the next row / leave stale cells at the right edge.
        let lines = compose_frame(vec![wide, narrow], status, 4, 3);
        for line in &lines {
            let rendered: String = line.spans.iter().map(|span| span.text.as_str()).collect();
            assert_eq!(crate::hints::display_width(&rendered), 4);
        }
        let first: String = lines[0].spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(first, "abcd");
    }

    #[test]
    fn backspace_widens_the_search_again() {
        // "ex" matches only the URL; backspacing to "e" also matches "date".
        let (outcome, _) = run(
            vec![
                PickerInputEvent::Char('e'),
                PickerInputEvent::Char('x'),
                PickerInputEvent::Backspace,
                PickerInputEvent::Enter,
                PickerInputEvent::Char('b'),
                PickerInputEvent::Char('v'),
                PickerInputEvent::Char('e'),
                PickerInputEvent::Char('y'),
            ],
            vec!["date https://example.com"],
        );

        assert_eq!(
            outcome,
            PickerOutcome::Copied {
                text: "date".to_string()
            }
        );
    }

    #[test]
    fn enter_takes_the_first_match_and_widens_nothing() {
        // The first 'p' is inside "alpha", not the "purple" on the next row, and `v y` yanks
        // exactly that one character rather than the word around it.
        let (outcome, _) = run(
            vec![
                PickerInputEvent::Char('p'),
                PickerInputEvent::Enter,
                PickerInputEvent::Char('v'),
                PickerInputEvent::Char('y'),
            ],
            vec!["alpha beta", "purple"],
        );

        assert_eq!(
            outcome,
            PickerOutcome::Copied {
                text: "p".to_string()
            }
        );
    }

    #[test]
    fn escape_cancels_without_copying() {
        let (outcome, copied) = run(
            vec![PickerInputEvent::Char('a'), PickerInputEvent::Escape],
            vec!["alpha beta"],
        );

        assert_eq!(outcome, PickerOutcome::Cancelled);
        assert!(copied.is_empty());
    }

    #[test]
    fn a_character_that_extends_a_match_searches_instead_of_selecting() {
        // After "al", 'p' continues "alpha" so it must never be handed out as a label.
        let (outcome, _) = run(
            vec![
                PickerInputEvent::Char('a'),
                PickerInputEvent::Char('l'),
                PickerInputEvent::Char('p'),
                PickerInputEvent::Enter,
                PickerInputEvent::Char('v'),
                PickerInputEvent::Char('e'),
                PickerInputEvent::Char('y'),
            ],
            vec!["alpha always"],
        );

        assert_eq!(
            outcome,
            PickerOutcome::Copied {
                text: "alpha".to_string()
            }
        );
    }

    #[test]
    fn motions_extend_the_selection_past_the_token() {
        // Land on "cargo", start a selection there, then walk to the end of "test".
        let (outcome, _) = run(
            vec![
                PickerInputEvent::Char('c'),
                PickerInputEvent::Char('a'),
                PickerInputEvent::Enter,
                PickerInputEvent::Char('v'),
                PickerInputEvent::Char('w'),
                PickerInputEvent::Char('e'),
                PickerInputEvent::Char('y'),
            ],
            vec!["run cargo test now"],
        );

        assert_eq!(
            outcome,
            PickerOutcome::Copied {
                text: "cargo test".to_string()
            }
        );
    }

    #[test]
    fn linewise_yanks_the_whole_row() {
        let (outcome, _) = run(
            vec![
                PickerInputEvent::Char('c'),
                PickerInputEvent::Char('a'),
                PickerInputEvent::Enter,
                PickerInputEvent::Char('V'),
                PickerInputEvent::Char('y'),
            ],
            vec!["run cargo test now"],
        );

        assert_eq!(
            outcome,
            PickerOutcome::Copied {
                text: "run cargo test now".to_string()
            }
        );
    }

    #[test]
    fn escape_exits_from_the_cursor_mode() {
        let (outcome, copied) = run(
            vec![
                PickerInputEvent::Char('c'),
                PickerInputEvent::Char('a'),
                PickerInputEvent::Enter,
                PickerInputEvent::Escape,
            ],
            vec!["arc cargo test"],
        );

        assert_eq!(outcome, PickerOutcome::Cancelled);
        assert!(copied.is_empty());
    }

    #[test]
    fn escape_exits_from_an_active_selection_too() {
        let (outcome, copied) = run(
            vec![
                PickerInputEvent::Char('c'),
                PickerInputEvent::Char('a'),
                PickerInputEvent::Enter,
                PickerInputEvent::Char('v'),
                PickerInputEvent::Char('w'),
                PickerInputEvent::Escape,
            ],
            vec!["arc cargo test"],
        );

        assert_eq!(outcome, PickerOutcome::Cancelled);
        assert!(copied.is_empty());
    }

    #[test]
    fn ctrl_c_still_cancels_from_inside_a_selection() {
        let (outcome, copied) = run(
            vec![
                PickerInputEvent::Char('c'),
                PickerInputEvent::Char('a'),
                PickerInputEvent::Enter,
                PickerInputEvent::CtrlC,
            ],
            vec!["run cargo test"],
        );

        assert_eq!(outcome, PickerOutcome::Cancelled);
        assert!(copied.is_empty());
    }

    #[test]
    fn selection_view_shows_its_own_legend() {
        let clipboard = FakeClipboard::default();
        let mut input = FakeInput::new(vec![
            PickerInputEvent::Char('c'),
            PickerInputEvent::Char('a'),
            PickerInputEvent::Enter,
            PickerInputEvent::CtrlC,
        ]);
        let mut output = Vec::new();
        run_flash_with(
            &snapshot(vec!["run cargo test"], 80, 3),
            &mut input,
            &clipboard,
            &mut output,
        )
        .unwrap();

        let rendered = String::from_utf8(output).unwrap();
        assert!(rendered.contains("cursor"));
        assert!(rendered.contains("v/V select"));
        assert!(rendered.contains("bksp search"));
        assert!(rendered.contains("esc exit"));
        // The query must survive the mode switch; losing it hid the fact that a keystroke had
        // been eaten as a label instead of extending the search.
        assert!(rendered.contains("flash"));
        assert!(rendered.contains("ca"));
    }

    #[test]
    fn y_before_a_selection_exists_does_nothing() {
        let (outcome, copied) = run(
            vec![
                PickerInputEvent::Char('c'),
                PickerInputEvent::Char('a'),
                PickerInputEvent::Enter,
                // No `v` yet, so this `y` must not copy anything.
                PickerInputEvent::Char('y'),
                PickerInputEvent::CtrlC,
            ],
            vec!["run cargo test"],
        );

        assert_eq!(outcome, PickerOutcome::Cancelled);
        assert!(copied.is_empty());
    }

    #[test]
    fn pressing_v_again_drops_the_selection_and_keeps_the_cursor() {
        let (outcome, _) = run(
            vec![
                PickerInputEvent::Char('c'),
                PickerInputEvent::Char('a'),
                PickerInputEvent::Enter,
                PickerInputEvent::Char('v'),
                PickerInputEvent::Char('w'),
                // `v` is the only way back now that escape always exits.
                PickerInputEvent::Char('v'),
                PickerInputEvent::Char('v'),
                PickerInputEvent::Char('e'),
                PickerInputEvent::Char('y'),
            ],
            vec!["run cargo test"],
        );

        // The cursor was left on "test" after `w`, so reselecting yanks that, not "cargo".
        assert_eq!(
            outcome,
            PickerOutcome::Copied {
                text: "test".to_string()
            }
        );
    }

    #[test]
    fn a_long_query_clips_instead_of_blanking_the_row() {
        let row = fill_row(
            vec![
                (" flash ".to_string(), RenderStyle::Hint),
                ("x".repeat(200), RenderStyle::Match),
                ("  trailing".to_string(), RenderStyle::Unmatched),
            ],
            20,
        );

        let rendered: String = row.spans.iter().map(|span| span.text.as_str()).collect();
        assert_eq!(crate::hints::display_width(&rendered), 20);
        assert!(rendered.starts_with(" flash "));
        assert!(rendered.contains('x'), "the query must survive truncation");
    }

    #[test]
    fn clipping_never_splits_a_wide_character() {
        // Width 3 fits one wide char plus one column; the second must not be halved.
        assert_eq!(clip_to_width("界界", 3), "界");
        assert_eq!(clip_to_width("界界", 4), "界界");
    }

    #[test]
    fn prompt_line_replaces_the_bottom_row() {
        let clipboard = FakeClipboard::default();
        let mut input = FakeInput::new(vec![PickerInputEvent::Escape]);
        let mut output = Vec::new();
        run_flash_with(
            &snapshot(vec!["alpha"], 40, 3),
            &mut input,
            &clipboard,
            &mut output,
        )
        .unwrap();

        let rendered = String::from_utf8(output).unwrap();
        assert!(rendered.contains("flash"));
        assert!(rendered.contains("type to search"));
    }
}
