use crate::model::{RenderLine, RenderStyle};
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
/// Frames never clear the screen, because a clear-then-repaint shows a blank frame whenever the
/// terminal composites between the two, and the DEC 2026 guards that were meant to prevent that
/// are best-effort — Herdr drops them under its render timeout. The caller therefore owns
/// coverage: a repaint loop must paint full-width rows for the whole frame so overwriting in
/// place is complete; a one-shot emit that covers less must `clear_screen` first. The frame is
/// also assembled in memory and handed over as one write, keeping a mid-write composite down to
/// a soft tear.
pub fn emit_render_lines(writer: &mut impl Write, lines: &[RenderLine]) -> Result<()> {
    let mut frame = Vec::new();
    queue!(frame, BeginSynchronizedUpdate)?;

    for (line_index, line) in lines.iter().enumerate() {
        queue!(frame, MoveTo(0, line_index as u16))?;
        for span in &line.spans {
            queue_style(&mut frame, span.style)?;
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

/// Clears the whole screen. Entry-time only: a clear inside the render loop is a flicker.
pub fn clear_screen(writer: &mut impl Write) -> Result<()> {
    queue!(writer, Clear(ClearType::All))?;
    Ok(())
}

fn queue_style(writer: &mut impl Write, style: RenderStyle) -> Result<()> {
    match style {
        // Every arm must open with a reset. Without one this style inherits whatever background
        // the previous span set, and a lone Hint cell bleeds cyan across the rest of the frame.
        RenderStyle::Unmatched => queue!(
            writer,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(Color::DarkGrey),
            SetAttribute(Attribute::Dim)
        )?,
        RenderStyle::Match => queue!(
            writer,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(Color::Yellow)
        )?,
        RenderStyle::Hint => queue!(
            writer,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(Color::Black),
            SetBackgroundColor(Color::Cyan),
            SetAttribute(Attribute::Bold)
        )?,
        // Vim's visual mode only paints a background; the text underneath keeps its own colour.
        // Reset alone restores the terminal's default foreground, which reads brighter than the
        // dimmed grey around it without shouting like the yellow match colour.
        RenderStyle::Selection => queue!(
            writer,
            SetAttribute(Attribute::Reset),
            SetBackgroundColor(Color::Rgb {
                r: 0x4d,
                g: 0x3a,
                b: 0x4a
            })
        )?,
        RenderStyle::Cursor => queue!(
            writer,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(Color::Black),
            SetBackgroundColor(Color::White)
        )?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RenderSpan, RenderStyle};

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

        emit_render_lines(&mut output, &lines).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.starts_with("\u{1b}[?2026h\u{1b}[1;1H"));
        // A clear mid-session is a blank frame waiting to be composited: Herdr drops the DEC 2026
        // guard under its render timeout, and that blank frame is the flicker this regressed into.
        assert!(!output.contains("\u{1b}[2J"));
        assert!(output.contains("open "));
        assert!(output.contains("a"));
        assert!(output.contains("ttps://example.com"));
        assert!(output.contains("\u{1b}[38;5;0m"));
        assert!(output.contains("\u{1b}[48;5;14m"));
        assert!(output.contains("\u{1b}[38;5;11m"));
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

        emit_render_lines(&mut output, &lines).unwrap();
        let output = String::from_utf8(output).unwrap();

        let hint_bg = output
            .find("\u{1b}[48;5;14m")
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

        emit_render_lines(&mut output, &lines).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("\u{1b}[1;1H"));
        assert!(output.contains("\u{1b}[2;1H"));
        assert!(!output.contains("\r\n"));
        assert!(output.ends_with("\u{1b}[1;1H\u{1b}[0m\u{1b}[0m\u{1b}[?2026l"));
    }

    #[test]
    fn clear_screen_emits_a_full_clear() {
        let mut output = Vec::new();
        clear_screen(&mut output).unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "\u{1b}[2J");
    }
}
