use crate::cli_tui::session::TuiSessionIdentity;
use std::fmt;
use tokio::sync::mpsc;
use zeroize::Zeroize;

/// A profile credential (password / token) carried from the DiscoveryHub form
/// (B4) to the worker. The inner secret is zeroized on drop and redacted in
/// `Debug`, so it never lands in a log line, a status string, or a panic
/// payload. `Eq`/`PartialEq` compare the inner value so `WorkerCommand` stays
/// `Eq`-clean. The vault write happens in the worker; the TUI only holds it long
/// enough to send it across the channel.
#[derive(Clone, Default)]
pub struct TuiSecret(String);

impl TuiSecret {
    /// Expose the raw secret. Call sites are limited to the worker's vault write
    /// and the form's masked-length render; never log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.chars().count()
    }

    pub fn push(&mut self, c: char) {
        self.0.push(c);
    }

    pub fn pop(&mut self) {
        self.0.pop();
    }
}

impl fmt::Debug for TuiSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never render the secret; only whether one is present and its length.
        write!(f, "TuiSecret(len={})", self.0.chars().count())
    }
}

impl PartialEq for TuiSecret {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for TuiSecret {}

impl Drop for TuiSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// A profile draft built by the DiscoveryHub form (B4) and persisted by the
/// worker. Typed (not `serde_json::Value`) so `WorkerCommand` stays `Eq`-clean;
/// the worker turns it into the GUI `ServerProfile` JSON shape. An empty `id`
/// means "create" (the worker mints one); a non-empty `id` means "edit".
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct TuiProfileDraft {
    pub id: String,
    pub name: String,
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub initial_path: String,
    pub local_initial_path: String,
}

pub type WorkerCommandSender = mpsc::UnboundedSender<WorkerCommand>;
pub type WorkerCommandReceiver = mpsc::UnboundedReceiver<WorkerCommand>;
pub type WorkerEventSender = mpsc::UnboundedSender<WorkerEvent>;
pub type WorkerEventReceiver = mpsc::UnboundedReceiver<WorkerEvent>;

pub struct TuiWorkerClient {
    pub commands: WorkerCommandSender,
    pub events: WorkerEventReceiver,
}

pub fn worker_channels() -> (TuiWorkerClient, WorkerCommandReceiver, WorkerEventSender) {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();

    (
        TuiWorkerClient {
            commands: command_tx,
            events: event_rx,
        },
        command_rx,
        event_tx,
    )
}

/// Direction of a queued transfer.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TransferDirection {
    Download,
    Upload,
}

impl TransferDirection {
    pub fn label(self) -> &'static str {
        match self {
            TransferDirection::Download => "download",
            TransferDirection::Upload => "upload",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum WorkerCommand {
    OpenSession {
        identity: TuiSessionIdentity,
        initial_cwd: String,
    },
    List {
        path: String,
    },
    Stat {
        path: String,
    },
    Mkdir {
        path: String,
    },
    Remove {
        path: String,
        recursive: bool,
    },
    Rename {
        from: String,
        to: String,
    },
    Download {
        id: u64,
        remote_path: String,
        local_path: String,
    },
    Upload {
        id: u64,
        local_path: String,
        remote_path: String,
    },
    Cancel,
    /// Disconnect the live session and drop it (Esc -> back to the IntroHub).
    /// The TUI has already reset its session state optimistically; the worker
    /// closes the provider so the connection is not leaked.
    Disconnect,
    /// Remove the `.aerotmp` partial for a transfer dropped from the queue, so
    /// clearing a cancelled transfer also discards its resumable leftover.
    DiscardPartial {
        local_path: String,
    },
    // Phase 3 dual-pane: local filesystem operations (independent of remote session).
    LocalList {
        path: String,
    },
    LocalStat {
        path: String,
    },
    /// Create a directory on the LOCAL filesystem (dual-pane mkdir on the Local
    /// side). Distinct from `Mkdir`, which targets the remote provider.
    LocalMkdir {
        path: String,
    },
    /// Delete an entry on the LOCAL filesystem (dual-pane delete on the Local
    /// side). `recursive` removes a directory and its contents.
    LocalRemove {
        path: String,
        recursive: bool,
    },
    /// Rename/move an entry on the LOCAL filesystem (dual-pane rename on the
    /// Local side). Distinct from `Rename`, which targets the remote provider.
    LocalRename {
        from: String,
        to: String,
    },
    /// Persist the favorite flag for a saved profile (vault
    /// `config_favorite_servers`). Fire-and-forget: the IntroHub flips its own
    /// display state optimistically and asks the worker to persist.
    ToggleFavorite {
        profile_id: String,
    },
    /// Add/remove a saved profile to/from a named group (vault
    /// `config_server_groups`), creating the group when the name is new. The
    /// named generalisation of `ToggleFavorite` (#320); fire-and-forget, the
    /// IntroHub already updated its in-memory group state.
    ToggleGroupMembership {
        profile_id: String,
        group_name: String,
    },
    /// Rename a group across `config_server_groups`. Fire-and-forget.
    RenameGroup {
        old_name: String,
        new_name: String,
    },
    /// Delete a group (members untouched, only the grouping is removed) from
    /// `config_server_groups`. Fire-and-forget.
    DeleteGroup {
        name: String,
    },
    /// Delete a saved profile from the named user's partition (B4
    /// DiscoveryHub): drop the row by id, then prune the id from
    /// `config_server_groups` and `config_favorite_servers`, and best-effort
    /// delete its `server_<id>` credential. Optimistic: the IntroHub already
    /// removed the row. Fire-and-forget like the favourite/group flips.
    DeleteProfile {
        user_name: String,
        profile_id: String,
    },
    /// Create (empty `draft.id`) or edit (non-empty `draft.id`) a saved profile
    /// in the named user's partition (B4). When `secret` is `Some`, the password
    /// is dual-written under `server_<id>` (vault + user partition). Optimistic;
    /// fire-and-forget. The worker mints the id on create and echoes it back so
    /// the IntroHub can adopt it.
    SaveProfile {
        user_name: String,
        draft: TuiProfileDraft,
        secret: Option<TuiSecret>,
    },
    /// Probe reachability of a saved profile (DNS/TCP/TLS/HTTP) via the shared
    /// `server_health_check`, without opening a provider session. Reports back a
    /// `HealthReady` event the IntroHub renders as a status dot.
    HealthCheck {
        profile_id: String,
        host: String,
        port: u16,
        protocol: String,
        endpoint: Option<String>,
    },
    /// Refresh storage quota for a saved profile via a transient provider
    /// connection (the same `storage_info` path as `df`). Reports a
    /// `QuotaReady` event and persists the result to the bookmark `lastQuota`.
    RefreshQuota {
        identity: TuiSessionIdentity,
        profile_id: String,
    },
}

impl WorkerCommand {
    #[allow(dead_code)]
    pub fn operation(&self) -> TuiWorkerOperation {
        match self {
            WorkerCommand::OpenSession { .. } => TuiWorkerOperation::Connect,
            WorkerCommand::List { .. } => TuiWorkerOperation::List,
            WorkerCommand::Stat { .. } => TuiWorkerOperation::Stat,
            WorkerCommand::Mkdir { .. } => TuiWorkerOperation::Mkdir,
            WorkerCommand::Remove { .. } => TuiWorkerOperation::Remove,
            WorkerCommand::Rename { .. } => TuiWorkerOperation::Rename,
            WorkerCommand::Download { .. } | WorkerCommand::Upload { .. } => {
                TuiWorkerOperation::Transfer
            }
            WorkerCommand::Cancel => TuiWorkerOperation::Cancel,
            WorkerCommand::Disconnect => TuiWorkerOperation::Connect,
            WorkerCommand::DiscardPartial { .. } => TuiWorkerOperation::Discard,
            WorkerCommand::LocalList { .. } | WorkerCommand::LocalStat { .. } => {
                TuiWorkerOperation::List
            }
            WorkerCommand::LocalMkdir { .. } => TuiWorkerOperation::Mkdir,
            WorkerCommand::LocalRemove { .. } => TuiWorkerOperation::Remove,
            WorkerCommand::LocalRename { .. } => TuiWorkerOperation::Rename,
            WorkerCommand::ToggleFavorite { .. } => TuiWorkerOperation::Favorite,
            WorkerCommand::ToggleGroupMembership { .. }
            | WorkerCommand::RenameGroup { .. }
            | WorkerCommand::DeleteGroup { .. } => TuiWorkerOperation::Group,
            WorkerCommand::DeleteProfile { .. } | WorkerCommand::SaveProfile { .. } => {
                TuiWorkerOperation::Profile
            }
            WorkerCommand::HealthCheck { .. } => TuiWorkerOperation::Health,
            WorkerCommand::RefreshQuota { .. } => TuiWorkerOperation::Quota,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TuiWorkerOperation {
    Connect,
    List,
    Stat,
    Mkdir,
    Remove,
    Rename,
    Transfer,
    Cancel,
    Discard,
    Favorite,
    Group,
    Profile,
    Health,
    Quota,
}

impl TuiWorkerOperation {
    pub fn label(self) -> &'static str {
        match self {
            TuiWorkerOperation::Connect => "connect",
            TuiWorkerOperation::List => "list",
            TuiWorkerOperation::Stat => "stat",
            TuiWorkerOperation::Mkdir => "mkdir",
            TuiWorkerOperation::Remove => "remove",
            TuiWorkerOperation::Rename => "rename",
            TuiWorkerOperation::Transfer => "transfer",
            TuiWorkerOperation::Cancel => "cancel",
            TuiWorkerOperation::Discard => "discard",
            TuiWorkerOperation::Favorite => "favorite",
            TuiWorkerOperation::Group => "group",
            TuiWorkerOperation::Profile => "profile",
            TuiWorkerOperation::Health => "health",
            TuiWorkerOperation::Quota => "quota",
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum WorkerEvent {
    Idle,
    Busy {
        operation: TuiWorkerOperation,
        identity: Option<TuiSessionIdentity>,
    },
    SessionReady {
        identity: Option<TuiSessionIdentity>,
        cwd: String,
    },
    PathReady {
        operation: TuiWorkerOperation,
        path: String,
    },
    TransferProgress {
        id: u64,
        transferred: u64,
        total: u64,
    },
    TransferDone {
        id: u64,
        message: String,
    },
    TransferFailed {
        id: u64,
        message: String,
    },
    /// A transfer was aborted by the user. The partial `.aerotmp` is kept on
    /// disk by the provider so a later re-download resumes from the offset.
    TransferCancelled {
        id: u64,
    },
    ListReady {
        identity: Option<TuiSessionIdentity>,
        path: String,
        result: TuiListResult,
    },
    StatReady {
        identity: Option<TuiSessionIdentity>,
        path: String,
        result: TuiStatResult,
    },
    Failed {
        operation: TuiWorkerOperation,
        identity: Option<TuiSessionIdentity>,
        message: String,
    },
    Cancelled {
        operation: TuiWorkerOperation,
    },
    /// Result of a [`WorkerCommand::HealthCheck`] probe for a saved profile.
    HealthReady {
        profile_id: String,
        status: String,
        score: u8,
        latency_ms: Option<u32>,
    },
    /// Result of a [`WorkerCommand::RefreshQuota`] probe for a saved profile.
    QuotaReady {
        profile_id: String,
        used: u64,
        total: u64,
    },
    /// A [`WorkerCommand::RefreshQuota`] that could not reach the provider.
    QuotaFailed {
        profile_id: String,
        message: String,
    },
}

impl WorkerEvent {
    pub fn label(&self) -> String {
        match self {
            WorkerEvent::Idle => "idle".to_string(),
            WorkerEvent::Busy { operation, .. } => format!("{} busy", operation.label()),
            WorkerEvent::SessionReady { cwd, .. } => format!("session ready {}", cwd),
            WorkerEvent::PathReady { operation, path } => {
                format!("{} ready {}", operation.label(), path)
            }
            WorkerEvent::ListReady { path, result, .. } => {
                format!("list ready {} ({} items)", path, result.summary.total)
            }
            WorkerEvent::StatReady { path, .. } => format!("stat ready {}", path),
            WorkerEvent::TransferProgress {
                id,
                transferred,
                total,
            } => format!("transfer #{} {}/{}", id, transferred, total),
            WorkerEvent::TransferDone { id, .. } => format!("transfer #{} done", id),
            WorkerEvent::TransferFailed { id, .. } => format!("transfer #{} failed", id),
            WorkerEvent::TransferCancelled { id } => format!("transfer #{} cancelled", id),
            WorkerEvent::Failed { operation, .. } => format!("{} failed", operation.label()),
            WorkerEvent::Cancelled { operation } => format!("{} cancelled", operation.label()),
            WorkerEvent::HealthReady {
                profile_id, status, ..
            } => format!("health {} {}", profile_id, status),
            WorkerEvent::QuotaReady {
                profile_id,
                used,
                total,
            } => format!("quota {} {}/{}", profile_id, used, total),
            WorkerEvent::QuotaFailed { profile_id, .. } => format!("quota {} failed", profile_id),
        }
    }
}

/// Terminal outcome of driving a transfer future via [`drive_tui_transfer`].
#[derive(Debug, Eq, PartialEq)]
pub enum TransferDrive {
    /// The transfer future resolved successfully.
    Completed,
    /// The transfer future resolved with an error message.
    Failed(String),
    /// A `Cancel` command arrived; the future is dropped (aborted) on return.
    Cancelled,
    /// The command channel closed (the UI went away) while transferring.
    ChannelClosed,
}

/// Drive a transfer future while keeping the worker responsive to `Cancel`.
///
/// The transfer borrows the live session mutably, so on a single connection no
/// other provider call can run concurrently. Instead, non-cancel commands that
/// arrive mid-transfer are buffered onto `pending` and replayed by the worker
/// loop, in arrival order, once the transfer settles. A `Cancel` returns
/// [`TransferDrive::Cancelled`]; dropping the awaited future on return is the
/// idiomatic async abort and propagates to the in-flight provider read/write at
/// its next await point. A closed channel (UI gone) aborts the same way.
pub async fn drive_tui_transfer<F>(
    transfer: F,
    command_rx: &mut WorkerCommandReceiver,
    pending: &mut std::collections::VecDeque<WorkerCommand>,
) -> TransferDrive
where
    F: std::future::Future<Output = Result<(), String>>,
{
    tokio::pin!(transfer);
    loop {
        tokio::select! {
            result = &mut transfer => {
                return match result {
                    Ok(()) => TransferDrive::Completed,
                    Err(message) => TransferDrive::Failed(message),
                };
            }
            maybe_command = command_rx.recv() => {
                match maybe_command {
                    Some(WorkerCommand::Cancel) => return TransferDrive::Cancelled,
                    Some(other) => pending.push_back(other),
                    None => return TransferDrive::ChannelClosed,
                }
            }
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TuiListResult {
    pub entries: Vec<TuiListEntry>,
    pub summary: TuiListSummary,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TuiListEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TuiListSummary {
    pub total: usize,
    pub files: usize,
    pub dirs: usize,
    pub total_bytes: u64,
    pub truncated: bool,
    pub total_before_limit: usize,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TuiStatResult {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<String>,
    pub permissions: Option<String>,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub is_symlink: bool,
    pub link_target: Option<String>,
    pub mime_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> TuiSessionIdentity {
        TuiSessionIdentity {
            user_name: "default".to_string(),
            profile_selector: "1".to_string(),
            profile_name: "Production".to_string(),
            protocol: "sftp".to_string(),
            host: "example.com".to_string(),
        }
    }

    #[test]
    fn worker_commands_map_to_single_operation_names() {
        let cases = [
            (
                WorkerCommand::OpenSession {
                    identity: identity(),
                    initial_cwd: "/".to_string(),
                },
                TuiWorkerOperation::Connect,
            ),
            (
                WorkerCommand::List {
                    path: "/".to_string(),
                },
                TuiWorkerOperation::List,
            ),
            (
                WorkerCommand::Stat {
                    path: "/file.txt".to_string(),
                },
                TuiWorkerOperation::Stat,
            ),
            (
                WorkerCommand::Mkdir {
                    path: "/new".to_string(),
                },
                TuiWorkerOperation::Mkdir,
            ),
            (
                WorkerCommand::Remove {
                    path: "/old".to_string(),
                    recursive: false,
                },
                TuiWorkerOperation::Remove,
            ),
            (
                WorkerCommand::Rename {
                    from: "/old".to_string(),
                    to: "/new".to_string(),
                },
                TuiWorkerOperation::Rename,
            ),
            (WorkerCommand::Cancel, TuiWorkerOperation::Cancel),
        ];

        for (command, expected) in cases {
            assert_eq!(command.operation(), expected);
        }
    }

    #[test]
    fn group_commands_share_the_group_operation() {
        let cases = [
            WorkerCommand::ToggleGroupMembership {
                profile_id: "srv-1".to_string(),
                group_name: "Production".to_string(),
            },
            WorkerCommand::RenameGroup {
                old_name: "Production".to_string(),
                new_name: "Prod".to_string(),
            },
            WorkerCommand::DeleteGroup {
                name: "Production".to_string(),
            },
        ];
        for command in cases {
            assert_eq!(command.operation(), TuiWorkerOperation::Group);
            assert_eq!(command.operation().label(), "group");
        }
    }

    #[test]
    fn worker_event_labels_are_short_status_strings() {
        assert_eq!(WorkerEvent::Idle.label(), "idle");
        assert_eq!(
            WorkerEvent::Busy {
                operation: TuiWorkerOperation::List,
                identity: Some(identity()),
            }
            .label(),
            "list busy"
        );
        assert_eq!(
            WorkerEvent::PathReady {
                operation: TuiWorkerOperation::Stat,
                path: "/file.txt".to_string()
            }
            .label(),
            "stat ready /file.txt"
        );
        assert_eq!(
            WorkerEvent::ListReady {
                identity: Some(identity()),
                path: "/".to_string(),
                result: TuiListResult {
                    entries: Vec::new(),
                    summary: TuiListSummary {
                        total: 0,
                        files: 0,
                        dirs: 0,
                        total_bytes: 0,
                        truncated: false,
                        total_before_limit: 0,
                    },
                },
            }
            .label(),
            "list ready / (0 items)"
        );
        assert_eq!(
            WorkerEvent::StatReady {
                identity: Some(identity()),
                path: "/file.txt".to_string(),
                result: TuiStatResult {
                    name: "file.txt".to_string(),
                    path: "/file.txt".to_string(),
                    is_dir: false,
                    size: 42,
                    modified: None,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: None,
                },
            }
            .label(),
            "stat ready /file.txt"
        );
    }

    #[tokio::test]
    async fn drive_transfer_reports_completion() {
        let (_tx, mut rx) = mpsc::unbounded_channel::<WorkerCommand>();
        let mut pending = std::collections::VecDeque::new();
        let outcome = drive_tui_transfer(async { Ok(()) }, &mut rx, &mut pending).await;
        assert_eq!(outcome, TransferDrive::Completed);
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn drive_transfer_reports_failure_message() {
        let (_tx, mut rx) = mpsc::unbounded_channel::<WorkerCommand>();
        let mut pending = std::collections::VecDeque::new();
        let outcome =
            drive_tui_transfer(async { Err("boom".to_string()) }, &mut rx, &mut pending).await;
        assert_eq!(outcome, TransferDrive::Failed("boom".to_string()));
    }

    #[tokio::test]
    async fn drive_transfer_aborts_on_cancel_and_buffers_other_commands() {
        // A command that lands mid-transfer must be buffered (not lost), and a
        // Cancel must abort. The transfer future never resolves on its own, so
        // the only way out is the command channel.
        let (tx, mut rx) = mpsc::unbounded_channel::<WorkerCommand>();
        tx.send(WorkerCommand::List {
            path: "/srv".to_string(),
        })
        .unwrap();
        tx.send(WorkerCommand::Cancel).unwrap();
        let mut pending = std::collections::VecDeque::new();
        let never = std::future::pending::<Result<(), String>>();
        let outcome = drive_tui_transfer(never, &mut rx, &mut pending).await;
        assert_eq!(outcome, TransferDrive::Cancelled);
        assert_eq!(
            pending.pop_front(),
            Some(WorkerCommand::List {
                path: "/srv".to_string()
            })
        );
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn drive_transfer_aborts_when_channel_closes() {
        let (tx, mut rx) = mpsc::unbounded_channel::<WorkerCommand>();
        drop(tx);
        let mut pending = std::collections::VecDeque::new();
        let never = std::future::pending::<Result<(), String>>();
        let outcome = drive_tui_transfer(never, &mut rx, &mut pending).await;
        assert_eq!(outcome, TransferDrive::ChannelClosed);
    }
}
