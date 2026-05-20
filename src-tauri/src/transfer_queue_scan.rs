// TQ-2: lazy per-level remote scanner that feeds the Transfer Queue panel.
//
// This walker is intentionally separate from `scan_ftp_download_entries`
// (lib.rs:4569). That function is the regression oracle for the legacy
// folder-download path and mixes descent with side effects (mkdir, skip
// decisions, transfer events). The Transfer Queue panel needs a pure
// list-only walk so the user can prune children before the real transfer
// starts; mixing the two would either bloat the existing function or risk
// changing its byte-identical output. UX spec: see APPENDIX-TRANSFER-QUEUE
// `tasks/TQ-1_UX-spec.md` section 4.

use crate::AppState;
use crate::ftp::{FtpManager, RemoteFile};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::State;
use tracing::{debug, warn};

/// Single entry returned by the lazy-per-level remote scanner. `depth` is
/// relative to the scan root: depth=0 means the entry is a direct child of
/// the requested `path`.
#[derive(Debug, Clone, Serialize)]
pub struct ScanNode {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
    pub modified: Option<String>,
    pub depth: u32,
    pub parent_path: String,
}

/// Per-level truncation marker. The UI renders a "+N more (omitted)"
/// pseudo-row at each truncated level (UX spec section 4).
#[derive(Debug, Clone, Serialize)]
pub struct TruncatedLevel {
    pub parent_path: String,
    pub kept: u32,
    pub omitted: u32,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ScanTreeResult {
    pub root_path: String,
    pub nodes: Vec<ScanNode>,
    pub truncated_parents: Vec<TruncatedLevel>,
    pub total_visited: u32,
    pub cancelled: bool,
}

/// Default expand: one level under `path` (lazy-per-level, UX spec).
pub const DEFAULT_DEPTH: u32 = 1;
/// Hard depth cap mirrors the CLI sync defaults; protects against runaway
/// callers that pass an unbounded depth.
pub const HARD_DEPTH_CAP: u32 = 10;
/// Hard entries-per-level cap, mirrors the CLI sync defaults.
pub const HARD_ENTRIES_PER_LEVEL: usize = 5_000;
/// Defensive total-nodes guard. A deep tree may respect the per-level cap
/// but still produce a massive flat list; this cap keeps the panel
/// responsive on pathological cases.
pub const HARD_TOTAL_NODES: usize = 50_000;

fn clamp_depth(requested: Option<u32>) -> u32 {
    requested.unwrap_or(DEFAULT_DEPTH).clamp(1, HARD_DEPTH_CAP)
}

fn clamp_entry_cap(requested: Option<usize>) -> usize {
    requested
        .unwrap_or(HARD_ENTRIES_PER_LEVEL)
        .clamp(1, HARD_ENTRIES_PER_LEVEL)
}

fn join_remote(parent: &str, name: &str) -> String {
    let name_trimmed = name.trim_start_matches('/');
    if parent == "/" {
        format!("/{}", name_trimmed)
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), name_trimmed)
    }
}

/// Walk an FTP/SFTP subtree breadth-first, bounded by `max_depth` and
/// `entry_cap_per_level`. Reuses the live `FtpManager` connection, so the
/// caller must hold the mutex for the duration. Honours `cancel_flag`
/// between levels (same pattern as `scan_ftp_download_entries`).
///
/// This is a list-only walker: it never touches the local filesystem and
/// never emits transfer events. It exists to feed the Transfer Queue panel
/// (TQ-2) which surfaces the remote tree for the user to prune before the
/// real transfer starts.
pub async fn walk_remote_ftp(
    ftp_manager: &mut FtpManager,
    cancel_flag: &Arc<AtomicBool>,
    root_path: &str,
    max_depth: u32,
    entry_cap_per_level: usize,
) -> ScanTreeResult {
    let mut result = ScanTreeResult {
        root_path: root_path.to_string(),
        ..Default::default()
    };

    // Capture the current CWD so the scan does not perturb other in-flight
    // operations that share the same connection. Restoration is best-effort
    // on exit: if it fails the next user-driven operation will navigate
    // explicitly anyway.
    let original_path = ftp_manager.current_path();

    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    queue.push_back((root_path.to_string(), 0));

    while let Some((current_dir, current_depth)) = queue.pop_front() {
        if cancel_flag.load(Ordering::Relaxed) {
            result.cancelled = true;
            break;
        }
        if (result.total_visited as usize) >= HARD_TOTAL_NODES {
            warn!(
                "transfer_queue_scan: HARD_TOTAL_NODES ({}) reached at {}",
                HARD_TOTAL_NODES, current_dir
            );
            result.truncated_parents.push(TruncatedLevel {
                parent_path: current_dir.clone(),
                kept: 0,
                omitted: 0,
            });
            break;
        }

        if let Err(e) = ftp_manager.change_dir(&current_dir).await {
            warn!(
                "transfer_queue_scan: cannot enter {}: {}",
                current_dir, e
            );
            continue;
        }

        let files: Vec<RemoteFile> = match ftp_manager.list_files().await {
            Ok(f) => f,
            Err(e) => {
                warn!(
                    "transfer_queue_scan: cannot list {}: {}",
                    current_dir, e
                );
                continue;
            }
        };

        let total = files.len();
        let cap = entry_cap_per_level.min(total);
        if total > entry_cap_per_level {
            result.truncated_parents.push(TruncatedLevel {
                parent_path: current_dir.clone(),
                kept: cap as u32,
                omitted: (total - cap) as u32,
            });
        }

        for file in files.into_iter().take(cap) {
            let path = join_remote(&current_dir, &file.name);
            let is_dir = file.is_dir;
            let size = file.size.unwrap_or(0);
            result.nodes.push(ScanNode {
                name: file.name.clone(),
                path: path.clone(),
                size,
                is_dir,
                modified: file.modified,
                depth: current_depth,
                parent_path: current_dir.clone(),
            });
            result.total_visited += 1;

            if is_dir && current_depth + 1 < max_depth {
                queue.push_back((path, current_depth + 1));
            }
        }
    }

    if !original_path.is_empty() {
        let _ = ftp_manager.change_dir(&original_path).await;
    }

    result
}

/// Tauri command: lazy per-level scan of a remote subtree for the
/// Transfer Queue panel. `path` is required (no implicit root). `depth`
/// defaults to 1 (one level under `path`). `entry_cap` defaults to
/// `HARD_ENTRIES_PER_LEVEL`. Both are clamped to the hard caps.
#[tauri::command]
pub async fn transfer_queue_scan_remote_tree(
    state: State<'_, AppState>,
    path: String,
    depth: Option<u32>,
    entry_cap: Option<usize>,
) -> Result<ScanTreeResult, String> {
    if path.is_empty() {
        return Err("path is required".to_string());
    }
    let max_depth = clamp_depth(depth);
    let cap = clamp_entry_cap(entry_cap);
    debug!(
        "transfer_queue_scan_remote_tree path={} depth={} entry_cap={}",
        path, max_depth, cap
    );

    let mut ftp_manager = state.ftp_manager.lock().await;
    Ok(walk_remote_ftp(&mut ftp_manager, &state.cancel_flag, &path, max_depth, cap).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_depth_defaults_to_1() {
        assert_eq!(clamp_depth(None), DEFAULT_DEPTH);
    }
    #[test]
    fn clamp_depth_caps_at_hard_cap() {
        assert_eq!(clamp_depth(Some(99)), HARD_DEPTH_CAP);
    }
    #[test]
    fn clamp_depth_floor_at_1() {
        assert_eq!(clamp_depth(Some(0)), 1);
    }
    #[test]
    fn clamp_entry_cap_defaults_to_hard_cap() {
        assert_eq!(clamp_entry_cap(None), HARD_ENTRIES_PER_LEVEL);
    }
    #[test]
    fn clamp_entry_cap_caps_at_hard_cap() {
        assert_eq!(clamp_entry_cap(Some(100_000)), HARD_ENTRIES_PER_LEVEL);
    }
    #[test]
    fn clamp_entry_cap_floors_at_1() {
        assert_eq!(clamp_entry_cap(Some(0)), 1);
    }
    #[test]
    fn join_remote_root_parent() {
        assert_eq!(join_remote("/", "foo"), "/foo");
    }
    #[test]
    fn join_remote_strips_trailing_slash() {
        assert_eq!(join_remote("/dir/", "foo"), "/dir/foo");
    }
    #[test]
    fn join_remote_strips_leading_slash_on_child() {
        assert_eq!(join_remote("/dir", "/foo"), "/dir/foo");
    }
    #[test]
    fn join_remote_nested() {
        assert_eq!(join_remote("/a/b", "c"), "/a/b/c");
    }
}
