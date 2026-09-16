use crate::herdr::context::HerdrContext;
use crate::herdr::layout::LayoutSnapshot;
use crate::herdr::protocol;
use crate::herdr::socket::UnixSocketTransport;
use crate::model::PaneId;
use anyhow::{Context, Result};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/**
 * Application-facing argv layout for a temporary Herdr tab.
 */
#[derive(Debug, Clone, PartialEq)]
pub enum LaunchLayoutNode {
    Pane {
        command: Vec<String>,
    },
    Split {
        direction: crate::model::SplitDirection,
        ratio: f32,
        first: Box<LaunchLayoutNode>,
        second: Box<LaunchLayoutNode>,
    },
}

/**
 * Identities created for the temporary picker layout.
 */
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedLayout {
    pub tab_id: String,
    pub picker_pane_id: PaneId,
}

/**
 * Domain seam for all Herdr operations used by picker orchestration.
 */
pub trait HerdrClient {
    fn pane_layout(&mut self, pane: &PaneId) -> Result<LayoutSnapshot>;
    fn pane_read_visible(&mut self, pane: &PaneId, lines: u16) -> Result<String>;
    fn apply_hidden_layout(
        &mut self,
        workspace_id: &str,
        tab_label: &str,
        root: &LaunchLayoutNode,
    ) -> Result<AppliedLayout>;
    fn focus_pane(&mut self, pane: &PaneId) -> Result<()>;
    fn focus_tab(&mut self, tab_id: &str) -> Result<()>;
    fn close_tab(&mut self, tab_id: &str) -> Result<()>;
}

/**
 * Production Herdr client over the inherited Unix socket.
 */
#[derive(Debug, Clone)]
pub struct SocketHerdrClient {
    transport: UnixSocketTransport,
}

impl SocketHerdrClient {
    pub fn from_context(context: &HerdrContext) -> Result<Self> {
        let path = context
            .socket_path
            .clone()
            .context("HERDR_SOCKET_PATH is missing; Herdr Flash requires Herdr 0.7.4 or newer")?;
        Ok(Self {
            transport: UnixSocketTransport::new(path),
        })
    }

    fn call(&self, method: &str, params: impl Serialize) -> Result<(String, serde_json::Value)> {
        let id = format!(
            "flash-{}-{}",
            std::process::id(),
            REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let value = self
            .transport
            .exchange(&protocol::request(id.clone(), method, params))?;
        Ok((id, value))
    }
}

impl HerdrClient for SocketHerdrClient {
    fn pane_layout(&mut self, pane: &PaneId) -> Result<LayoutSnapshot> {
        let (id, value) = self.call("pane.layout", protocol::pane_target(&pane.0))?;
        protocol::pane_layout(value, &id)
    }

    fn pane_read_visible(&mut self, pane: &PaneId, lines: u16) -> Result<String> {
        let (id, value) = self.call("pane.read", protocol::pane_read_params(&pane.0, lines))?;
        protocol::pane_read(value, &id)
    }

    fn apply_hidden_layout(
        &mut self,
        workspace_id: &str,
        tab_label: &str,
        root: &LaunchLayoutNode,
    ) -> Result<AppliedLayout> {
        let (id, value) = self.call(
            "layout.apply",
            protocol::layout_apply_params(workspace_id, tab_label, root),
        )?;
        let (tab_id, picker_pane_id) = protocol::applied_layout(value, &id, root)?;
        // Herdr 0.9.0 omits layout.apply from its geometry-invalidating requests. A zero-delta
        // resize reconciles hidden PTYs before the painted barrier, without revealing the tab.
        // Keep the created IDs even if an older server rejects this optional refresh: the
        // existing bounded preview wait and explicit cleanup must still be able to run.
        let refresh = self
            .call(
                "pane.resize",
                protocol::geometry_refresh_params(&picker_pane_id),
            )
            .and_then(|(id, value)| protocol::geometry_refreshed(value, &id));
        if let Err(error) = refresh {
            eprintln!("Herdr Flash: hidden layout geometry refresh unavailable: {error:#}");
        }
        Ok(AppliedLayout {
            tab_id,
            picker_pane_id: PaneId::new(picker_pane_id),
        })
    }

    fn focus_pane(&mut self, pane: &PaneId) -> Result<()> {
        let (id, value) = self.call("pane.focus", protocol::pane_target(&pane.0))?;
        protocol::pane_focused(value, &id)
    }

    fn focus_tab(&mut self, tab_id: &str) -> Result<()> {
        let (id, value) = self.call("tab.focus", protocol::tab_target(tab_id))?;
        protocol::tab_focused(value, &id)
    }

    fn close_tab(&mut self, tab_id: &str) -> Result<()> {
        let (id, value) = self.call("tab.close", protocol::tab_target(tab_id))?;
        protocol::tab_closed(value, &id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::executor::launch_layout_tab_picker;
    use crate::herdr::snapshot::PickerLaunchFiles;
    use crate::model::{PickerAction, StylePalette};
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::thread;

    /// Models the 0.9.0 server leaving a hidden PTY stale until a geometry request arrives.
    fn launch_against_server(refresh_supported: bool) {
        let socket = std::env::temp_dir().join(format!(
            "flash-refresh-{}-{refresh_supported}.sock",
            std::process::id()
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let mut files = None;
            let mut refreshed = false;
            loop {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                let method = request["method"].as_str().unwrap();
                let result = match method {
                    "pane.layout" => json!({"type":"pane_layout", "layout": {
                        "area":{"x":0,"y":0,"width":80,"height":24},
                        "panes":[{"pane_id":"w:p1","focused":true,
                            "rect":{"x":0,"y":0,"width":80,"height":24}}],
                        "focused_pane_id":"w:p1", "tab_id":"w:t1", "workspace_id":"w"
                    }}),
                    "pane.read" => json!({"type":"pane_read","read":{"text":"hello"}}),
                    "layout.apply" => {
                        assert_eq!(request["params"]["focus"], false);
                        let command = &request["params"]["root"]["command"];
                        let launch = PickerLaunchFiles::from_paths(
                            command[3].as_str().unwrap().into(),
                            command[5].as_str().unwrap().into(),
                            command[7].as_str().unwrap().into(),
                        );
                        // Legacy servers resize hidden tabs without an explicit refresh.
                        if !refresh_supported {
                            launch.signal_painted().unwrap();
                        }
                        files = Some(launch);
                        json!({"type":"layout_apply","layout":{"tab_id":"w:t2",
                            "root":{"type":"pane","pane_id":"w:p2"}}})
                    }
                    "pane.resize" => {
                        assert_eq!(
                            request["params"],
                            json!({
                                "pane_id":"w:p2", "direction":"right", "amount":0.0
                            })
                        );
                        if refresh_supported {
                            refreshed = true;
                            files.as_ref().unwrap().signal_painted().unwrap();
                            json!({"type":"pane_resize","resize":{"changed":false}})
                        } else {
                            let error = json!({"id":request["id"], "error":{
                                "code":"unknown_method", "message":"unsupported pane.resize"
                            }});
                            writeln!(reader.get_mut(), "{error}").unwrap();
                            continue;
                        }
                    }
                    "pane.focus" => json!({"type":"pane_info"}),
                    _ => panic!("unexpected request {method}"),
                };
                let response = json!({"id":request["id"],"result":result});
                writeln!(reader.get_mut(), "{response}").unwrap();
                if method == "pane.focus" {
                    return (files.unwrap(), refreshed);
                }
            }
        });
        let mut client = SocketHerdrClient {
            transport: UnixSocketTransport::new(&socket),
        };
        launch_layout_tab_picker(
            &mut client,
            &PaneId::new("w:p1"),
            Path::new("/tmp/flash"),
            PickerAction::Flash,
            Vec::new(),
            true,
            StylePalette::default(),
        )
        .unwrap();
        let (files, refreshed) = server.join().unwrap();
        let painted = files.painted_path.exists();
        assert!(files.ready_path.exists());
        files.cleanup().unwrap();
        std::fs::remove_file(socket).unwrap();
        assert!(
            painted,
            "entry exhausted its timeout waiting for a stale hidden PTY"
        );
        assert_eq!(refreshed, refresh_supported);
    }

    use std::path::Path;

    #[test]
    fn hidden_layout_refreshes_geometry_before_waiting_for_first_frame() {
        launch_against_server(true);
    }

    #[test]
    fn unsupported_geometry_refresh_preserves_legacy_launch() {
        launch_against_server(false);
    }
}
