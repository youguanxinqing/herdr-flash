//! Re-entrancy guard for the temporary picker tab.
//!
//! Invoking the action while the picker is focused would snapshot the picker's own rendered
//! frame and stack a second picker over it. The running `pick` process publishes its tab id as
//! a pid file; the action entrypoint refuses to open a picker over any pane of a tab that still
//! has a live owner.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Marks the current process as the live picker for `tab_id` until dropped.
#[derive(Debug)]
pub(crate) struct PickerTabLock {
    path: PathBuf,
}

impl PickerTabLock {
    /// Publishes this process as the picker owning `tab_id`.
    ///
    /// Best-effort: a failed write leaves the guard inert rather than blocking the picker itself.
    pub(crate) fn acquire(tab_id: &str) -> Self {
        let path = lock_path(tab_id);
        if let Err(error) = fs::write(&path, std::process::id().to_string()) {
            eprintln!(
                "Herdr Flash: failed to write picker lock {}: {error}",
                path.display()
            );
        }
        Self { path }
    }
}

impl Drop for PickerTabLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// True when `tab_id` belongs to a picker whose process is still alive.
///
/// A stale lock (owner dead, pid recycled by an unrelated process, or an unreadable pid) is
/// removed on sight so a killed picker can never permanently wedge the action on a reused tab id.
pub(crate) fn tab_has_live_picker(tab_id: &str) -> bool {
    let path = lock_path(tab_id);
    let alive = fs::read_to_string(&path)
        .ok()
        .and_then(|content| content.trim().parse::<u32>().ok())
        .is_some_and(lock_owner_is_alive);
    if !alive {
        let _ = fs::remove_file(&path);
    }
    alive
}

fn lock_path(tab_id: &str) -> PathBuf {
    let safe: String = tab_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || "._-".contains(ch) {
                ch
            } else {
                '_'
            }
        })
        .collect();
    std::env::temp_dir().join(format!("herdr-flash-picker-{safe}.pid"))
}

/// Liveness probe: the pid must exist AND still be a herdr-flash binary.
///
/// Comparing the command name is what makes a Herdr crash or restart safe: locks survive it, tab
/// ids are re-allocated from scratch, and a recycled pid squatting on a stale lock would
/// otherwise silently wedge the action on that tab until the squatter exits. An unspawnable
/// probe counts as alive: the lock file is the primary signal, and this check exists only to
/// rescue provably stale locks. (`herdr_flash` matches the cargo test binary.)
fn lock_owner_is_alive(pid: u32) -> bool {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match output {
        // Non-success means no such process on both supported platforms.
        Ok(probe) if probe.status.success() => {
            let comm = String::from_utf8_lossy(&probe.stdout);
            comm.contains("herdr-flash") || comm.contains("herdr_flash")
        }
        Ok(_) => false,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Beyond pid_max on both supported platforms (Linux caps at 2^22, macOS at 99998).
    const DEAD_PID: &str = "99999999";

    #[test]
    fn live_lock_refuses_reentry_and_drop_clears_it() {
        let tab = "lock-test-live:tab";
        let lock = PickerTabLock::acquire(tab);

        assert!(tab_has_live_picker(tab));
        drop(lock);
        assert!(!tab_has_live_picker(tab));
        assert!(!lock_path(tab).exists());
    }

    #[test]
    fn dead_owner_marks_lock_stale_and_removes_it() {
        let tab = "lock-test-dead:tab";
        fs::write(lock_path(tab), DEAD_PID).unwrap();

        assert!(!tab_has_live_picker(tab));
        assert!(!lock_path(tab).exists());
    }

    #[test]
    fn pid_recycled_by_an_unrelated_process_marks_lock_stale() {
        // pid 1 (launchd/systemd) is always alive and never a herdr-flash binary: the exact
        // shape of a lock surviving a Herdr crash whose pid was recycled after restart.
        let tab = "lock-test-recycled:tab";
        fs::write(lock_path(tab), "1").unwrap();

        assert!(!tab_has_live_picker(tab));
        assert!(!lock_path(tab).exists());
    }

    #[test]
    fn unreadable_pid_marks_lock_stale() {
        let tab = "lock-test-garbage:tab";
        fs::write(lock_path(tab), "not a pid").unwrap();

        assert!(!tab_has_live_picker(tab));
        assert!(!lock_path(tab).exists());
    }

    #[test]
    fn missing_lock_means_no_live_picker() {
        assert!(!tab_has_live_picker("lock-test-absent:tab"));
    }

    #[test]
    fn tab_ids_map_to_distinct_filesystem_safe_paths() {
        let first = lock_path("w1:t1");
        let second = lock_path("w1:t2");
        assert_ne!(first, second);

        let name = first.file_name().unwrap().to_str().unwrap();
        assert!(name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "._-".contains(ch)));
    }
}
