//! Bounded AeroAgent requests to the main window's semantic GUI controller.
//! This broker grants no approval, exposes no IPC listener and accepts no writes.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager};
use tokio::sync::oneshot;

const MAX_PENDING: usize = 32;
const MAX_REPLY_BYTES: usize = 256 * 1024;
const INTENTS: &[&str] = &[
    "state",
    "wait",
    "show_view",
    "navigate",
    "refresh",
    "select",
    "connect",
    "disconnect",
    "stop",
];

#[derive(Clone, Copy, PartialEq, Eq)]
struct Scope {
    user: Option<i64>,
    unlocked: bool,
}

async fn scope(app: &tauri::AppHandle) -> Result<Scope, String> {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if crate::credential_store::CredentialStore::from_cache().is_none() {
            return Ok(Scope {
                user: None,
                unlocked: false,
            });
        }
        let conn = crate::user_partitions::open_or_init(&app)?;
        let status = crate::user_partitions::user_unlock_status(&conn)?;
        Ok(Scope {
            user: status.active_user_id,
            unlocked: status.is_unlocked,
        })
    })
    .await
    .map_err(|_| "gui_scope_unavailable".to_string())?
}

struct Pending {
    deadline: Instant,
    scope: Scope,
    claimed: bool,
    sender: oneshot::Sender<Reply>,
}
static PENDING: LazyLock<Mutex<HashMap<String, Pending>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
fn pending() -> std::sync::MutexGuard<'static, HashMap<String, Pending>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Session {
    id: String,
    name: String,
    protocol: String,
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    saved_profile_id: Option<String>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Panel {
    path: String,
    loading: bool,
    selection: Vec<String>,
    selection_count: usize,
    entries_count: usize,
}
#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
struct Panels {
    #[serde(skip_serializing_if = "Option::is_none")]
    remote: Option<Panel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    local: Option<Panel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    local2: Option<Panel>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Queue {
    active: usize,
    pending: usize,
    failed: usize,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema_version: u8,
    state_revision: u64,
    version: String,
    locked: bool,
    blocked: bool,
    view: String,
    connected: bool,
    active_session_id: Option<String>,
    sessions: Vec<Session>,
    panels: Panels,
    queue: Queue,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    ok: bool,
    error: Option<String>,
    snapshot: Snapshot,
}

fn parse_reply(payload: Value, unlocked: bool) -> Result<Reply, String> {
    if serde_json::to_vec(&payload)
        .map_err(|_| "gui_invalid_reply")?
        .len()
        > MAX_REPLY_BYTES
    {
        return Err("gui_reply_too_large".into());
    }
    let reply: Reply = serde_json::from_value(payload).map_err(|_| "gui_invalid_reply")?;
    let s = &reply.snapshot;
    if s.schema_version != 1
        || s.sessions.len() > 32
        || !["servers", "files", "other"].contains(&s.view.as_str())
    {
        return Err("gui_invalid_reply".into());
    }
    if reply.ok != reply.error.is_none()
        || reply.error.as_deref().is_some_and(|e| {
            ![
                "unsupported_intent",
                "invalid_args",
                "locked",
                "busy",
                "blocked",
                "stale_state",
                "lease_interrupted",
                "gui_timeout",
                "action_failed",
                "not_connected",
                "pending_human",
            ]
            .contains(&e)
        })
    {
        return Err("gui_invalid_reply".into());
    }
    for panel in [&s.panels.remote, &s.panels.local, &s.panels.local2]
        .into_iter()
        .flatten()
    {
        if panel.path.len() > 16_384
            || panel.selection.len() > 100
            || panel.selection_count < panel.selection.len()
            || panel.selection.iter().any(|n| n.len() > 16_384)
        {
            return Err("gui_invalid_reply".into());
        }
    }
    if (!unlocked || s.locked)
        && (!s.locked
            || s.connected
            || s.active_session_id.is_some()
            || !s.sessions.is_empty()
            || s.panels.remote.is_some()
            || s.panels.local.is_some()
            || s.panels.local2.is_some()
            || s.queue.active != 0
            || s.queue.pending != 0
            || s.queue.failed != 0)
    {
        return Err("gui_locked_reply".into());
    }
    Ok(reply)
}

fn check_window(label: &str) -> Result<(), String> {
    if label == "main" {
        Ok(())
    } else {
        Err("gui_wrong_window".into())
    }
}
fn check_pending(entry: &Pending, current: Scope) -> Result<(), String> {
    if Instant::now() >= entry.deadline {
        return Err("gui_timeout".into());
    }
    if entry.scope != current {
        return Err("gui_scope_changed".into());
    }
    Ok(())
}
fn claim_entry(entry: &mut Pending, current: Scope) -> Result<u64, String> {
    check_pending(entry, current)?;
    if entry.claimed {
        return Err("gui_already_claimed".into());
    }
    entry.claimed = true;
    Ok(entry
        .deadline
        .saturating_duration_since(Instant::now())
        .as_millis() as u64)
}
fn consume_entry(
    map: &mut HashMap<String, Pending>,
    id: &str,
    current: Scope,
) -> Result<Pending, String> {
    let entry = map.get(id).ok_or("gui_unknown_request")?;
    check_pending(entry, current)?;
    if !entry.claimed {
        return Err("gui_not_claimed".into());
    }
    map.remove(id)
        .ok_or_else(|| "gui_unknown_request".to_string())
}

struct RequestGuard {
    id: String,
    app: tauri::AppHandle,
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        if pending().remove(&self.id).is_some() {
            let _ = self
                .app
                .emit_to("main", "gui-intent-cancel", json!({ "id": self.id }));
        }
    }
}

/// Sends one catalogued operation to the running main window; never falls back to headless work.
pub(crate) async fn request_intent(
    app: &tauri::AppHandle,
    name: &str,
    args: Value,
    timeout_ms: u64,
    if_revision: Option<u64>,
) -> Result<Value, String> {
    if !INTENTS.contains(&name) {
        return Err("unsupported_intent".into());
    }
    if !args.is_object()
        || serde_json::to_vec(&args).map_err(|_| "invalid_args")?.len() > 64 * 1024
        || !(100..=30_000).contains(&timeout_ms)
    {
        return Err("invalid_args".into());
    }
    if app.get_webview_window("main").is_none() {
        return Err("gui_unavailable".into());
    }
    let current = scope(app).await?;
    if !current.unlocked && !["state", "wait", "stop"].contains(&name) {
        return Err("locked".into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let (sender, receiver) = oneshot::channel();
    {
        let mut map = pending();
        if map.len() >= MAX_PENDING {
            return Err("busy".into());
        }
        map.insert(
            id.clone(),
            Pending {
                deadline: Instant::now() + Duration::from_millis(timeout_ms),
                scope: current,
                claimed: false,
                sender,
            },
        );
    }
    let _guard = RequestGuard {
        id: id.clone(),
        app: app.clone(),
    };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "gui_clock_error")?
        .as_millis() as u64;
    let mut request =
        json!({ "name": name, "args": args, "timeout_ms": timeout_ms, "pace": "watch" });
    if let Some(revision) = if_revision {
        request["if_revision"] = json!(revision);
    }
    app.emit_to(
        "main",
        "gui-intent",
        json!({ "id": id, "expires_at": now_ms + timeout_ms, "request": request }),
    )
    .map_err(|_| "gui_unavailable")?;
    let reply = tokio::time::timeout(Duration::from_millis(timeout_ms), receiver)
        .await
        .map_err(|_| "gui_timeout")?
        .map_err(|_| "gui_cancelled")?;
    serde_json::to_value(reply).map_err(|_| "gui_invalid_reply".into())
}

/// Claims an unexpired request once, in its original account and intended window.
#[tauri::command]
pub async fn gui_intent_claim(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    id: String,
) -> Result<u64, String> {
    check_window(window.label())?;
    let current = scope(&app).await?;
    let mut map = pending();
    claim_entry(map.get_mut(&id).ok_or("gui_unknown_request")?, current)
}

/// Only the main window can answer a request it claimed. This cannot grant approval.
#[tauri::command]
pub async fn gui_intent_result(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    id: String,
    payload: Value,
) -> Result<(), String> {
    check_window(window.label())?;
    let current = scope(&app).await?;
    let reply = parse_reply(payload, current.unlocked)?;
    let entry = consume_entry(&mut pending(), &id, current)?;
    entry.sender.send(reply).map_err(|_| "gui_cancelled".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn locked_reply() -> Value {
        json!({ "ok": true, "error": null, "snapshot": { "schema_version": 1, "state_revision": 1,
            "version": "test", "locked": true, "blocked": true, "view": "other", "connected": false,
            "active_session_id": null, "sessions": [], "panels": {}, "queue": { "active": 0, "pending": 0, "failed": 0 } } })
    }
    #[test]
    fn gui_controller_replies_reject_secrets_and_locked_data() {
        assert!(parse_reply(locked_reply(), false).is_ok());
        let mut value = locked_reply();
        value["snapshot"]["password"] = json!("SECRET");
        assert!(parse_reply(value, true).is_err());
        let mut value = locked_reply();
        value["snapshot"]["connected"] = json!(true);
        assert!(parse_reply(value, false).is_err());
        let mut value = locked_reply();
        value["error"] = json!("SENTINEL_PASSWORD");
        value["ok"] = json!(false);
        assert!(parse_reply(value, true).is_err());
    }
    #[test]
    fn gui_controller_connect_metadata_and_human_handoff_are_closed() {
        let mut value = locked_reply();
        value["ok"] = json!(false);
        value["error"] = json!("pending_human");
        value["snapshot"]["locked"] = json!(false);
        value["snapshot"]["sessions"] = json!([{
            "id": "session", "name": "Fixture", "protocol": "ftp",
            "status": "connected", "saved_profile_id": "profile"
        }]);
        let reply = parse_reply(value.clone(), true).unwrap();
        assert_eq!(
            reply.snapshot.sessions[0].saved_profile_id.as_deref(),
            Some("profile")
        );
        value["snapshot"]["sessions"][0]["password"] = json!("SECRET");
        assert!(parse_reply(value, true).is_err());
    }
    #[test]
    fn gui_controller_window_and_expiry_are_bound() {
        assert!(check_window("main").is_ok());
        for label in ["ai-approval-123", "preview", "main-forged", ""] {
            assert!(check_window(label).is_err());
        }
        let (sender, _) = oneshot::channel();
        let original = Scope {
            user: Some(1),
            unlocked: true,
        };
        let mut entry = Pending {
            deadline: Instant::now() + Duration::from_secs(1),
            scope: original,
            claimed: false,
            sender,
        };
        assert!(check_pending(&entry, original).is_ok());
        assert!(check_pending(
            &entry,
            Scope {
                user: Some(2),
                unlocked: true
            }
        )
        .is_err());
        entry.deadline = Instant::now();
        assert!(check_pending(&entry, original).is_err());
    }
    #[tokio::test]
    async fn gui_controller_claims_and_replies_are_single_use() {
        let mut map = HashMap::new();
        let (sender, receiver) = oneshot::channel();
        let original = Scope {
            user: Some(1),
            unlocked: true,
        };
        map.insert(
            "issued".into(),
            Pending {
                deadline: Instant::now() + Duration::from_secs(2),
                scope: original,
                claimed: false,
                sender,
            },
        );
        assert!(consume_entry(&mut map, "issued", original).is_err());
        assert!(claim_entry(map.get_mut("issued").unwrap(), original).is_ok());
        assert!(claim_entry(map.get_mut("issued").unwrap(), original).is_err());
        assert!(consume_entry(
            &mut map,
            "issued",
            Scope {
                user: Some(2),
                unlocked: true
            }
        )
        .is_err());
        let entry = consume_entry(&mut map, "issued", original).unwrap();
        entry
            .sender
            .send(parse_reply(locked_reply(), true).unwrap())
            .unwrap();
        assert!(receiver.await.unwrap().ok);
        assert!(consume_entry(&mut map, "issued", original).is_err());
        assert!(consume_entry(&mut map, "never-issued", original).is_err());
    }
}
