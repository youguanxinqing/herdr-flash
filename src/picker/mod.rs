mod copy;
mod flash;
mod input;
mod open_url;
pub mod render;
mod session;

pub use flash::run_flash_picker;
pub use render::{
    build_picker_view, build_readonly_picker_view, run_readonly_picker, PickerView,
    ReadonlyPickerView,
};
pub use session::run_picker;

use crate::model::{PickerAction, PickerSnapshot};
use anyhow::Result;

/// Paints the picker's initial frame straight to stdout, before the launch barrier releases.
///
/// The picker tab stays hidden until this frame is complete. The interactive picker repaints the
/// same state after focus, so handoff does not expose a blank intermediate pane.
pub(crate) fn paint_entry_preview(snapshot: &PickerSnapshot) -> Result<()> {
    let mut stdout = std::io::stdout();
    // Hide directly rather than via CursorGuard: the picker's own guard takes over ownership of
    // cursor visibility when it starts, and restores it on exit.
    crossterm::queue!(stdout, crossterm::cursor::Hide)?;
    match snapshot.action {
        PickerAction::Flash => flash::emit_entry_preview(snapshot, &mut stdout),
        PickerAction::Copy | PickerAction::OpenUrl => {
            render::emit_entry_preview(snapshot, &mut stdout)
        }
    }
}
