use crate::herdr::layout::{derive_source_geometry, derive_source_pane_geometries, LayoutSnapshot};
use crate::model::{
    PaneId, PaneTextCaptureMode, PatternSpec, PickerAction, PickerPaneSnapshot,
    PickerReturnContext, PickerSnapshot, SiblingPane, SourcePaneSnapshot, StylePalette,
    VisibleViewport,
};
use anyhow::{bail, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Snapshot and atomic markers coordinating the hidden picker launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerLaunchFiles {
    pub snapshot_path: PathBuf,
    pub ready_path: PathBuf,
    pub painted_path: PathBuf,
    /// Where each sibling placeholder publishes its tty path, in `siblings` order.
    pub sibling_reports: Vec<PathBuf>,
    ready_temp_path: PathBuf,
    painted_temp_path: PathBuf,
}

impl PickerLaunchFiles {
    /// Allocates unique absent paths and writes the snapshot.
    pub fn create(snapshot: &PickerSnapshot) -> Result<Self> {
        let stem = unique_stem();
        let files = Self {
            snapshot_path: std::env::temp_dir().join(format!("{stem}.json")),
            ready_path: std::env::temp_dir().join(format!("{stem}.ready")),
            painted_path: std::env::temp_dir().join(format!("{stem}.painted")),
            ready_temp_path: std::env::temp_dir().join(format!("{stem}.ready.tmp")),
            painted_temp_path: std::env::temp_dir().join(format!("{stem}.painted.tmp")),
            sibling_reports: Vec::new(),
        }
        .with_sibling_reports(snapshot.source.siblings.len());
        files.cleanup()?;
        let json = serde_json::to_vec(snapshot).context("failed to serialize picker snapshot")?;
        fs::write(&files.snapshot_path, json)
            .with_context(|| format!("failed to write {}", files.snapshot_path.display()))?;
        Ok(files)
    }

    /// Reconstructs the launch-file set inside the spawned picker process.
    pub fn from_paths(snapshot_path: PathBuf, ready_path: PathBuf, painted_path: PathBuf) -> Self {
        Self {
            snapshot_path,
            ready_temp_path: ready_path.with_extension("ready.tmp"),
            painted_temp_path: painted_path.with_extension("painted.tmp"),
            ready_path,
            painted_path,
            sibling_reports: Vec::new(),
        }
    }

    /// Derives the sibling report paths from the snapshot path, so the action and picker
    /// processes agree on them without passing one argument per pane.
    pub fn with_sibling_reports(mut self, count: usize) -> Self {
        self.sibling_reports = (0..count)
            .map(|index| {
                self.snapshot_path
                    .with_extension(format!("sibling{index}.tty"))
            })
            .collect();
        self
    }

    /// Atomically releases the picker after its painted tab receives focus.
    pub fn signal_ready(&self) -> Result<()> {
        signal_marker(&self.ready_temp_path, &self.ready_path, b"ready")
            .context("failed to release picker launch barrier")
    }

    /// Atomically tells the action process that the hidden tab has a complete first frame.
    pub fn signal_painted(&self) -> Result<()> {
        signal_marker(&self.painted_temp_path, &self.painted_path, b"painted")
            .context("failed to signal painted picker frame")
    }

    /// Removes all launch files, ignoring already-removed files.
    pub fn cleanup(&self) -> Result<()> {
        let mut first = None;
        for path in [
            &self.snapshot_path,
            &self.ready_path,
            &self.painted_path,
            &self.ready_temp_path,
            &self.painted_temp_path,
        ]
        .into_iter()
        .chain(&self.sibling_reports)
        {
            if let Err(error) = remove_file(path) {
                if first.is_none() {
                    first = Some(error);
                }
            }
        }
        first.map_or(Ok(()), Err)
    }
}

fn signal_marker(temp_path: &Path, marker_path: &Path, payload: &[u8]) -> Result<()> {
    fs::write(temp_path, payload)?;
    fs::rename(temp_path, marker_path)
        .with_context(|| format!("failed to publish marker at {}", marker_path.display()))
}

/// Publishes this placeholder pane's tty path so the picker can paint the pane directly.
pub fn report_sibling_tty(report: &Path) -> Result<()> {
    let tty = rustix::termios::ttyname(std::io::stdin(), Vec::new())
        .context("placeholder pane has no tty")?;
    signal_marker(&report.with_extension("tty.tmp"), report, tty.as_bytes())
        .context("failed to report placeholder tty")
}

/// Waits a bounded duration for a sibling placeholder's report, then opens its tty for writing.
pub fn open_sibling_tty(report: &Path, timeout: Duration) -> Result<File> {
    wait_for_marker(report, timeout, "sibling pane tty report")?;
    let tty = fs::read_to_string(report)
        .with_context(|| format!("failed to read {}", report.display()))?;
    OpenOptions::new()
        .write(true)
        // The picker already owns its own controlling terminal; never adopt a sibling's.
        .custom_flags(rustix::fs::OFlags::NOCTTY.bits() as i32)
        .open(&tty)
        .with_context(|| format!("failed to open sibling tty {tty}"))
}

/// Waits a bounded duration for the launch barrier.
pub fn wait_for_ready(path: &Path, timeout: Duration) -> Result<()> {
    wait_for_marker(path, timeout, "Herdr layout launch barrier")
}

/// Waits a bounded duration for the hidden picker tab to paint its first frame.
pub fn wait_for_painted(path: &Path, timeout: Duration) -> Result<()> {
    wait_for_marker(path, timeout, "painted picker frame")
}

fn wait_for_marker(path: &Path, timeout: Duration, description: &str) -> Result<()> {
    let started = Instant::now();
    while !path.exists() {
        if started.elapsed() >= timeout {
            bail!("timed out waiting for {description} at {}", path.display());
        }
        // Every launch-path marker is on the critical path to the first visible frame, and the
        // writer usually lands a few ms after the first check: a 10 ms nap added most of a tick
        // per marker. A 1 ms stat loop costs nothing next to that.
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn build_source_snapshot(
    layout: &LayoutSnapshot,
    target: &PaneId,
    logical_lines: Vec<String>,
    visible_viewport: Option<VisibleViewport>,
    session: PickerReturnContext,
    action: PickerAction,
    custom_patterns: Vec<PatternSpec>,
    flash_exit_on_yank: bool,
    palette: StylePalette,
    siblings: Vec<SiblingPane>,
) -> Result<PickerSnapshot> {
    let source_tab_id = layout
        .tab_id
        .clone()
        .context("pane layout did not include source tab id")?;
    let workspace_id = layout
        .workspace_id
        .clone()
        .context("pane layout did not include workspace id")?;
    let source_panes = derive_source_pane_geometries(layout);
    let geometry = derive_source_geometry(layout, target);
    let picker_content_rect = if geometry.zoomed && geometry.target_focused {
        layout
            .area
            .reserve_right_gutter(u16::from(layout.area.width > 1))
    } else {
        geometry.source_content_rect
    };
    if !source_panes.iter().any(|pane| pane.pane_id == *target) {
        bail!("target pane geometry missing from source layout");
    }
    Ok(PickerSnapshot {
        source: SourcePaneSnapshot {
            target_pane_id: target.clone(),
            source_tab_id,
            workspace_id,
            source_panes,
            target_content_width: geometry.source_content_rect.width,
            target_content_height: geometry.source_content_rect.height,
            logical_lines,
            visible_viewport,
            capture_mode: PaneTextCaptureMode::ExactVisibleUnwrapped,
            siblings,
        },
        picker: PickerPaneSnapshot {
            content_width: picker_content_rect.width,
            content_height: picker_content_rect.height,
        },
        session,
        action,
        custom_patterns,
        flash_exit_on_yank,
        palette,
    })
}

pub fn read_snapshot_file(path: &Path) -> Result<PickerSnapshot> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("failed to parse {}", path.display()))
}

fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn unique_stem() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let counter = FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("herdr-flash-{millis}-{}-{counter}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_markers_release_and_cleanup_is_idempotent() {
        let snapshot = PickerSnapshot {
            source: crate::model::SourcePaneSnapshot {
                target_pane_id: PaneId::new("p1"),
                source_tab_id: "t1".into(),
                workspace_id: "w1".into(),
                source_panes: Vec::new(),
                target_content_width: 80,
                target_content_height: 24,
                logical_lines: Vec::new(),
                visible_viewport: None,
                capture_mode: PaneTextCaptureMode::ExactVisibleUnwrapped,
                siblings: Vec::new(),
            },
            picker: PickerPaneSnapshot {
                content_width: 80,
                content_height: 24,
            },
            session: PickerReturnContext {
                return_tab_id: "t1".into(),
                return_pane_id: PaneId::new("p1"),
            },
            action: PickerAction::Flash,
            custom_patterns: Vec::new(),
            flash_exit_on_yank: true,
            palette: StylePalette::default(),
        };
        let files = PickerLaunchFiles::create(&snapshot).unwrap();
        let writer = files.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            writer.signal_painted().unwrap();
        });
        wait_for_painted(&files.painted_path, Duration::from_secs(1)).unwrap();
        files.signal_ready().unwrap();
        wait_for_ready(&files.ready_path, Duration::from_secs(1)).unwrap();
        files.cleanup().unwrap();
        files.cleanup().unwrap();
    }

    #[test]
    fn sibling_reports_sit_beside_the_snapshot_and_are_cleaned_up() {
        let files = PickerLaunchFiles::from_paths(
            std::env::temp_dir().join(format!("flash-sib-{}.json", std::process::id())),
            "unused.ready".into(),
            "unused.painted".into(),
        )
        .with_sibling_reports(2);
        assert_eq!(
            files.sibling_reports[1].file_name().unwrap(),
            format!("flash-sib-{}.sibling1.tty", std::process::id()).as_str()
        );

        fs::write(&files.sibling_reports[0], b"/dev/ttys000").unwrap();
        files.cleanup().unwrap();
        assert!(!files.sibling_reports[0].exists());
    }

    #[test]
    fn ready_marker_times_out() {
        let path = std::env::temp_dir().join(format!("flash-missing-{}", std::process::id()));
        let _ = fs::remove_file(&path);
        assert!(wait_for_ready(&path, Duration::from_millis(20))
            .unwrap_err()
            .to_string()
            .contains("timed out"));
    }
}
