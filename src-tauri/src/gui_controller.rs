//! Bounded AeroAgent requests to the main window's semantic GUI controller.
//! This broker grants no approval or external listener. Claimed approved Settings
//! requests may commit only their original, validated public preference delta.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager};
use tokio::sync::oneshot;

const MAX_PENDING: usize = 32;
const MAX_REPLY_BYTES: usize = 256 * 1024;
const MAX_SETTINGS_KEYS: usize = 40;
const MAX_SETTINGS_PROVIDERS: usize = 32;
const MAX_SETTINGS_MODELS: usize = 64;
const MAX_SETTINGS_STRING: usize = 256;
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
    "settings_open",
    "settings_read",
    "settings_update",
    "settings_close",
    "tools_open",
    "tools_read",
    "tools_close",
];

#[derive(Clone, Copy, PartialEq, Eq)]
struct Scope {
    user: Option<i64>,
    unlocked: bool,
    vault_generation: u64,
    partition_generation: u64,
}

async fn scope(app: &tauri::AppHandle) -> Result<Scope, String> {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        if crate::credential_store::CredentialStore::from_cache().is_none() {
            return Ok(Scope {
                user: None,
                unlocked: false,
                vault_generation: crate::credential_store::CredentialStore::cache_generation(),
                partition_generation: crate::user_partitions::session_generation(),
            });
        }
        let conn = crate::user_partitions::open_or_init(&app)?;
        let status = crate::user_partitions::user_unlock_status(&conn)?;
        Ok(Scope {
            user: status.active_user_id,
            unlocked: status.is_unlocked,
            vault_generation: crate::credential_store::CredentialStore::cache_generation(),
            partition_generation: crate::user_partitions::session_generation(),
        })
    })
    .await
    .map_err(|_| "gui_scope_unavailable".to_string())?
}

/// Public display metadata is separate from the stable caller identity.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct GuiActor {
    pub(crate) id: String,
    pub(crate) kind: &'static str,
    pub(crate) label: String,
}
impl GuiActor {
    fn aeroagent(session_id: Option<&str>) -> Self {
        use sha2::{Digest, Sha256};
        // A caller without a session must never inherit another caller's idle lease.
        let fallback = uuid::Uuid::new_v4().to_string();
        let session = session_id.filter(|s| !s.is_empty()).unwrap_or(&fallback);
        Self {
            id: format!("aeroagent:{:x}", Sha256::digest(session.as_bytes())),
            kind: "aeroagent",
            label: "AeroAgent".into(),
        }
    }
}
#[cfg(debug_assertions)]
mod dev_sessions {
    use super::*;
    struct DevSession {
        scope: Scope,
        actor: GuiActor,
        expires: Instant,
    }
    static SESSIONS: LazyLock<Mutex<HashMap<String, DevSession>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    fn sessions() -> std::sync::MutexGuard<'static, HashMap<String, DevSession>> {
        SESSIONS.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn prune(map: &mut HashMap<String, DevSession>, current: Scope) {
        map.retain(|_, s| s.scope == current && s.expires > Instant::now());
    }
    pub(super) fn label(input: &str) -> Result<String, String> {
        if input.len() > 512 {
            return Err("invalid_actor".into());
        }
        let clean: String = input.chars().filter(|c| !c.is_control() &&
            !matches!(*c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')).collect();
        let clean = clean.trim();
        if clean.is_empty() || clean.len() > 96 {
            return Err("invalid_actor".into());
        }
        Ok(clean.into())
    }
    pub(super) fn begin(current: Scope, input: &str) -> Result<Value, String> {
        let label = label(input)?;
        let mut map = sessions();
        prune(&mut map, current);
        if map.len() >= 32 {
            return Err("busy".into());
        }
        let id = format!("gui-dev-{}", uuid::Uuid::new_v4());
        let actor = GuiActor {
            id: id.clone(),
            kind: "dev",
            label,
        };
        map.insert(
            id.clone(),
            DevSession {
                scope: current,
                actor: actor.clone(),
                expires: Instant::now() + Duration::from_secs(3600),
            },
        );
        Ok(json!({ "session_id": id, "actor": actor }))
    }
    pub(super) fn actor(current: Scope, id: &str) -> Result<GuiActor, String> {
        let mut map = sessions();
        prune(&mut map, current);
        let session = map.get_mut(id).ok_or("gui_session_expired")?;
        session.expires = Instant::now() + Duration::from_secs(3600);
        Ok(session.actor.clone())
    }
    pub(super) fn end(current: Scope, id: &str) -> Option<GuiActor> {
        let mut map = sessions();
        prune(&mut map, current);
        map.remove(id).map(|s| s.actor)
    }
}
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn gui_dev_session_begin(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    label: String,
) -> Result<Value, String> {
    check_window(window.label())?;
    let current = scope(&app).await?;
    if !current.unlocked {
        return Err("locked".into());
    }
    dev_sessions::begin(current, &label)
}
#[cfg(debug_assertions)]
fn validate_dev_session_id(id: &str) -> Result<(), String> {
    let valid = id
        .strip_prefix("gui-dev-")
        .and_then(|value| {
            uuid::Uuid::parse_str(value)
                .ok()
                .filter(|uuid| uuid.hyphenated().to_string() == value)
        })
        .is_some();
    if valid {
        Ok(())
    } else {
        Err("invalid_args".into())
    }
}
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn gui_dev_session_end(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    session_id: String,
) -> Result<(), String> {
    check_window(window.label())?;
    validate_dev_session_id(&session_id)?;
    // Cleanup remains possible after expiry or an account/unlock generation change.
    // UUID identity ensures an old finish cannot release a newer actor's lease.
    let _ = dev_sessions::end(scope(&app).await?, &session_id);
    app.emit_to("main", "gui-actor-ended", json!({ "actor_id": session_id }))
        .map_err(|_| "gui_unavailable".to_string())
}
struct Pending {
    actor: GuiActor,
    deadline: Instant,
    scope: Scope,
    claimed: bool,
    sender: oneshot::Sender<Reply>,
    settings_delta: Option<crate::gui_settings::SettingsDelta>,
    settings_committed: bool,
    cancelled: bool,
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
/// Safe Settings projection: allowlisted public values only. The frontend
/// builds it field-by-field; this validator re-checks shape, bounds and
/// exact allowlisted values, and the locked check below requires it to be absent
/// whenever the app is locked.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SettingsAiProvider {
    id: String,
    name: String,
    #[serde(rename = "type")]
    provider_type: String,
    enabled: bool,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SettingsAiModel {
    id: String,
    provider_id: String,
    name: String,
    enabled: bool,
    is_default: bool,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SettingsAi {
    providers: Vec<SettingsAiProvider>,
    models: Vec<SettingsAiModel>,
    default_model_id: Option<String>,
    advanced: serde_json::Map<String, Value>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SettingsView {
    open: Option<String>,
    general: Option<serde_json::Map<String, Value>>,
    ai: Option<SettingsAi>,
}
fn settings_integer(value: &Value, min: i64, max: i64) -> bool {
    value
        .as_f64()
        .is_some_and(|n| n.fract() == 0.0 && n >= min as f64 && n <= max as f64)
}
fn settings_number(value: &Value, min: f64, max: f64) -> bool {
    value
        .as_f64()
        .is_some_and(|n| n.is_finite() && n >= min && n <= max)
}
fn settings_choice(value: &Value, choices: &[&str]) -> bool {
    value.as_str().is_some_and(|s| choices.contains(&s))
}
pub(crate) fn general_settings_map(map: &serde_json::Map<String, Value>) -> bool {
    map.len() <= MAX_SETTINGS_KEYS
        && map.iter().all(|(key, value)| match key.as_str() {
            "showHiddenFiles"
            | "showStatusBar"
            | "showTransferProgress"
            | "compactMode"
            | "swapPanels"
            | "sortFoldersFirst"
            | "showFileExtensions"
            | "showToastNotifications"
            | "discoverHealthCheck" => value.is_boolean(),
            "fontSize" => settings_integer(value, 10, 22),
            "introHubIconSize" => settings_integer(value, 18, 32),
            "dateFormat" => settings_choice(value, &["localized", "iso", "dmy", "mdy"]),
            "cardLayout" => settings_choice(value, &["compact", "detailed"]),
            "favoriteMarker" => settings_choice(value, &["star", "heart"]),
            "fontFamily" => settings_choice(
                value,
                &[
                    "'Inter', system-ui, sans-serif",
                    "system-ui, -apple-system, sans-serif",
                    "'FiraGO', sans-serif",
                    "'Noto Sans', sans-serif",
                    "'JetBrains Mono', monospace",
                ],
            ),
            _ => false,
        })
}
pub(crate) fn advanced_settings_map(map: &serde_json::Map<String, Value>) -> bool {
    map.len() <= MAX_SETTINGS_KEYS
        && map.iter().all(|(key, value)| match key.as_str() {
            "temperature" => settings_number(value, 0.0, 2.0),
            "max_tokens" => settings_integer(value, 256, 32768),
            "top_p" => settings_number(value, 0.0, 1.0),
            "top_k" => settings_integer(value, 1, 100),
            "conversation_style" => settings_choice(value, &["precise", "balanced", "creative"]),
            "response_style" => {
                settings_choice(value, &["default", "concise", "explanatory", "learning"])
            }
            _ => false,
        })
}
fn valid_settings_view(settings: &SettingsView) -> bool {
    if settings
        .open
        .as_deref()
        .is_some_and(|area| !matches!(area, "general" | "ai"))
    {
        return false;
    }
    if let Some(general) = &settings.general {
        if !general_settings_map(general) {
            return false;
        }
    }
    if let Some(ai) = &settings.ai {
        if ai.providers.len() > MAX_SETTINGS_PROVIDERS
            || ai.models.len() > MAX_SETTINGS_MODELS
            || !advanced_settings_map(&ai.advanced)
            || ai.providers.iter().any(|p| {
                p.id.len() > MAX_SETTINGS_STRING
                    || p.name.len() > MAX_SETTINGS_STRING
                    || p.provider_type.len() > 64
            })
            || ai.models.iter().any(|m| {
                m.id.len() > MAX_SETTINGS_STRING
                    || m.provider_id.len() > MAX_SETTINGS_STRING
                    || m.name.len() > MAX_SETTINGS_STRING
            })
            || ai
                .default_model_id
                .as_deref()
                .is_some_and(|id| id.len() > MAX_SETTINGS_STRING)
        {
            return false;
        }
    }
    true
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ToolsView {
    open: bool,
    visible_panels: Vec<String>,
    protected: bool,
}
fn valid_tools_view(tools: &ToolsView) -> bool {
    tools.visible_panels.len() <= 4
        && (tools.open || tools.visible_panels.is_empty())
        && tools.visible_panels.iter().enumerate().all(|(i, panel)| {
            ["editor", "terminal", "agent", "security"].contains(&panel.as_str())
                && !tools.visible_panels[..i].contains(panel)
                && (panel != "security" || tools.protected)
        })
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    settings: Option<SettingsView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tools: Option<ToolsView>,
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
    if let Some(settings) = &s.settings {
        if !valid_settings_view(settings) {
            return Err("gui_invalid_reply".into());
        }
    }
    if s.tools
        .as_ref()
        .is_some_and(|tools| !valid_tools_view(tools))
    {
        return Err("gui_invalid_reply".into());
    }
    if (!unlocked || s.locked)
        && (!s.locked
            || s.connected
            || s.active_session_id.is_some()
            || !s.sessions.is_empty()
            || s.panels.remote.is_some()
            || s.panels.local.is_some()
            || s.panels.local2.is_some()
            || s.settings.is_some()
            || s.tools.is_some()
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
    if entry.cancelled {
        return Err("gui_cancelled".into());
    }
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
    session_id: Option<&str>,
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
    let actor = GuiActor::aeroagent(session_id);
    #[cfg(debug_assertions)]
    let actor = match session_id.filter(|id| id.starts_with("gui-dev-")) {
        Some(id) => dev_sessions::actor(current, id)?,
        None => actor,
    };
    let id = uuid::Uuid::new_v4().to_string();
    let settings_delta = if name == "settings_update" {
        Some(crate::gui_settings::SettingsDelta::from_args(&args)?)
    } else {
        None
    };
    let (sender, receiver) = oneshot::channel();
    {
        let mut map = pending();
        if map.len() >= MAX_PENDING {
            return Err("busy".into());
        }
        map.insert(
            id.clone(),
            Pending {
                actor,
                deadline: Instant::now() + Duration::from_millis(timeout_ms),
                scope: current,
                claimed: false,
                sender,
                settings_delta,
                settings_committed: false,
                cancelled: false,
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
) -> Result<Value, String> {
    check_window(window.label())?;
    let current = scope(&app).await?;
    let mut map = pending();
    let entry = map.get_mut(&id).ok_or("gui_unknown_request")?;
    let remaining = claim_entry(entry, current)?;
    Ok(json!({ "remaining_ms": remaining, "actor": entry.actor }))
}

/// Recheck the original claimed request before each awaited frontend Settings step.
#[tauri::command]
pub async fn gui_intent_check(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    id: String,
) -> Result<(), String> {
    check_window(window.label())?;
    let current = scope(&app).await?;
    let map = pending();
    let entry = map.get(&id).ok_or("gui_unknown_request")?;
    check_pending(entry, current)?;
    if !entry.claimed {
        return Err("gui_not_claimed".into());
    }
    if entry.cancelled {
        return Err("gui_cancelled".into());
    }
    Ok(())
}

/// Stop invalidates future writes; the request remains available for its failure receipt.
#[tauri::command]
pub async fn gui_intent_cancel(window: tauri::WebviewWindow, id: String) -> Result<(), String> {
    check_window(window.label())?;
    let mut map = pending();
    let entry = map.get_mut(&id).ok_or("gui_unknown_request")?;
    if !entry.claimed {
        return Err("gui_not_claimed".into());
    }
    entry.cancelled = true;
    Ok(())
}

fn check_settings_commit(entry: &Pending) -> Result<(), String> {
    if Instant::now() >= entry.deadline {
        return Err("gui_timeout".into());
    }
    if !entry.claimed {
        return Err("gui_not_claimed".into());
    }
    if entry.cancelled {
        return Err("gui_cancelled".into());
    }
    if !entry.scope.unlocked {
        return Err("locked".into());
    }
    if entry.settings_committed {
        return Err("gui_already_committed".into());
    }
    if entry.settings_delta.is_none() {
        return Err("invalid_args".into());
    }
    Ok(())
}

/// Commit only the original approved public delta. No destination, blob or new delta is accepted.
#[tauri::command]
pub async fn gui_settings_commit(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    id: String,
) -> Result<Value, String> {
    check_window(window.label())?;
    tauri::async_runtime::spawn_blocking(move || {
        // Stop/removal, SQLite account switches, user locks, master locks and vault writers
        // cannot cross this critical section. A write accepted before Stop remains committed.
        let mut map = pending();
        let entry = map.get_mut(&id).ok_or("gui_unknown_request")?;
        check_settings_commit(entry)?;
        let delta = entry.settings_delta.as_ref().ok_or("invalid_args")?;
        let mut conn = crate::user_partitions::open_or_init(&app)?;
        let value = crate::user_partitions::with_gui_account(
            &mut conn,
            entry.scope.user,
            entry.scope.partition_generation,
            || {
                crate::credential_store::CredentialStore::update_gui_config(
                    entry.scope.vault_generation,
                    delta.account(),
                    || check_settings_commit(entry),
                    |existing| delta.apply(existing),
                )
            },
        )?;
        let area = delta.area.clone();
        entry.settings_committed = true;
        Ok(json!({ "area": area, "value": value }))
    })
    .await
    .map_err(|_| "action_failed".to_string())?
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
    let mut map = pending();
    if map.get(&id).is_some_and(|entry| entry.cancelled) && reply.ok {
        return Err("gui_cancelled".into());
    }
    if reply.ok
        && map
            .get(&id)
            .is_some_and(|entry| entry.settings_delta.is_some() && !entry.settings_committed)
    {
        return Err("gui_not_committed".into());
    }
    let entry = consume_entry(&mut map, &id, current)?;
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
    fn gui_actor_identity_is_session_bound_without_exposing_the_session_key() {
        let first = GuiActor::aeroagent(Some("private-session-one"));
        let same = GuiActor::aeroagent(Some("private-session-one"));
        let second = GuiActor::aeroagent(Some("private-session-two"));
        assert_eq!(first.id, same.id);
        assert_ne!(first.id, second.id);
        assert!(!serde_json::to_string(&first)
            .unwrap()
            .contains("private-session"));
        assert_ne!(GuiActor::aeroagent(None).id, GuiActor::aeroagent(None).id);
    }
    #[cfg(debug_assertions)]
    #[test]
    fn gui_dev_sessions_sanitize_names_and_expire_across_account_and_unlock_changes() {
        assert_eq!(dev_sessions::label("  Co\u{202e}dex\n  ").unwrap(), "Codex");
        assert!(dev_sessions::label("\n\u{202e}").is_err());
        assert!(dev_sessions::label(&"é".repeat(49)).is_err());
        let current = Scope {
            user: Some(9123),
            unlocked: true,
            vault_generation: 7,
            partition_generation: 3,
        };
        let first = dev_sessions::begin(current, "Codex").unwrap();
        let second = dev_sessions::begin(current, "Codex").unwrap();
        let first_id = first["session_id"].as_str().unwrap();
        let second_id = second["session_id"].as_str().unwrap();
        assert_ne!(first_id, second_id);
        assert_eq!(
            dev_sessions::actor(current, first_id).unwrap().label,
            "Codex"
        );
        dev_sessions::end(current, first_id).unwrap();
        assert!(dev_sessions::end(current, first_id).is_none());
        assert!(dev_sessions::actor(current, first_id).is_err());
        assert!(dev_sessions::actor(
            Scope {
                vault_generation: 8,
                ..current
            },
            second_id
        )
        .is_err());
        assert!(dev_sessions::end(current, second_id).is_none());
        let third = dev_sessions::begin(current, "Claude").unwrap();
        assert!(dev_sessions::actor(
            Scope {
                user: Some(9124),
                ..current
            },
            third["session_id"].as_str().unwrap()
        )
        .is_err());
    }
    #[cfg(debug_assertions)]
    #[test]
    fn developer_cleanup_refuses_other_namespaces_but_accepts_stale_ids() {
        assert!(validate_dev_session_id("gui-dev-00000000-0000-4000-8000-000000000000").is_ok());
        for id in [
            "aeroagent:abc",
            "gui-dev-",
            "gui-dev-00000000000040008000000000000000",
            "gui-dev-xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
        ] {
            assert!(validate_dev_session_id(id).is_err());
        }
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
    fn gui_controller_tools_metadata_is_closed_bounded_and_locked_redacted() {
        let mut value = locked_reply();
        value["snapshot"]["locked"] = json!(false);
        value["snapshot"]["tools"] =
            json!({"open":true,"visible_panels":["editor","terminal","agent"],"protected":false});
        assert!(parse_reply(value.clone(), true).is_ok());
        for tools in [
            json!({"open":true,"visible_panels":["shell"],"protected":false}),
            json!({"open":true,"visible_panels":["editor","editor"],"protected":false}),
            json!({"open":false,"visible_panels":["editor"],"protected":false}),
            json!({"open":true,"visible_panels":["security"],"protected":false}),
            json!({"open":true,"visible_panels":[],"protected":false,"content":"SECRET"}),
        ] {
            value["snapshot"]["tools"] = tools;
            assert!(parse_reply(value.clone(), true).is_err());
        }
        let mut locked = locked_reply();
        locked["snapshot"]["tools"] = json!({"open":false,"visible_panels":[],"protected":false});
        assert_eq!(parse_reply(locked, false).unwrap_err(), "gui_locked_reply");
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
    fn unlocked_reply_with_settings() -> Value {
        json!({ "ok": true, "error": null, "snapshot": { "schema_version": 1, "state_revision": 2,
            "version": "test", "locked": false, "blocked": false, "view": "other", "connected": false,
            "active_session_id": null, "sessions": [], "panels": {}, "queue": { "active": 0, "pending": 0, "failed": 0 },
            "settings": {
                "open": "general",
                "general": { "showHiddenFiles": true, "fontSize": 16, "dateFormat": "iso" },
                "ai": {
                    "providers": [{ "id": "p1", "name": "Fixture", "type": "openai", "enabled": true }],
                    "models": [{ "id": "m1", "provider_id": "p1", "name": "fixture-1", "enabled": true, "is_default": true }],
                    "default_model_id": null,
                    "advanced": { "temperature": 0.7, "max_tokens": 4096 }
                }
            } } })
    }
    #[test]
    fn gui_controller_settings_projection_is_bounded_and_locked_redacted() {
        // A well-formed unlocked reply with the safe projection parses.
        assert!(parse_reply(unlocked_reply_with_settings(), true).is_ok());
        // The projection must be absent from a locked reply.
        let mut locked = locked_reply();
        locked["snapshot"]["settings"] = json!({ "open": null, "general": null, "ai": null });
        assert!(parse_reply(locked, false).is_err());
        // Unknown areas, nested objects, oversized lists and secret-shaped
        // values are all rejected before the reply can leave the broker.
        let mut bad = unlocked_reply_with_settings();
        bad["snapshot"]["settings"]["open"] = json!("vault");
        assert!(parse_reply(bad, true).is_err());
        let mut bad = unlocked_reply_with_settings();
        bad["snapshot"]["settings"]["general"]["nested"] = json!({ "apiKey": "x" });
        assert!(parse_reply(bad, true).is_err());
        let mut bad = unlocked_reply_with_settings();
        bad["snapshot"]["settings"]["ai"]["providers"] = json!(vec![
            json!({ "id": "p", "name": "n", "type": "t", "enabled": true });
            33
        ]);
        assert!(parse_reply(bad, true).is_err());
        let mut bad = unlocked_reply_with_settings();
        bad["snapshot"]["settings"]["ai"]["providers"][0]["apiKey"] = json!("SECRET");
        assert!(parse_reply(bad, true).is_err());
    }
    #[test]
    fn gui_controller_settings_reject_scalar_secrets_and_invalid_values() {
        for area in ["general", "ai"] {
            for key in ["apiKey", "password", "constructor", "unknown"] {
                let mut bad = unlocked_reply_with_settings();
                let map = if area == "general" {
                    &mut bad["snapshot"]["settings"]["general"]
                } else {
                    &mut bad["snapshot"]["settings"]["ai"]["advanced"]
                };
                map[key] = json!("SECRET");
                assert!(parse_reply(bad, true).is_err(), "{area}.{key}");
            }
        }
        for (key, value) in [
            ("fontSize", json!(99)),
            ("fontSize", json!(14.5)),
            ("showHiddenFiles", json!("true")),
            ("dateFormat", json!("SECRET")),
        ] {
            let mut bad = unlocked_reply_with_settings();
            bad["snapshot"]["settings"]["general"][key] = value;
            assert!(parse_reply(bad, true).is_err(), "{key}");
        }
        for (key, value) in [
            ("temperature", json!(2.1)),
            ("max_tokens", json!(100)),
            ("top_p", json!(-1)),
            ("response_style", json!("SECRET")),
        ] {
            let mut bad = unlocked_reply_with_settings();
            bad["snapshot"]["settings"]["ai"]["advanced"][key] = value;
            assert!(parse_reply(bad, true).is_err(), "{key}");
        }
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
            vault_generation: 1,
            partition_generation: 1,
        };
        let mut entry = Pending {
            actor: GuiActor::aeroagent(Some("test")),
            deadline: Instant::now() + Duration::from_secs(1),
            scope: original,
            claimed: false,
            sender,
            settings_delta: None,
            settings_committed: false,
            cancelled: false,
        };
        assert!(check_pending(&entry, original).is_ok());
        assert!(check_pending(
            &entry,
            Scope {
                user: Some(2),
                unlocked: true,
                ..original
            }
        )
        .is_err());
        entry.deadline = Instant::now();
        assert!(check_pending(&entry, original).is_err());
    }
    #[test]
    fn settings_commit_refuses_foreign_unclaimed_expired_cancelled_and_replayed_requests() {
        let (sender, _) = oneshot::channel();
        let original = Scope {
            user: Some(1),
            unlocked: true,
            vault_generation: 1,
            partition_generation: 1,
        };
        let mut entry = Pending {
            actor: GuiActor::aeroagent(Some("test")),
            deadline: Instant::now() + Duration::from_secs(2),
            scope: original,
            claimed: false,
            sender,
            settings_delta: None,
            settings_committed: false,
            cancelled: false,
        };
        assert!(check_settings_commit(&entry).is_err());
        entry.claimed = true;
        assert!(check_settings_commit(&entry).is_err());
        entry.settings_delta = Some(
            crate::gui_settings::SettingsDelta::from_args(
                &json!({"area":"general","set":{"fontSize":16}}),
            )
            .unwrap(),
        );
        assert!(check_settings_commit(&entry).is_ok());
        for changed in [
            Scope {
                vault_generation: 2,
                ..original
            },
            Scope {
                partition_generation: 2,
                ..original
            },
        ] {
            assert!(check_pending(&entry, changed).is_err()); // lock/unlock and account ABA invalidate the original request.
        }
        entry.cancelled = true;
        assert!(check_settings_commit(&entry).is_err());
        entry.cancelled = false;
        entry.settings_committed = true;
        assert!(check_settings_commit(&entry).is_err());
        entry.settings_committed = false;
        entry.deadline = Instant::now();
        assert!(check_settings_commit(&entry).is_err());
    }
    #[tokio::test]
    async fn gui_controller_claims_and_replies_are_single_use() {
        let mut map = HashMap::new();
        let (sender, receiver) = oneshot::channel();
        let original = Scope {
            user: Some(1),
            unlocked: true,
            vault_generation: 1,
            partition_generation: 1,
        };
        map.insert(
            "issued".into(),
            Pending {
                actor: GuiActor::aeroagent(Some("test")),
                deadline: Instant::now() + Duration::from_secs(2),
                scope: original,
                claimed: false,
                sender,
                settings_delta: None,
                settings_committed: false,
                cancelled: false,
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
                unlocked: true,
                ..original
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
