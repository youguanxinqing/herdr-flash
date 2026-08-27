use crate::model::{RenderLine, RenderStyle, StylePalette};
use anyhow::Result;
use crossterm::{
    cursor::MoveTo,
    queue,
    style::{
        Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
    terminal::{BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate},
};
use std::io::Write;

/// Emits abstract picker render lines to a terminal writer using v1 styling.
///
/// Repaint frames never clear the screen, because a clear-then-repaint shows a blank frame
/// whenever the terminal composites between the two, and the DEC 2026 guards that were meant to
/// prevent that are best-effort — Herdr drops them under its render timeout. The caller owns
/// coverage: a repaint loop must paint every cell of the frame so overwriting in place is
/// complete; a one-shot emit that covers less passes `clear_first`, which rides the clear inside
/// the same buffered write as the content so a bare cleared screen can never be composited
/// alone. The frame is assembled in memory and handed over as one write for the same reason.
pub fn emit_render_lines(
    writer: &mut impl Write,
    lines: &[RenderLine],
    palette: &StylePalette,
    clear_first: bool,
) -> Result<()> {
    let mut frame = Vec::new();
    queue!(frame, BeginSynchronizedUpdate)?;
    if clear_first {
        queue!(frame, Clear(ClearType::All))?;
    }

    for (line_index, line) in lines.iter().enumerate() {
        queue!(frame, MoveTo(0, line_index as u16))?;
        for span in &line.spans {
            queue_style(&mut frame, span.style, palette)?;
            queue!(frame, Print(&span.text))?;
        }
    }

    // Cancel any wrap-pending state left by a full-width final row.
    queue!(
        frame,
        MoveTo(0, 0),
        ResetColor,
        SetAttribute(Attribute::Reset),
        EndSynchronizedUpdate
    )?;
    writer.write_all(&frame)?;
    Ok(())
}

fn queue_style(writer: &mut impl Write, style: RenderStyle, palette: &StylePalette) -> Result<()> {
    let spec = match style {
        RenderStyle::Unmatched => palette.unmatched,
        RenderStyle::Match => palette.matched,
        RenderStyle::Hint => palette.label,
        RenderStyle::Selection => palette.selection,
        RenderStyle::Cursor => palette.cursor,
    };
    // Every span opens with a reset. Without one a style inherits whatever background the
    // previous span set, and a lone label cell bleeds its background across the rest of the frame.
    queue!(writer, SetAttribute(Attribute::Reset))?;
    if let Some([r, g, b]) = spec.fg {
        queue!(writer, SetForegroundColor(Color::Rgb { r, g, b }))?;
    }
    if let Some([r, g, b]) = spec.bg {
        queue!(writer, SetBackgroundColor(Color::Rgb { r, g, b }))?;
    }
    if spec.bold {
        queue!(writer, SetAttribute(Attribute::Bold))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RenderSpan, RenderStyle, StylePalette};

    /// Pins crossterm's global color switch on so exact escape assertions hold even when the
    /// ambient environment sets NO_COLOR, which would otherwise suppress the color sequences.
    fn force_colors() {
        crossterm::style::force_color_output(true);
    }

    #[test]
    fn terminal_emission_writes_all_spans_without_clearing() {
        force_colors();
        let lines = vec![RenderLine {
            spans: vec![
                RenderSpan {
                    text: "open ".to_string(),
                    style: RenderStyle::Unmatched,
                },
                RenderSpan {
                    text: "a".to_string(),
                    style: RenderStyle::Hint,
                },
                RenderSpan {
                    text: "ttps://example.com".to_string(),
                    style: RenderStyle::Match,
                },
            ],
        }];
        let mut output = Vec::new();

        emit_render_lines(&mut output, &lines, &StylePalette::default(), false).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.starts_with("\u{1b}[?2026h\u{1b}[1;1H"));
        // A clear mid-session is a blank frame waiting to be composited: Herdr drops the DEC 2026
        // guard under its render timeout, and that blank frame is the flicker this regressed into.
        assert!(!output.contains("\u{1b}[2J"));
        assert!(output.contains("open "));
        assert!(output.contains("a"));
        assert!(output.contains("ttps://example.com"));
        assert!(output.contains("\u{1b}[38;2;122;130;148m"));
        assert!(output.contains("\u{1b}[48;2;255;0;124m"));
        assert!(output.contains("\u{1b}[48;2;62;104;215m"));
    }

    #[test]
    fn a_styled_background_does_not_bleed_into_the_next_span() {
        force_colors();
        let lines = vec![RenderLine {
            spans: vec![
                RenderSpan {
                    text: "a".to_string(),
                    style: RenderStyle::Hint,
                },
                RenderSpan {
                    text: "tail".to_string(),
                    style: RenderStyle::Unmatched,
                },
            ],
        }];
        let mut output = Vec::new();

        emit_render_lines(&mut output, &lines, &StylePalette::default(), false).unwrap();
        let output = String::from_utf8(output).unwrap();

        let hint_bg = output
            .find("\u{1b}[48;2;255;0;124m")
            .expect("hint sets a background");
        let reset_after = output[hint_bg..]
            .find("\u{1b}[0m")
            .expect("the next span resets it");
        let tail = output[hint_bg..].find("tail").expect("tail is emitted");
        assert!(
            reset_after < tail,
            "the reset must land before the unmatched text, not after it"
        );
    }

    #[test]
    fn terminal_emission_positions_lines_without_newlines() {
        force_colors();
        let lines = vec![
            RenderLine {
                spans: vec![RenderSpan {
                    text: "one".to_string(),
                    style: RenderStyle::Unmatched,
                }],
            },
            RenderLine {
                spans: vec![RenderSpan {
                    text: "two".to_string(),
                    style: RenderStyle::Match,
                }],
            },
        ];
        let mut output = Vec::new();

        emit_render_lines(&mut output, &lines, &StylePalette::default(), false).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("\u{1b}[1;1H"));
        assert!(output.contains("\u{1b}[2;1H"));
        assert!(!output.contains("\r\n"));
        assert!(output.ends_with("\u{1b}[1;1H\u{1b}[0m\u{1b}[0m\u{1b}[?2026l"));
    }

    #[test]
    fn a_one_shot_emit_folds_the_clear_into_the_synchronized_frame() {
        force_colors();
        let lines = vec![RenderLine {
            spans: vec![RenderSpan {
                text: "note".to_string(),
                style: RenderStyle::Unmatched,
            }],
        }];
        let mut output = Vec::new();

        emit_render_lines(&mut output, &lines, &StylePalette::default(), true).unwrap();
        let output = String::from_utf8(output).unwrap();

        // The clear must ride inside the same guarded frame, never reach the pty on its own.
        assert!(output.starts_with("\u{1b}[?2026h\u{1b}[2J"));
        assert!(output.contains("note"));
    }

    #[test]
    fn a_custom_palette_drives_the_emitted_colors() {
        force_colors();
        let mut palette = StylePalette::default();
        palette.label = crate::model::StyleSpec {
            fg: None,
            bg: Some([1, 2, 3]),
            bold: false,
        };
        let lines = vec![RenderLine {
            spans: vec![RenderSpan {
                text: "a".to_string(),
                style: RenderStyle::Hint,
            }],
        }];
        let mut output = Vec::new();

        emit_render_lines(&mut output, &lines, &palette, false).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("\u{1b}[48;2;1;2;3m"));
        assert!(!output.contains("\u{1b}[48;2;255;0;124m"));
        assert!(!output.contains("\u{1b}[1m"), "bold was overridden off");
    }
}
