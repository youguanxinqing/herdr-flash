use crate::herdr::client::{HerdrClient, LaunchLayoutNode};
use crate::herdr::layout::{
    derive_layout_recreation_plan, derive_source_geometry, derive_source_pane_geometries,
    LayoutSnapshot,
};
use crate::herdr::snapshot::{build_source_snapshot, wait_for_painted, PickerLaunchFiles};
use crate::model::{
    LayoutNode, PaneId, PatternSpec, PickerAction, PickerOutcome, PickerReturnContext,
    PickerSnapshot, SiblingPane, StylePalette,
};
use crate::picker::Sibling;
use crate::viewport::map_visible_viewport;
use anyhow::{bail, Context, Result};
use std::fs::File;
use std::path::Path;
use std::time::Duration;

const PAINTED_FRAME_TIMEOUT: Duration = Duration::from_millis(300);

/// Captures source state and atomically applies the temporary picker layout.
pub fn launch_layout_tab_picker<C: HerdrClient>(
    client: &mut C,
    target: &PaneId,
    binary_path: &Path,
    action: PickerAction,
    custom_patterns: Vec<PatternSpec>,
    flash_exit_on_yank: bool,
    palette: StylePalette,
) -> Result<()> {
    let layout = client.pane_layout(target)?;

    // prefix+s pressed inside flash mode targets a pane of the picker's own temporary tab. The
    // picker the user is already looking at is the correct result, so the repeat is a no-op.
    if let Some(tab_id) = layout.tab_id.as_deref() {
        if crate::herdr::lock::tab_has_live_picker(tab_id) {
            eprintln!(
                "Herdr Flash: a picker is already open in tab {tab_id}; ignoring repeat invocation"
            );
            return Ok(());
        }
    }

    let geometry = derive_source_geometry(&layout, target);
    let source_was_zoomed = layout.zoomed && layout_target_is_focused(&layout, target);
    let plan = if source_was_zoomed {
        None
    } else {
        Some(derive_layout_recreation_plan(&layout, target)?)
    };
    let read_lines = geometry.source_content_rect.height;

    if read_lines == 0 {
        bail!("target pane {target} has zero visible content height");
    }

    let visible_text = client.pane_read_visible(target, read_lines)?;

    let viewport = map_visible_viewport(
        visible_text.lines().map(str::to_string).collect(),
        geometry.source_content_rect.width,
        read_lines,
    );

    let return_context = PickerReturnContext {
        return_tab_id: layout
            .tab_id
            .clone()
            .context("pane layout did not include return tab id")?,
        return_pane_id: target.clone(),
    };

    // Flash searches every pane on screen. A zoomed source shows only the target, so it has no
    // siblings; the hidden pattern pickers stay single-pane.
    let siblings = match (&plan, action) {
        (Some(_), PickerAction::Flash) => capture_siblings(client, &layout, target),
        _ => Vec::new(),
    };

    let snapshot = build_source_snapshot(
        &layout,
        target,
        viewport.logical_lines.clone(),
        Some(viewport),
        return_context.clone(),
        action,
        custom_patterns,
        flash_exit_on_yank,
        palette,
        siblings,
    )?;

    let files = PickerLaunchFiles::create(&snapshot)?;

    let picker = picker_layout_node(
        binary_path,
        &files.snapshot_path,
        &files.ready_path,
        &files.painted_path,
    );
    let root = match plan {
        Some(plan) => {
            let reports = snapshot
                .source
                .siblings
                .iter()
                .zip(&files.sibling_reports)
                .collect::<Vec<_>>();
            let idle = |pane: &PaneId| {
                let report = reports
                    .iter()
                    .find(|(sibling, _)| sibling.pane_id == *pane)
                    .map(|(_, path)| path.as_path());
                idle_layout_node(binary_path, report)
            };
            convert_layout(&plan.root, target, &picker, &idle)
        }
        // A zoomed source already occupies the full content area. Replaying its hidden split tree
        // and zooming after focus creates a second visible resize; one picker pane has the same
        // final geometry without that transition.
        None => picker,
    };

    let workspace_id = layout
        .workspace_id
        .as_deref()
        .context("pane layout did not include workspace id")?;
    let tab_label = match action {
        PickerAction::Copy => "Herdr Flash: Pluck",
        PickerAction::OpenUrl => "Herdr Flash: Open URL",
        PickerAction::Flash => "Herdr Flash",
    };
    let applied = match client.apply_hidden_layout(workspace_id, tab_label, &root) {
        Ok(value) => value,
        Err(error) => {
            let _ = files.cleanup();
            return Err(error);
        }
    };

    // Keep the source tab visible until the hidden picker pane has both its final PTY geometry and
    // a complete first frame. Timeout is deliberately best-effort: a failed preview must not make
    // the action unusable.
    if let Err(error) = wait_for_painted(&files.painted_path, PAINTED_FRAME_TIMEOUT) {
        eprintln!("Herdr Flash: entry preview was not ready before focus: {error:#}");
    }

    let focus_result = client.focus_pane(&applied.picker_pane_id);

    if let Err(error) = focus_result.and_then(|_| files.signal_ready()) {
        if let Err(cleanup) = cleanup_session(client, &return_context, &applied.tab_id, None) {
            eprintln!("launch cleanup also failed: {cleanup:#}");
        }

        if let Err(cleanup) = files.cleanup() {
            eprintln!("launch file cleanup also failed: {cleanup:#}");
        }

        return Err(error);
    }

    Ok(())
}

/// Captures every other pane's visible text so flash can search the whole tab.
///
/// A pane that cannot be read is left out rather than failing the launch: it stays a blank
/// placeholder, exactly as in the single-pane picker.
fn capture_siblings<C: HerdrClient>(
    client: &mut C,
    layout: &LayoutSnapshot,
    target: &PaneId,
) -> Vec<SiblingPane> {
    derive_source_pane_geometries(layout)
        .into_iter()
        .filter(|pane| pane.pane_id != *target && pane.content_height > 0)
        .filter_map(|pane| {
            let text = client
                .pane_read_visible(&pane.pane_id, pane.content_height)
                .map_err(|error| {
                    eprintln!("Herdr Flash: skipping pane {}: {error:#}", pane.pane_id)
                })
                .ok()?;
            Some(SiblingPane {
                viewport: map_visible_viewport(
                    text.lines().map(str::to_string).collect(),
                    pane.content_width,
                    pane.content_height,
                ),
                pane_id: pane.pane_id,
                content_width: pane.content_width,
                content_height: pane.content_height,
            })
        })
        .collect()
}

/// Replays the source split tree: the target leaf runs the picker, every other leaf a placeholder.
fn convert_layout(
    node: &LayoutNode,
    target: &PaneId,
    picker: &LaunchLayoutNode,
    idle: &dyn Fn(&PaneId) -> LaunchLayoutNode,
) -> LaunchLayoutNode {
    match node {
        LayoutNode::Pane { source_pane_id, .. } if source_pane_id == target => picker.clone(),
        LayoutNode::Pane { source_pane_id, .. } => idle(source_pane_id),
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
            ..
        } => LaunchLayoutNode::Split {
            direction: *direction,
            ratio: *ratio,
            first: Box::new(convert_layout(first, target, picker, idle)),
            second: Box::new(convert_layout(second, target, picker, idle)),
        },
    }
}

/// Placeholder leaf; with a report path it publishes its tty so the picker can paint it.
fn idle_layout_node(binary: &Path, report: Option<&Path>) -> LaunchLayoutNode {
    let mut command = vec![binary.to_string_lossy().into_owned(), "idle".into()];
    if let Some(report) = report {
        command.push("--report".into());
        command.push(report.to_string_lossy().into_owned());
    }
    LaunchLayoutNode::Pane { command }
}

fn picker_layout_node(
    binary: &Path,
    snapshot: &Path,
    ready: &Path,
    painted: &Path,
) -> LaunchLayoutNode {
    LaunchLayoutNode::Pane {
        command: vec![
            binary.to_string_lossy().into_owned(),
            "pick".into(),
            "--snapshot".into(),
            snapshot.to_string_lossy().into_owned(),
            "--ready".into(),
            ready.to_string_lossy().into_owned(),
            "--painted".into(),
            painted.to_string_lossy().into_owned(),
        ],
    }
}

fn layout_target_is_focused(layout: &LayoutSnapshot, target: &PaneId) -> bool {
    layout
        .focused_pane_id
        .as_ref()
        .is_some_and(|id| id == &target.0)
        || layout
            .panes
            .iter()
            .any(|p| p.pane_id == target.0 && p.focused)
}

/// Restores the source tab and closes only the explicit temporary tab.
pub fn cleanup_session<C: HerdrClient>(
    client: &mut C,
    session: &PickerReturnContext,
    temporary_tab_id: &str,
    focus_pane: Option<&PaneId>,
) -> Result<()> {
    if temporary_tab_id.is_empty() {
        bail!("temporary picker tab id is missing");
    }

    if temporary_tab_id == session.return_tab_id {
        bail!(
            "refusing to close source tab {} as temporary picker tab",
            temporary_tab_id
        );
    }

    let mut first = None;

    if let Err(e) = client.focus_tab(&session.return_tab_id) {
        first = Some(e);
    }

    if let Some(pane) = focus_pane {
        if let Err(e) = client.focus_pane(pane) {
            if first.is_none() {
                first = Some(e);
            }
        }
    }

    if let Err(e) = client.close_tab(temporary_tab_id) {
        if first.is_none() {
            first = Some(e);
        }
    }

    first.map_or(Ok(()), Err)
}

pub fn run_snapshot_picker(
    snapshot: &PickerSnapshot,
    siblings: &mut [Sibling<File>],
) -> Result<PickerOutcome> {
    match snapshot.action {
        PickerAction::Flash => crate::picker::run_flash_picker(snapshot, siblings),
        PickerAction::Copy | PickerAction::OpenUrl => crate::picker::run_picker(snapshot),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::client::AppliedLayout;
    use crate::herdr::layout::{LayoutPane, LayoutSnapshot};
    use crate::model::{
        PaneTextCaptureMode, PickerPaneSnapshot, Rect, SourcePaneSnapshot, SplitDirection,
        StylePalette, VisibleViewport,
    };
    use anyhow::anyhow;

    #[derive(Default)]
    struct FakeClient {
        layout: Option<LayoutSnapshot>,
        calls: Vec<String>,
        launch_paths: Option<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)>,
        launch_root: Option<LaunchLayoutNode>,
        fail_focus_pane: bool,
        fail_focus_tab: bool,
    }

    impl HerdrClient for FakeClient {
        fn pane_layout(&mut self, _pane: &PaneId) -> Result<LayoutSnapshot> {
            self.calls.push("pane_layout".into());
            self.layout.take().context("missing fake layout")
        }

        fn pane_read_visible(&mut self, _pane: &PaneId, lines: u16) -> Result<String> {
            self.calls.push(format!("pane_read:{lines}"));
            Ok("https://example.com".into())
        }

        fn apply_hidden_layout(
            &mut self,
            workspace_id: &str,
            _tab_label: &str,
            root: &LaunchLayoutNode,
        ) -> Result<AppliedLayout> {
            self.calls.push(format!("apply_hidden:{workspace_id}"));
            self.launch_paths = picker_paths(root);
            self.launch_root = Some(root.clone());
            let (_, _, painted) = self
                .launch_paths
                .as_ref()
                .context("missing picker launch paths")?;
            std::fs::write(painted, b"painted")?;
            Ok(AppliedLayout {
                tab_id: "w1:t2".into(),
                picker_pane_id: PaneId::new("w1:p2"),
            })
        }

        fn focus_pane(&mut self, pane: &PaneId) -> Result<()> {
            self.calls.push(format!("focus_pane:{pane}"));
            if let Some((_, ready, painted)) = &self.launch_paths {
                assert!(!ready.exists(), "barrier released before picker focus");
                assert!(painted.exists(), "picker focused before its first frame");
            }
            if self.fail_focus_pane {
                Err(anyhow!("focus failed"))
            } else {
                Ok(())
            }
        }

        fn focus_tab(&mut self, tab_id: &str) -> Result<()> {
            self.calls.push(format!("focus_tab:{tab_id}"));
            if self.fail_focus_tab {
                Err(anyhow!("tab focus failed"))
            } else {
                Ok(())
            }
        }

        fn close_tab(&mut self, tab_id: &str) -> Result<()> {
            self.calls.push(format!("close_tab:{tab_id}"));
            Ok(())
        }
    }

    fn picker_paths(
        node: &LaunchLayoutNode,
    ) -> Option<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> {
        match node {
            LaunchLayoutNode::Pane { command }
                if command.get(1).is_some_and(|argument| argument == "pick") =>
            {
                Some((
                    command.get(3)?.into(),
                    command.get(5)?.into(),
                    command.get(7)?.into(),
                ))
            }
            LaunchLayoutNode::Split { first, second, .. } => {
                picker_paths(first).or_else(|| picker_paths(second))
            }
            _ => None,
        }
    }

    fn source_layout(zoomed: bool) -> LayoutSnapshot {
        LayoutSnapshot {
            area: Rect::new(0, 0, 80, 24),
            focused_pane_id: Some("w1:p1".into()),
            panes: vec![LayoutPane {
                focused: true,
                pane_id: "w1:p1".into(),
                rect: Rect::new(0, 0, 80, 24),
            }],
            splits: Vec::new(),
            tab_id: Some("w1:t1".into()),
            workspace_id: Some("w1".into()),
            zoomed,
        }
    }

    fn picker_snapshot() -> PickerSnapshot {
        PickerSnapshot {
            source: SourcePaneSnapshot {
                target_pane_id: PaneId::new("w1:p1"),
                source_tab_id: "w1:t1".into(),
                workspace_id: "w1".into(),
                source_panes: Vec::new(),
                target_content_width: 80,
                target_content_height: 24,
                logical_lines: Vec::new(),
                visible_viewport: Some(VisibleViewport {
                    rows: Vec::new(),
                    logical_lines: Vec::new(),
                    segments: Vec::new(),
                }),
                capture_mode: PaneTextCaptureMode::ExactVisibleUnwrapped,
                siblings: Vec::new(),
            },
            picker: PickerPaneSnapshot {
                content_width: 80,
                content_height: 24,
            },
            session: PickerReturnContext {
                return_tab_id: "w1:t1".into(),
                return_pane_id: PaneId::new("w1:p1"),
            },
            action: PickerAction::Copy,
            custom_patterns: Vec::new(),
            flash_exit_on_yank: true,
            palette: StylePalette::default(),
        }
    }

    #[test]
    fn conversion_preserves_argv_and_split() {
        let tree = LayoutNode::Split {
            direction: SplitDirection::Right,
            ratio: 0.37,
            first: Box::new(LayoutNode::Pane {
                source_pane_id: PaneId::new("a"),
                rect: Rect::new(0, 0, 1, 1),
            }),
            second: Box::new(LayoutNode::Pane {
                source_pane_id: PaneId::new("b"),
                rect: Rect::new(0, 0, 1, 1),
            }),
            rect: Rect::new(0, 0, 2, 1),
        };
        let binary = Path::new("/a b/π'");
        let picker = picker_layout_node(
            binary,
            Path::new("/s p"),
            Path::new("/r p"),
            Path::new("/p p"),
        );
        let converted = convert_layout(&tree, &PaneId::new("b"), &picker, &|_| {
            idle_layout_node(binary, None)
        });
        let LaunchLayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } = converted
        else {
            panic!("expected split layout");
        };
        assert_eq!(direction, SplitDirection::Right);
        assert_eq!(ratio, 0.37);
        assert!(matches!(
            first.as_ref(),
            LaunchLayoutNode::Pane { command }
                if command == &vec!["/a b/π'".to_string(), "idle".to_string()]
        ));
        assert!(matches!(
            second.as_ref(),
            LaunchLayoutNode::Pane { command }
                if command.first().is_some_and(|value| value == "/a b/π'")
                    && command.get(1).is_some_and(|value| value == "pick")
        ));
    }

    #[test]
    fn launch_waits_for_painted_frame_before_focus_and_ready_barrier() {
        let mut client = FakeClient {
            layout: Some(source_layout(false)),
            ..FakeClient::default()
        };

        launch_layout_tab_picker(
            &mut client,
            &PaneId::new("w1:p1"),
            Path::new("/tmp/herdr flash"),
            PickerAction::Copy,
            Vec::new(),
            true,
            StylePalette::default(),
        )
        .unwrap();

        assert_eq!(
            client.calls,
            [
                "pane_layout",
                "pane_read:24",
                "apply_hidden:w1",
                "focus_pane:w1:p2"
            ]
        );
        let (snapshot, ready, painted) = client.launch_paths.unwrap();
        assert!(ready.exists());
        assert!(painted.exists());
        let files = PickerLaunchFiles::from_paths(snapshot, ready, painted);
        files.cleanup().unwrap();
    }

    #[test]
    fn repeat_invocation_on_a_live_picker_tab_is_a_no_op() {
        let tab = "executor-reentry:tab";
        let mut layout = source_layout(false);
        layout.tab_id = Some(tab.into());
        let _lock = crate::herdr::lock::PickerTabLock::acquire(tab);
        let mut client = FakeClient {
            layout: Some(layout),
            ..FakeClient::default()
        };

        launch_layout_tab_picker(
            &mut client,
            &PaneId::new("w1:p1"),
            Path::new("/tmp/herdr-flash"),
            PickerAction::Flash,
            Vec::new(),
            true,
            StylePalette::default(),
        )
        .unwrap();

        assert_eq!(client.calls, ["pane_layout"]);
        assert!(client.launch_paths.is_none(), "no picker may be launched");
    }

    #[test]
    fn zoomed_source_uses_one_hidden_picker_pane() {
        let mut layout = source_layout(true);
        layout.panes[0].rect = Rect::new(0, 0, 40, 24);
        layout.panes.push(LayoutPane {
            focused: false,
            pane_id: "w1:p9".into(),
            rect: Rect::new(40, 0, 40, 24),
        });
        let mut client = FakeClient {
            layout: Some(layout),
            ..FakeClient::default()
        };

        launch_layout_tab_picker(
            &mut client,
            &PaneId::new("w1:p1"),
            Path::new("/tmp/herdr flash"),
            PickerAction::Flash,
            Vec::new(),
            true,
            StylePalette::default(),
        )
        .unwrap();

        assert!(matches!(
            client.launch_root,
            Some(LaunchLayoutNode::Pane { ref command })
                if command.get(1).is_some_and(|argument| argument == "pick")
        ));
        let (snapshot, ready, painted) = client.launch_paths.unwrap();
        let payload = crate::herdr::snapshot::read_snapshot_file(&snapshot).unwrap();
        assert_eq!(payload.source.target_content_width, 77);
        assert_eq!(payload.source.target_content_height, 22);
        assert_eq!(payload.picker.content_width, 79);
        assert_eq!(payload.picker.content_height, 24);
        PickerLaunchFiles::from_paths(snapshot, ready, painted)
            .cleanup()
            .unwrap();
    }

    fn split_layout() -> LayoutSnapshot {
        let mut layout = source_layout(false);
        layout.panes[0].rect = Rect::new(0, 0, 40, 24);
        layout.panes.push(LayoutPane {
            focused: false,
            pane_id: "w1:p9".into(),
            rect: Rect::new(40, 0, 40, 24),
        });
        layout.splits.push(crate::herdr::layout::LayoutSplit {
            direction: SplitDirection::Right,
            ratio: 0.5,
            rect: Rect::new(0, 0, 80, 24),
        });
        layout
    }

    fn launch_split(action: PickerAction) -> (FakeClient, PickerSnapshot, Vec<String>) {
        let mut client = FakeClient {
            layout: Some(split_layout()),
            ..FakeClient::default()
        };
        launch_layout_tab_picker(
            &mut client,
            &PaneId::new("w1:p1"),
            Path::new("/tmp/herdr-flash"),
            action,
            Vec::new(),
            true,
            StylePalette::default(),
        )
        .unwrap();
        let (snapshot, ready, painted) = client.launch_paths.clone().unwrap();
        let payload = crate::herdr::snapshot::read_snapshot_file(&snapshot).unwrap();
        let Some(LaunchLayoutNode::Split { second, .. }) = &client.launch_root else {
            panic!("a split source must replay as a split");
        };
        let LaunchLayoutNode::Pane { command } = second.as_ref() else {
            panic!("the sibling leaf must be a pane");
        };
        let idle = command.clone();
        PickerLaunchFiles::from_paths(snapshot, ready, painted)
            .with_sibling_reports(payload.source.siblings.len())
            .cleanup()
            .unwrap();
        (client, payload, idle)
    }

    #[test]
    fn flash_captures_siblings_and_their_placeholders_report_ttys() {
        let (client, payload, idle) = launch_split(PickerAction::Flash);

        // Both panes are read at their own content height (24 rows minus the border).
        assert_eq!(
            client.calls.iter().filter(|c| *c == "pane_read:22").count(),
            2
        );
        assert_eq!(payload.source.siblings.len(), 1);
        assert_eq!(payload.source.siblings[0].pane_id, PaneId::new("w1:p9"));
        assert_eq!(payload.source.siblings[0].content_width, 37);
        assert_eq!(idle[1..3], ["idle".to_string(), "--report".to_string()]);
        assert!(idle[3].ends_with(".sibling0.tty"));
    }

    #[test]
    fn pattern_pickers_stay_single_pane() {
        let (_, payload, idle) = launch_split(PickerAction::Copy);

        assert!(payload.source.siblings.is_empty());
        assert_eq!(idle[1..], ["idle".to_string()]);
    }

    #[test]
    fn a_zoomed_flash_source_searches_only_the_visible_pane() {
        let mut layout = split_layout();
        layout.zoomed = true;
        let mut client = FakeClient {
            layout: Some(layout),
            ..FakeClient::default()
        };

        launch_layout_tab_picker(
            &mut client,
            &PaneId::new("w1:p1"),
            Path::new("/tmp/herdr-flash"),
            PickerAction::Flash,
            Vec::new(),
            true,
            StylePalette::default(),
        )
        .unwrap();

        assert_eq!(
            client
                .calls
                .iter()
                .filter(|c| c.starts_with("pane_read"))
                .count(),
            1
        );
        let (snapshot, ready, painted) = client.launch_paths.unwrap();
        let payload = crate::herdr::snapshot::read_snapshot_file(&snapshot).unwrap();
        assert!(payload.source.siblings.is_empty());
        PickerLaunchFiles::from_paths(snapshot, ready, painted)
            .cleanup()
            .unwrap();
    }

    #[test]
    fn failed_focus_compensates_with_returned_tab_id_and_preserves_primary_error() {
        let mut client = FakeClient {
            layout: Some(source_layout(false)),
            fail_focus_pane: true,
            fail_focus_tab: true,
            ..FakeClient::default()
        };

        let error = launch_layout_tab_picker(
            &mut client,
            &PaneId::new("w1:p1"),
            Path::new("/tmp/herdr-flash"),
            PickerAction::Copy,
            Vec::new(),
            true,
            StylePalette::default(),
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "focus failed");
        assert!(client.calls.contains(&"focus_tab:w1:t1".into()));
        assert!(client.calls.contains(&"close_tab:w1:t2".into()));
        let (snapshot, ready, painted) = client.launch_paths.unwrap();
        assert!(!snapshot.exists() && !ready.exists() && !painted.exists());
    }

    #[test]
    fn cleanup_focuses_selected_source_before_closing_picker() {
        let session = picker_snapshot().session;
        let mut client = FakeClient::default();
        cleanup_session(&mut client, &session, "w1:t2", Some(&PaneId::new("w1:p9"))).unwrap();
        assert_eq!(
            client.calls,
            ["focus_tab:w1:t1", "focus_pane:w1:p9", "close_tab:w1:t2"]
        );
    }

    #[test]
    fn cleanup_attempts_close_after_focus_failure_and_rejects_source_tab() {
        let session = picker_snapshot().session;
        let mut client = FakeClient {
            fail_focus_tab: true,
            ..FakeClient::default()
        };

        let error = cleanup_session(&mut client, &session, "w1:t2", None).unwrap_err();

        assert_eq!(error.to_string(), "tab focus failed");
        assert_eq!(client.calls, ["focus_tab:w1:t1", "close_tab:w1:t2"]);
        assert!(cleanup_session(&mut client, &session, "w1:t1", None)
            .unwrap_err()
            .to_string()
            .contains("refusing"));
    }
}
