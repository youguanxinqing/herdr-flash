//! Cursor and selection over the mirrored viewport grid.
//!
//! Herdr's own copy mode is a key action inside Herdr's input dispatcher, not something the socket
//! API exposes, so a plugin cannot enter it or place its cursor. The picker already owns a
//! full-screen mirror of the pane, so the selection step happens here instead.
//!
//! Positions are `(row, display column)` against the visible grid, matching how the renderer lays
//! cells out. Working in columns rather than byte offsets is what keeps wide CJK cells aligned.

use crate::hints::HINT_ALPHABET;
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub row: usize,
    pub col: usize,
}

/// One visible character and the column it starts at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GridChar {
    col: usize,
    ch: char,
}

/// The mirrored viewport as addressable columns, mirroring the renderer's cell layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid {
    rows: Vec<Vec<GridChar>>,
}

impl Grid {
    pub fn new(rows: &[String]) -> Self {
        Self {
            rows: rows.iter().map(|row| row_chars(row)).collect(),
        }
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Column of the last character on `row`, or 0 when the row is blank.
    fn last_col(&self, row: usize) -> usize {
        self.rows
            .get(row)
            .and_then(|chars| chars.last())
            .map_or(0, |last| last.col)
    }

    /// Clamps a position onto an existing row and column.
    fn clamp(&self, pos: Pos) -> Pos {
        let row = pos.row.min(self.row_count().saturating_sub(1));
        Pos {
            row,
            col: pos.col.min(self.last_col(row)),
        }
    }

    fn chars(&self, row: usize) -> &[GridChar] {
        self.rows.get(row).map_or(&[], Vec::as_slice)
    }

    /// The cell just before `pos` on its row; a row-leading `pos` stays put, since there is no
    /// earlier cell for a `t` landing to occupy.
    fn cell_before(&self, pos: Pos) -> Pos {
        Pos {
            col: prev_col(self.chars(pos.row), pos.col).unwrap_or(pos.col),
            ..pos
        }
    }

    /// The cell just after `pos` on its row; a row-ending `pos` stays put, mirroring
    /// [`Grid::cell_before`] for backward `T` landings.
    fn cell_after(&self, pos: Pos) -> Pos {
        Pos {
            col: next_col(self.chars(pos.row), pos.col).unwrap_or(pos.col),
            ..pos
        }
    }
}

/// One visible occurrence of a char-jump (`t`/`f`/`T`/`F`) target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharJumpTarget {
    /// The cell holding the target character; this is the cell that gets highlighted.
    pub hit: Pos,
    /// Where the cursor lands: the hit itself for `f`/`F`, the cell on the cursor's side of it
    /// for `t`/`T`.
    pub landing: Pos,
    /// The key that picks this target, or `None` past the alphabet — highlighted but unreachable.
    pub label: Option<char>,
}

/// Every occurrence of `target` strictly on the cursor's search side of `from`, nearest first —
/// reading order ahead of the cursor, reverse reading order behind it — so the first labels sit
/// on the closest hits.
///
/// vim's `t`/`f` stop at the current row; a label overlay is only worth its keystroke when it can
/// reach the rest of the pane, so candidates continue over the remaining rows. Matching is
/// case-exact like vim. The target character is withheld from the label alphabet: this mode
/// takes exactly one search character, and a label equal to it would read as typing a second one.
pub fn char_jump_targets(
    grid: &Grid,
    from: Pos,
    target: char,
    till: bool,
    backward: bool,
) -> Vec<CharJumpTarget> {
    let hits: Vec<Pos> = if backward {
        (0..=from.row)
            .rev()
            .flat_map(|row| {
                grid.chars(row)
                    .iter()
                    .rev()
                    .filter(move |grid_char| {
                        grid_char.ch == target && !(row == from.row && grid_char.col >= from.col)
                    })
                    .map(move |grid_char| Pos {
                        row,
                        col: grid_char.col,
                    })
            })
            .collect()
    } else {
        (from.row..grid.row_count())
            .flat_map(|row| {
                grid.chars(row)
                    .iter()
                    .filter(move |grid_char| {
                        grid_char.ch == target && !(row == from.row && grid_char.col <= from.col)
                    })
                    .map(move |grid_char| Pos {
                        row,
                        col: grid_char.col,
                    })
            })
            .collect()
    };

    let mut targets: Vec<CharJumpTarget> = hits
        .into_iter()
        .map(|hit| CharJumpTarget {
            hit,
            landing: match (till, backward) {
                (true, false) => grid.cell_before(hit),
                (true, true) => grid.cell_after(hit),
                (false, _) => hit,
            },
            label: None,
        })
        .collect();
    label_page(&mut targets, target, 0);
    targets
}

/// Moves the labels one alphabet-page further into the targets, wrapping past the last page.
///
/// A single character can hit more spots than the alphabet has keys. Two-key labels would break
/// this mode's one-keystroke contract, so instead the same alphabet pages through the targets;
/// hits outside the current page keep their highlight and wait for their page to come around.
pub fn advance_char_jump_page(targets: &mut [CharJumpTarget], target: char) {
    let page_len = HINT_ALPHABET
        .chars()
        .filter(|ch| *ch != target)
        .count()
        .max(1);
    let start = targets
        .iter()
        .position(|jump| jump.label.is_some())
        .map_or(0, |first| first + page_len);
    let start = if start >= targets.len() { 0 } else { start };
    label_page(targets, target, start);
}

/// Assigns the label alphabet to the targets from `start` on, clearing every earlier label.
fn label_page(targets: &mut [CharJumpTarget], target: char, start: usize) {
    let mut labels = HINT_ALPHABET.chars().filter(|ch| *ch != target);
    for (index, jump) in targets.iter_mut().enumerate() {
        jump.label = if index >= start { labels.next() } else { None };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBack,
    WordEnd,
    LineStart,
    LineEnd,
    SwapEnds,
}

/// A movable cursor that may or may not be dragging a selection behind it.
///
/// The picker lands as a bare cursor: `anchor` stays `None` until the user starts a selection, so
/// nothing is chosen on their behalf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub cursor: Pos,
    pub anchor: Option<Pos>,
    pub linewise: bool,
}

impl Selection {
    /// A bare cursor at `cursor` with nothing selected.
    pub fn new(cursor: Pos) -> Self {
        Self {
            cursor,
            anchor: None,
            linewise: false,
        }
    }

    pub fn active(&self) -> bool {
        self.anchor.is_some()
    }

    /// Anchors a selection at the cursor, or drops it when the same mode is asked for twice.
    pub fn begin(&mut self, linewise: bool) {
        if self.active() && self.linewise == linewise {
            self.cancel();
            return;
        }
        self.anchor = Some(self.cursor);
        self.linewise = linewise;
    }

    pub fn cancel(&mut self) {
        self.anchor = None;
        self.linewise = false;
    }

    /// Normalized `(start, end)` in reading order, or `None` while only a cursor exists.
    pub fn range(&self) -> Option<(Pos, Pos)> {
        let anchor = self.anchor?;
        Some(if anchor <= self.cursor {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        })
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        let Some((start, end)) = self.range() else {
            return false;
        };
        if row < start.row || row > end.row {
            return false;
        }
        if self.linewise {
            return true;
        }
        if start.row == end.row {
            return col >= start.col && col <= end.col;
        }
        if row == start.row {
            return col >= start.col;
        }
        if row == end.row {
            return col <= end.col;
        }
        true
    }

    pub fn apply(&mut self, motion: Motion, grid: &Grid) {
        if motion == Motion::SwapEnds {
            if let Some(anchor) = self.anchor {
                self.anchor = Some(self.cursor);
                self.cursor = anchor;
            }
            return;
        }
        self.cursor = grid.clamp(moved(self.cursor, motion, grid));
    }

    /// Selected text, one line per grid row with trailing padding removed, or `None` if nothing
    /// is selected yet.
    pub fn text(&self, grid: &Grid) -> Option<String> {
        let (start, end) = self.range()?;
        Some(
            (start.row..=end.row)
                .filter(|row| *row < grid.row_count())
                .map(|row| {
                    let chars = grid.chars(row);
                    let piece: String = chars
                        .iter()
                        .filter(|grid_char| self.contains(row, grid_char.col))
                        .map(|grid_char| grid_char.ch)
                        .collect();
                    piece.trim_end().to_string()
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}

fn moved(pos: Pos, motion: Motion, grid: &Grid) -> Pos {
    let chars = grid.chars(pos.row);
    match motion {
        Motion::Left => Pos {
            col: prev_col(chars, pos.col).unwrap_or(pos.col),
            ..pos
        },
        Motion::Right => Pos {
            col: next_col(chars, pos.col).unwrap_or(pos.col),
            ..pos
        },
        Motion::Up => Pos {
            row: pos.row.saturating_sub(1),
            ..pos
        },
        Motion::Down => Pos {
            row: pos.row + 1,
            ..pos
        },
        Motion::LineStart => Pos { col: 0, ..pos },
        Motion::LineEnd => Pos {
            col: chars.last().map_or(0, |last| last.col),
            ..pos
        },
        Motion::WordForward => Pos {
            col: word_forward(chars, pos.col),
            ..pos
        },
        Motion::WordBack => Pos {
            col: word_back(chars, pos.col),
            ..pos
        },
        Motion::WordEnd => Pos {
            col: word_end(chars, pos.col),
            ..pos
        },
        Motion::SwapEnds => pos,
    }
}

/// Vim-style word classes: whitespace, alphanumeric words, CJK runs, punctuation.
///
/// Whitespace-run words made an entire Chinese clause one "word" — there are no spaces to split
/// on — so w/b/e jumped across whole sentences. Classifying like vim's `mb_get_class` keeps a Han
/// run, a punctuation run, and an ASCII word apart. CJK is checked before `is_alphanumeric`,
/// which is true for ideographs too.
fn char_class(ch: char) -> u8 {
    if ch.is_whitespace() {
        0
    } else if is_cjk(ch) {
        2
    } else if ch.is_alphanumeric() || ch == '_' {
        1
    } else {
        3
    }
}

/// Han, kana, and hangul ranges; fullwidth punctuation deliberately falls through to class 3.
fn is_cjk(ch: char) -> bool {
    matches!(
        u32::from(ch),
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0x20000..=0x2FA1F
    )
}

fn index_of(chars: &[GridChar], col: usize) -> usize {
    chars
        .iter()
        .position(|grid_char| grid_char.col >= col)
        .unwrap_or(chars.len().saturating_sub(1))
}

fn next_col(chars: &[GridChar], col: usize) -> Option<usize> {
    chars.get(index_of(chars, col) + 1).map(|next| next.col)
}

fn prev_col(chars: &[GridChar], col: usize) -> Option<usize> {
    let index = index_of(chars, col);
    index
        .checked_sub(1)
        .and_then(|prev| chars.get(prev))
        .map(|prev| prev.col)
}

/// Start of the next same-class run, or the row's last column.
fn word_forward(chars: &[GridChar], col: usize) -> usize {
    if chars.is_empty() {
        return 0;
    }
    let mut index = index_of(chars, col);
    let class = char_class(chars[index].ch);
    if class != 0 {
        while index < chars.len() && char_class(chars[index].ch) == class {
            index += 1;
        }
    }
    while index < chars.len() && char_class(chars[index].ch) == 0 {
        index += 1;
    }
    chars
        .get(index)
        .map_or_else(|| chars.last().map_or(0, |last| last.col), |at| at.col)
}

/// Start of the current run, or of the previous one when already at a start.
fn word_back(chars: &[GridChar], col: usize) -> usize {
    if chars.is_empty() {
        return 0;
    }
    let mut index = index_of(chars, col);
    if index == 0 {
        return chars[0].col;
    }
    index -= 1;
    while index > 0 && char_class(chars[index].ch) == 0 {
        index -= 1;
    }
    let class = char_class(chars[index].ch);
    if class == 0 {
        return chars[index].col;
    }
    while index > 0 && char_class(chars[index - 1].ch) == class {
        index -= 1;
    }
    chars[index].col
}

/// Last column of the current run, or of the next one when already at its end.
fn word_end(chars: &[GridChar], col: usize) -> usize {
    if chars.is_empty() {
        return 0;
    }
    let mut index = index_of(chars, col);
    if index + 1 >= chars.len() {
        return chars[index].col;
    }
    index += 1;
    while index < chars.len() && char_class(chars[index].ch) == 0 {
        index += 1;
    }
    if index >= chars.len() {
        return chars.last().map_or(0, |last| last.col);
    }
    let class = char_class(chars[index].ch);
    while index + 1 < chars.len() && char_class(chars[index + 1].ch) == class {
        index += 1;
    }
    chars[index].col
}

/// Splits a row into characters and the columns they occupy, skipping zero-width marks.
fn row_chars(row: &str) -> Vec<GridChar> {
    let mut chars = Vec::new();
    let mut col = 0;
    for ch in row.chars() {
        let width = ch.width().unwrap_or(0);
        if width == 0 {
            continue;
        }
        chars.push(GridChar { col, ch });
        col += width;
    }
    chars
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(rows: &[&str]) -> Grid {
        Grid::new(&rows.iter().map(|row| row.to_string()).collect::<Vec<_>>())
    }

    fn at(row: usize, col: usize) -> Pos {
        Pos { row, col }
    }

    /// Cursor at `col`, selection started there, then the given motions applied.
    fn selecting(grid: &Grid, col: usize, motions: &[Motion]) -> Selection {
        let mut selection = Selection::new(at(0, col));
        selection.begin(false);
        for motion in motions {
            selection.apply(*motion, grid);
        }
        selection
    }

    #[test]
    fn a_fresh_cursor_selects_nothing() {
        let grid = grid(&["run cargo test"]);
        let selection = Selection::new(at(0, 4));

        assert!(!selection.active());
        assert_eq!(selection.text(&grid), None);
        assert!(!selection.contains(0, 4));
    }

    #[test]
    fn moving_before_selecting_leaves_nothing_selected() {
        let grid = grid(&["run cargo test"]);
        let mut selection = Selection::new(at(0, 4));

        selection.apply(Motion::WordForward, &grid);
        assert_eq!(selection.cursor.col, 10);
        assert_eq!(selection.text(&grid), None);
    }

    #[test]
    fn v_anchors_at_the_cursor_and_motions_extend_from_there() {
        let grid = grid(&["run cargo test"]);
        let selection = selecting(&grid, 4, &[Motion::WordEnd]);

        assert_eq!(selection.text(&grid).unwrap(), "cargo");
    }

    #[test]
    fn v_again_drops_the_selection() {
        let grid = grid(&["run cargo test"]);
        let mut selection = selecting(&grid, 4, &[Motion::WordEnd]);

        selection.begin(false);
        assert!(!selection.active());
        assert_eq!(selection.text(&grid), None);
        // The cursor stays where it was rather than snapping back.
        assert_eq!(selection.cursor.col, 8);
    }

    #[test]
    fn switching_between_charwise_and_linewise_keeps_the_anchor() {
        let grid = grid(&["alpha beta"]);
        let mut selection = selecting(&grid, 6, &[]);

        selection.begin(true);
        assert!(selection.active());
        assert!(selection.linewise);
        assert_eq!(selection.text(&grid).unwrap(), "alpha beta");
    }

    #[test]
    fn wide_characters_advance_by_their_display_width() {
        let grid = grid(&["界x"]);
        let selection = selecting(&grid, 0, &[Motion::Right]);

        // The wide char occupies columns 0-1, so 'x' starts at column 2.
        assert_eq!(selection.cursor, at(0, 2));
        assert_eq!(selection.text(&grid).unwrap(), "界x");
    }

    #[test]
    fn selection_extends_across_rows_and_drops_row_padding() {
        let grid = grid(&["first row   ", "second row  "]);
        let mut selection = Selection::new(at(0, 6));
        selection.begin(false);
        selection.cursor = at(1, 5);

        assert_eq!(selection.text(&grid).unwrap(), "row\nsecond");
    }

    #[test]
    fn word_motions_step_between_whitespace_runs() {
        let grid = grid(&["alpha  beta gamma"]);
        let mut selection = Selection::new(at(0, 0));

        selection.apply(Motion::WordForward, &grid);
        assert_eq!(selection.cursor.col, 7);
        selection.apply(Motion::WordForward, &grid);
        assert_eq!(selection.cursor.col, 12);
        selection.apply(Motion::WordBack, &grid);
        assert_eq!(selection.cursor.col, 7);
    }

    #[test]
    fn word_end_lands_on_the_last_character_of_a_run() {
        let grid = grid(&["alpha beta"]);
        let mut selection = Selection::new(at(0, 0));

        selection.apply(Motion::WordEnd, &grid);
        assert_eq!(selection.cursor.col, 4);
        selection.apply(Motion::WordEnd, &grid);
        assert_eq!(selection.cursor.col, 9);
    }

    #[test]
    fn swapping_ends_moves_the_other_side_next() {
        let grid = grid(&["abcdef"]);
        let mut selection = Selection::new(at(0, 2));
        selection.begin(false);
        selection.cursor = at(0, 4);

        selection.apply(Motion::SwapEnds, &grid);
        assert_eq!(selection.cursor, at(0, 2));
        selection.apply(Motion::Left, &grid);
        assert_eq!(selection.text(&grid).unwrap(), "bcde");
    }

    #[test]
    fn swapping_ends_without_a_selection_does_nothing() {
        let grid = grid(&["abcdef"]);
        let mut selection = Selection::new(at(0, 2));

        selection.apply(Motion::SwapEnds, &grid);
        assert_eq!(selection.cursor, at(0, 2));
        assert!(!selection.active());
    }

    #[test]
    fn motions_clamp_at_the_grid_edges() {
        let grid = grid(&["ab", "cdef"]);
        let mut selection = Selection::new(at(0, 0));

        selection.apply(Motion::Up, &grid);
        assert_eq!(selection.cursor, at(0, 0));
        selection.apply(Motion::Left, &grid);
        assert_eq!(selection.cursor, at(0, 0));

        selection.apply(Motion::Down, &grid);
        selection.apply(Motion::LineEnd, &grid);
        assert_eq!(selection.cursor, at(1, 3));
        selection.apply(Motion::Down, &grid);
        // Row 1 is the last row; the cursor stays put instead of falling off.
        assert_eq!(selection.cursor, at(1, 3));
    }

    #[test]
    fn a_shorter_row_clamps_the_column_on_vertical_moves() {
        let grid = grid(&["long line here", "ab"]);
        let mut selection = Selection::new(at(0, 10));

        selection.apply(Motion::Down, &grid);
        assert_eq!(selection.cursor, at(1, 1));
    }

    #[test]
    fn word_motions_treat_cjk_runs_and_punctuation_as_words() {
        // 黄色 (cols 0-3) ， (col 4) label (cols 6-10) 一格 (col 12-)
        let grid = grid(&["黄色，label 一格"]);
        let mut selection = Selection::new(at(0, 0));

        selection.apply(Motion::WordForward, &grid);
        assert_eq!(selection.cursor.col, 4, "Han run ends at the punctuation");
        selection.apply(Motion::WordForward, &grid);
        assert_eq!(
            selection.cursor.col, 6,
            "punctuation run ends at the ASCII word"
        );
        selection.apply(Motion::WordForward, &grid);
        assert_eq!(
            selection.cursor.col, 12,
            "ASCII word ends at the next Han run"
        );
        selection.apply(Motion::WordBack, &grid);
        assert_eq!(selection.cursor.col, 6);
        selection.apply(Motion::WordEnd, &grid);
        assert_eq!(selection.cursor.col, 10, "end of \"label\"");
    }

    #[test]
    fn char_jump_targets_run_forward_across_rows_and_stay_case_exact() {
        let grid = grid(&["go now", "big gap"]);
        let targets = char_jump_targets(&grid, at(0, 0), 'g', false, false);

        // The 'g' under the cursor is skipped; later rows contribute in reading order.
        let hits: Vec<Pos> = targets.iter().map(|target| target.hit).collect();
        assert_eq!(hits, vec![at(1, 2), at(1, 4)]);
        // `f` lands on the hit itself.
        assert_eq!(targets[0].landing, at(1, 2));
        assert!(char_jump_targets(&grid, at(0, 0), 'G', false, false).is_empty());
    }

    #[test]
    fn on_the_cursor_row_only_hits_after_the_cursor_count() {
        let grid = grid(&["axa axa"]);
        let targets = char_jump_targets(&grid, at(0, 2), 'a', false, false);

        let hits: Vec<Pos> = targets.iter().map(|target| target.hit).collect();
        assert_eq!(hits, vec![at(0, 4), at(0, 6)]);
    }

    #[test]
    fn till_lands_on_the_wide_char_before_the_target() {
        let grid = grid(&["黄色x"]);
        let targets = char_jump_targets(&grid, at(0, 0), 'x', true, false);

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].landing, at(0, 2), "色 starts at column 2");
    }

    #[test]
    fn till_at_a_row_start_stays_on_the_hit() {
        let grid = grid(&["abc", "xyz"]);
        let targets = char_jump_targets(&grid, at(0, 0), 'x', true, false);

        // There is no cell before a row-leading hit, so the landing clamps onto it.
        assert_eq!(targets[0].landing, at(1, 0));
    }

    #[test]
    fn labels_skip_the_target_char_and_run_out_gracefully() {
        let row = "z".repeat(30);
        let grid = grid(&[row.as_str()]);
        let targets = char_jump_targets(&grid, at(0, 0), 'z', false, false);

        assert_eq!(targets.len(), 29);
        assert!(
            targets.iter().all(|target| target.label != Some('z')),
            "the target char must never double as a label"
        );
        assert_eq!(targets[0].label, Some('a'));
        assert!(
            targets.last().unwrap().label.is_none(),
            "past the alphabet, targets keep only the highlight"
        );
    }

    #[test]
    fn advancing_the_page_relabels_the_overflow_and_wraps_around() {
        // 29 hits against a 25-key page ('z' is withheld): the tail four are unreachable until
        // the page advances onto them, and a further advance wraps back to the nearest hits.
        let row = "z".repeat(30);
        let grid = grid(&[row.as_str()]);
        let mut targets = char_jump_targets(&grid, at(0, 0), 'z', false, false);

        advance_char_jump_page(&mut targets, 'z');
        assert!(targets[..25].iter().all(|jump| jump.label.is_none()));
        assert_eq!(targets[25].label, Some('a'));
        assert_eq!(targets[28].label, Some('f'));

        advance_char_jump_page(&mut targets, 'z');
        assert_eq!(targets[0].label, Some('a'));
        assert!(targets[25..].iter().all(|jump| jump.label.is_none()));
    }

    #[test]
    fn backward_targets_run_nearest_first_and_stop_before_the_cursor() {
        let grid = grid(&["big gap", "go now"]);
        let targets = char_jump_targets(&grid, at(1, 0), 'g', false, true);

        // The 'g' under the cursor is skipped; earlier hits arrive closest-first so the first
        // labels sit next to the cursor.
        let hits: Vec<Pos> = targets.iter().map(|target| target.hit).collect();
        assert_eq!(hits, vec![at(0, 4), at(0, 2)]);
        assert_eq!(targets[0].label, Some('a'));
    }

    #[test]
    fn backward_till_lands_just_after_the_hit() {
        let grid = grid(&["x黄色"]);
        let targets = char_jump_targets(&grid, at(0, 3), 'x', true, true);

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].landing, at(0, 1), "黄 starts at column 1");
    }

    #[test]
    fn backward_till_at_a_row_end_stays_on_the_hit() {
        let grid = grid(&["abx", "cde"]);
        let targets = char_jump_targets(&grid, at(1, 1), 'x', true, true);

        // There is no cell after a row-ending hit, so the landing clamps onto it.
        assert_eq!(targets[0].landing, at(0, 2));
    }

    #[test]
    fn blank_rows_do_not_panic() {
        let grid = grid(&["", "text"]);
        let mut selection = Selection::new(at(0, 0));
        selection.begin(false);

        selection.apply(Motion::WordForward, &grid);
        selection.apply(Motion::WordEnd, &grid);
        selection.apply(Motion::LineEnd, &grid);
        assert_eq!(selection.text(&grid).unwrap(), "");
    }
}
