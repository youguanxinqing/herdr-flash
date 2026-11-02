pub mod client;
pub mod context;
pub mod executor;
pub mod layout;
mod lock;
mod protocol;
pub mod snapshot;
mod socket;

use crate::config::{resolve_flash_exit_on_yank, resolve_palette, resolve_pattern_specs};
use crate::herdr::client::SocketHerdrClient;
use crate::herdr::context::HerdrContext;
use crate::herdr::executor::{cleanup_session, launch_layout_tab_picker, run_snapshot_picker};
use crate::herdr::snapshot::{
    open_sibling_tty, read_snapshot_file, report_sibling_tty, wait_for_ready, PickerLaunchFiles,
};
use crate::model::{PaneId, PickerAction, PickerSnapshot};
use crate::picker::{Sibling, Surface};
use anyhow::{bail, Context, Result};
use crossterm::{cursor, execute, terminal};
use std::fs::File;
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
        let palette = resolve_palette();
        let mut client = SocketHerdrClient::from_context(&self.context)?;
        launch_layout_tab_picker(
            &mut client,
            target,
            &binary,
            action,
            patterns,
            flash_exit_on_yank,
            palette,
        )?;
        Ok(())
    }

    /// Waits for layout completion, runs the picker, and always cleans up explicit resources.
    pub fn run_picker_from_snapshot(
        &self,
        snapshot_path: &Path,
        ready_path: &Path,
        painted_path: &Path,
    ) -> Result<()> {
        let snapshot = read_snapshot_file(snapshot_path)?;
        let files = PickerLaunchFiles::from_paths(
            snapshot_path.to_path_buf(),
            ready_path.to_path_buf(),
            painted_path.to_path_buf(),
        )
        .with_sibling_reports(snapshot.source.siblings.len());
        // layout.apply keeps this tab hidden. Wait for Herdr's asynchronous PTY resize, paint the
        // complete first frame, then tell the action process it can switch tabs without exposing
        // an empty or mis-sized pane. Preview failure remains best-effort so launch can fall back.
        let deadline = Instant::now() + Duration::from_millis(250);
        let preview = wait_for_terminal_size(
            snapshot.picker.content_width,
            snapshot.picker.content_height,
            Duration::from_millis(250),
        );
        // Siblings share the same budget: their placeholders start alongside this process.
        let mut siblings = attach_siblings(&snapshot, &files, deadline);
        let preview =
            preview.and_then(|_| crate::picker::paint_entry_preview(&snapshot, &mut siblings));
        if let Err(error) = preview {
            eprintln!("Herdr Flash: entry preview failed: {error:#}");
        }
        if let Err(error) = files.signal_painted() {
            eprintln!("Herdr Flash: failed to signal entry preview: {error:#}");
        }
        let temp_tab = self
            .context
            .tab_id
            .clone()
            .context("picker process is missing HERDR_TAB_ID")?;
        // While this process lives, a repeat action keypress targeting this tab must be refused
        // instead of stacking a second picker over this one's rendered frame.
        let reentry_guard = lock::PickerTabLock::acquire(&temp_tab);
        let mut client = SocketHerdrClient::from_context(&self.context)?;
        let primary = wait_for_ready(ready_path, Duration::from_secs(10))
            .and_then(|_| run_snapshot_picker(&snapshot, &mut siblings));
        // Released before close_tab: Herdr may kill this process together with its tab, and the
        // Drop fallback would never run. The launch files go first for the same reason — removed
        // after close_tab, they leaked into the temp dir on every run.
        drop(reentry_guard);
        let files_cleanup = files.cleanup();
        let cleanup = cleanup_session(&mut client, &snapshot.session, &temp_tab);
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

/**
 * Opens every sibling placeholder that reports in before `deadline` and waits, within the same
 * deadline, for each to reach its captured size.
 *
 * A sibling that never reports is left out: its pane stays blank and its text is not searched,
 * which is the single-pane picker. A sibling still mid-resize is kept, because every frame is
 * fitted to its live size anyway.
 */
fn attach_siblings(
    snapshot: &PickerSnapshot,
    files: &PickerLaunchFiles,
    deadline: Instant,
) -> Vec<Sibling<File>> {
    let attached = snapshot
        .source
        .siblings
        .iter()
        .zip(&files.sibling_reports)
        .filter_map(|(pane, report)| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match open_sibling_tty(report, remaining) {
                Ok(out) => Some(Sibling {
                    pane: pane.clone(),
                    out,
                }),
                Err(error) => {
                    eprintln!("Herdr Flash: pane {} left out: {error:#}", pane.pane_id);
                    None
                }
            }
        })
        .collect::<Vec<_>>();
    for sibling in &attached {
        let expected = (
            usize::from(sibling.pane.content_width),
            usize::from(sibling.pane.content_height),
        );
        while sibling.out.live_size() != Some(expected) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    attached
}

/// Clears an inert pane and remains alive until Herdr closes its tab.
///
/// With `report`, the pane is a flash sibling: once blank, it publishes its tty so the picker can
/// paint it. Clearing first matters — a clear arriving after the picker's frame would erase it.
pub fn run_idle(report: Option<&Path>) -> Result<()> {
    let mut out = stdout();
    execute!(
        out,
        terminal::Clear(terminal::ClearType::All),
        cursor::MoveTo(0, 0),
        cursor::Hide
    )?;
    out.flush()?;
    // Exiting would close this pane and reflow the picker's layout, so a failed report only
    // leaves the pane out of the search.
    if let Some(Err(error)) = report.map(report_sibling_tty) {
        eprintln!("Herdr Flash: {error:#}");
    }
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
