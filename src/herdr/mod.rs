pub mod client;
pub mod context;
pub mod executor;
pub mod layout;
mod protocol;
pub mod snapshot;
mod socket;

use crate::config::{resolve_flash_exit_on_yank, resolve_pattern_specs};
use crate::herdr::client::SocketHerdrClient;
use crate::herdr::context::HerdrContext;
use crate::herdr::executor::{
    cleanup_session, launch_layout_tab_picker, run_snapshot_picker, zoom_picker,
};
use crate::herdr::snapshot::{read_snapshot_file, wait_for_ready, PickerLaunchFiles};
use crate::model::{PaneId, PickerAction};
use anyhow::{bail, Context, Result};
use crossterm::{cursor, execute, terminal};
use std::io::{stdout, Write};
use std::path::Path;
use std::time::{Duration, Instant};

pub use layout::derive_layout_recreation_plan;

/// Narrow production adapter for Herdr layout launch and picker cleanup.
#[derive(Debug, Clone)]
pub struct HerdrAdapter {
    context: HerdrContext,
}

impl HerdrAdapter {
    pub fn from_env() -> Self {
        Self {
            context: HerdrContext::from_env(),
        }
    }

    pub fn target_pane_from_context(&self) -> Option<PaneId> {
        self.context.target_pane()
    }

    /**
     * Opens the picker that copies any supported visible token.
     */
    pub fn open_copy_picker(&self, target: &PaneId) -> Result<()> {
        self.open_picker(target, PickerAction::Copy)
    }

    /**
     * Opens the picker that launches browser-openable visible URLs.
     */
    pub fn open_url_picker(&self, target: &PaneId) -> Result<()> {
        self.open_picker(target, PickerAction::OpenUrl)
    }

    /**
     * Opens the incremental-search picker that copies any visible text.
     */
    pub fn open_flash_picker(&self, target: &PaneId) -> Result<()> {
        self.open_picker(target, PickerAction::Flash)
    }

    fn open_picker(&self, target: &PaneId, action: PickerAction) -> Result<()> {
        let binary = std::env::current_exe().context("failed to locate herdr-flash binary")?;
        let patterns = match action {
            PickerAction::Copy => resolve_pattern_specs(self.context.focused_pane_cwd().as_deref()),
            // Flash matches the typed query literally, so pattern config buys it nothing.
            PickerAction::OpenUrl | PickerAction::Flash => Vec::new(),
        };
        let flash_exit_on_yank = match action {
            PickerAction::Flash => resolve_flash_exit_on_yank(),
            PickerAction::Copy | PickerAction::OpenUrl => true,
        };
        let mut client = SocketHerdrClient::from_context(&self.context)?;
        launch_layout_tab_picker(
            &mut client,
            target,
            &binary,
            action,
            patterns,
            flash_exit_on_yank,
        )?;
        Ok(())
    }

    /// Waits for layout completion, runs the picker, and always cleans up explicit resources.
    pub fn run_picker_from_snapshot(&self, snapshot_path: &Path, ready_path: &Path) -> Result<()> {
        let snapshot = read_snapshot_file(snapshot_path)?;
        let temp_tab = self
            .context
            .tab_id
            .clone()
            .context("picker process is missing HERDR_TAB_ID")?;
        let pane = self
            .context
            .pane_id
            .clone()
            .map(PaneId::new)
            .context("picker process is missing HERDR_PANE_ID")?;
        let files = PickerLaunchFiles {
            snapshot_path: snapshot_path.to_path_buf(),
            ready_path: ready_path.to_path_buf(),
            marker_temp_path: ready_path.with_extension("ready.tmp"),
        };
        let mut client = SocketHerdrClient::from_context(&self.context)?;
        let primary = wait_for_ready(ready_path, Duration::from_secs(10))
            .and_then(|_| zoom_picker(&mut client, &snapshot, &pane))
            .and_then(|_| {
                if snapshot.session.zoom_picker {
                    wait_for_terminal_size(
                        snapshot.source.target_content_width,
                        snapshot.source.target_content_height,
                        Duration::from_secs(2),
                    )
                } else {
                    Ok(())
                }
            })
            .and_then(|_| run_snapshot_picker(&snapshot));
        let cleanup = cleanup_session(&mut client, &snapshot.session, &temp_tab);
        let files_cleanup = files.cleanup();
        match primary {
            Err(e) => {
                if let Err(c) = cleanup {
                    eprintln!("cleanup also failed: {c:#}");
                }
                if let Err(c) = files_cleanup {
                    eprintln!("file cleanup also failed: {c:#}");
                }
                Err(e)
            }
            Ok(()) => {
                cleanup?;
                files_cleanup?;
                Ok(())
            }
        }
    }
}

/**
 * Waits for Herdr to propagate an asynchronous pane resize to the picker PTY.
 */
fn wait_for_terminal_size(width: u16, height: u16, timeout: Duration) -> Result<()> {
    let started = Instant::now();

    loop {
        let (current_width, current_height) =
            terminal::size().context("failed to read picker terminal size")?;

        if (current_width, current_height) == (width, height) {
            return Ok(());
        }

        if started.elapsed() >= timeout {
            bail!(
                "timed out waiting for picker terminal resize to {width}x{height}; current size is {current_width}x{current_height}"
            );
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Clears an inert pane and remains alive until Herdr closes its tab.
pub fn run_idle() -> Result<()> {
    let mut out = stdout();
    execute!(
        out,
        terminal::Clear(terminal::ClearType::All),
        cursor::MoveTo(0, 0)
    )?;
    out.flush()?;
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
