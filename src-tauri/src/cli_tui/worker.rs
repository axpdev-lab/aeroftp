use crate::cli_tui::session::TuiSessionIdentity;
use tokio::sync::mpsc;

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
        identity: TuiSessionIdentity,
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
    ListReady {
        identity: TuiSessionIdentity,
        path: String,
        result: TuiListResult,
    },
    StatReady {
        identity: TuiSessionIdentity,
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
            WorkerEvent::Failed { operation, .. } => format!("{} failed", operation.label()),
            WorkerEvent::Cancelled { operation } => format!("{} cancelled", operation.label()),
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
                identity: identity(),
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
                identity: identity(),
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
}
