//! Chat History SQLite Backend for AeroAgent
//!
//! Replaces the JSON flat-file approach with SQLite + FTS5 for:
//! - Atomic writes (WAL mode, crash-safe)
//! - Full-text search across all conversations
//! - Configurable retention policies
//! - Efficient partial reads (no full-file rewrite)

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatSession {
    pub id: String,
    pub title: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub message_count: i64,
    pub total_tokens: i64,
    pub total_cost: f64,
    pub project_path: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub tool_calls: Option<String>,
    pub thinking: Option<String>,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cost: f64,
    pub model: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatBranch {
    pub id: String,
    pub session_id: String,
    pub name: String,
    pub parent_message_id: String,
    pub created_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BranchMessage {
    pub id: String,
    pub branch_id: String,
    pub role: String,
    pub content: String,
    pub tool_calls: Option<String>,
    pub thinking: Option<String>,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cost: f64,
    pub model: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionWithMessages {
    pub session: ChatSession,
    pub messages: Vec<ChatMessage>,
    pub branches: Vec<ChatBranch>,
    pub active_branch_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub message_id: String,
    pub session_id: String,
    pub session_title: String,
    pub role: String,
    pub content: String,
    pub created_at: i64,
    pub snippet: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatStats {
    pub total_sessions: i64,
    pub total_messages: i64,
    pub total_tokens: i64,
    pub total_cost: f64,
    pub db_size_bytes: i64,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct ChatHistoryDb(pub Mutex<Connection>);

fn db_path(app: &AppHandle) -> Result<PathBuf, String> {
    let config_dir = app
        .path()
        .app_config_dir()
        .map_err(|_| "Cannot resolve app config dir".to_string())?;
    Ok(config_dir.join("ai_chat.db"))
}

/// Acquire DB lock with poison recovery (SEC-011/BUG-006)
fn acquire_lock(db: &ChatHistoryDb) -> std::sync::MutexGuard<'_, Connection> {
    db.0.lock().unwrap_or_else(|e| {
        log::warn!("Chat history DB mutex was poisoned, recovering: {e}");
        e.into_inner()
    })
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Initialize schema on an already-opened connection (used for in-memory fallback)
pub fn init_db_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA cache_size = -4000;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;",
    )
    .map_err(|e| format!("Pragma error: {e}"))?;

    apply_schema(conn)
}

fn apply_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            provider TEXT,
            model TEXT,
            message_count INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0,
            total_cost REAL NOT NULL DEFAULT 0.0,
            project_path TEXT,
            active_branch_id TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS messages (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            tool_calls TEXT,
            thinking TEXT,
            tokens_in INTEGER DEFAULT 0,
            tokens_out INTEGER DEFAULT 0,
            cost REAL DEFAULT 0.0,
            model TEXT,
            created_at INTEGER NOT NULL,
            FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS branches (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            name TEXT NOT NULL,
            parent_message_id TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS branch_messages (
            id TEXT PRIMARY KEY,
            branch_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            tool_calls TEXT,
            thinking TEXT,
            tokens_in INTEGER DEFAULT 0,
            tokens_out INTEGER DEFAULT 0,
            cost REAL DEFAULT 0.0,
            model TEXT,
            created_at INTEGER NOT NULL,
            FOREIGN KEY (branch_id) REFERENCES branches(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
        CREATE INDEX IF NOT EXISTS idx_messages_created ON messages(created_at);
        CREATE INDEX IF NOT EXISTS idx_branches_session ON branches(session_id);
        CREATE INDEX IF NOT EXISTS idx_branch_messages_branch ON branch_messages(branch_id);
        CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at DESC);",
    )
    .map_err(|e| format!("Schema error: {e}"))?;

    // FTS5 virtual table (SEC-012: log errors instead of silent ignore)
    if let Err(e) = conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
            content,
            content=messages,
            content_rowid=rowid
        );",
    ) {
        log::warn!("FTS5 not available: {e}. Full-text search will be disabled.");
    }

    // FTS triggers
    let has_ai_trigger: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='trigger' AND name='messages_ai'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(false);

    if !has_ai_trigger {
        if let Err(e) = conn.execute_batch(
            "CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid, content) VALUES (new.rowid, new.content);
            END;
            CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.rowid, old.content);
            END;
            CREATE TRIGGER messages_au AFTER UPDATE OF content ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.rowid, old.content);
                INSERT INTO messages_fts(rowid, content) VALUES (new.rowid, new.content);
            END;",
        ) {
            log::warn!("Failed to create FTS triggers: {e}. Search may be incomplete.");
        }
    }

    Ok(())
}

pub fn init_db(app: &AppHandle) -> Result<Connection, String> {
    let path = db_path(app)?;

    // Ensure parent dir exists with 0700
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create config dir: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }

    let conn = Connection::open(&path).map_err(|_| "Failed to initialize chat database".to_string())?;

    // Set DB file permissions to 0600 (SEC-008)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA cache_size = -4000;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;",
    )
    .map_err(|e| format!("Pragma error: {e}"))?;

    apply_schema(&conn)?;
    Ok(conn)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Sanitize FTS5 query to prevent query injection (SEC-003)
fn sanitize_fts_query(query: &str) -> String {
    let cleaned = query.replace('"', "\"\"");
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return String::new();
    }
    format!("\"{}\"", cleaned)
}

/// Sanitize FTS5 snippet HTML — escape all HTML except <mark> tags (SEC-001)
fn sanitize_fts_snippet(snippet: &str) -> String {
    snippet
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace("&lt;mark&gt;", "<mark>")
        .replace("&lt;/mark&gt;", "</mark>")
}

/// Log and filter row deserialization errors (BUG-002/SEC-006)
fn log_filter_row<T>(r: Result<T, rusqlite::Error>) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            log::warn!("Skipping malformed row: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Migration from JSON
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct LegacyConversation {
    id: String,
    title: String,
    messages: Vec<LegacyMessage>,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    #[serde(rename = "totalTokens", default)]
    total_tokens: i64,
    #[serde(rename = "totalCost", default)]
    total_cost: f64,
    branches: Option<Vec<LegacyBranch>>,
    #[serde(rename = "activeBranchId")]
    active_branch_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LegacyMessage {
    id: String,
    role: String,
    content: String,
    timestamp: String,
    #[serde(rename = "modelInfo")]
    model_info: Option<LegacyModelInfo>,
    #[serde(rename = "tokenInfo")]
    token_info: Option<LegacyTokenInfo>,
}

#[derive(Debug, Deserialize)]
struct LegacyModelInfo {
    #[serde(rename = "modelName", default)]
    model_name: String,
    #[serde(rename = "providerName", default)]
    #[allow(dead_code)]
    provider_name: String,
    #[serde(rename = "providerType", default)]
    provider_type: String,
}

#[derive(Debug, Deserialize)]
struct LegacyTokenInfo {
    #[serde(rename = "inputTokens", default)]
    input_tokens: Option<i64>,
    #[serde(rename = "outputTokens", default)]
    output_tokens: Option<i64>,
    #[serde(rename = "totalTokens", default)]
    #[allow(dead_code)]
    total_tokens: Option<i64>,
    #[serde(default)]
    cost: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct LegacyBranch {
    id: String,
    name: String,
    #[serde(rename = "parentMessageId")]
    parent_message_id: String,
    messages: Vec<LegacyMessage>,
    #[serde(rename = "createdAt")]
    created_at: String,
}

fn iso_to_epoch_ms(iso: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|dt| dt.timestamp_millis())
        .unwrap_or_else(|e| {
            log::warn!("Invalid timestamp '{iso}': {e}, using current time");
            chrono::Utc::now().timestamp_millis()
        })
}

pub fn migrate_from_json(conn: &Connection, app: &AppHandle) -> Result<usize, String> {
    let config_dir = app
        .path()
        .app_config_dir()
        .map_err(|_| "Cannot resolve config dir".to_string())?;
    let json_path = config_dir.join("ai_history.json");

    if !json_path.exists() {
        return Ok(0);
    }

    // Check if already migrated
    let migrated_path = config_dir.join("ai_history.json.migrated");
    if migrated_path.exists() {
        return Ok(0);
    }

    let content =
        std::fs::read_to_string(&json_path).map_err(|e| format!("Cannot read JSON: {e}"))?;
    let conversations: Vec<LegacyConversation> =
        serde_json::from_str(&content).map_err(|e| format!("Cannot parse JSON: {e}"))?;

    let mut migrated = 0;

    // Wrap entire migration in transaction (SEC-005/BUG-005/PERF-014)
    conn.execute_batch("BEGIN TRANSACTION")
        .map_err(|e| format!("Begin migration tx: {e}"))?;

    let result = (|| -> Result<(), String> {
        for conv in &conversations {
            let created_at = iso_to_epoch_ms(&conv.created_at);
            let updated_at = iso_to_epoch_ms(&conv.updated_at);

            let first_assistant = conv.messages.iter().find(|m| m.role == "assistant");
            let provider = first_assistant
                .and_then(|m| m.model_info.as_ref())
                .map(|mi| mi.provider_type.clone());
            let model = first_assistant
                .and_then(|m| m.model_info.as_ref())
                .map(|mi| mi.model_name.clone());

            conn.execute(
                "INSERT OR IGNORE INTO sessions (id, title, provider, model, message_count, total_tokens, total_cost, active_branch_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    conv.id, conv.title, provider, model,
                    conv.messages.len() as i64, conv.total_tokens, conv.total_cost,
                    conv.active_branch_id, created_at, updated_at,
                ],
            )
            .map_err(|e| format!("Insert session error: {e}"))?;

            for msg in &conv.messages {
                let ts = iso_to_epoch_ms(&msg.timestamp);
                let tokens_in = msg.token_info.as_ref().and_then(|ti| ti.input_tokens).unwrap_or(0);
                let tokens_out = msg.token_info.as_ref().and_then(|ti| ti.output_tokens).unwrap_or(0);
                let cost = msg.token_info.as_ref().and_then(|ti| ti.cost).unwrap_or(0.0);
                let msg_model = msg.model_info.as_ref().map(|mi| mi.model_name.clone());

                conn.execute(
                    "INSERT OR IGNORE INTO messages (id, session_id, role, content, tokens_in, tokens_out, cost, model, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![msg.id, conv.id, msg.role, msg.content, tokens_in, tokens_out, cost, msg_model, ts],
                )
                .map_err(|e| format!("Insert message error: {e}"))?;
            }

            if let Some(branches) = &conv.branches {
                for branch in branches {
                    let branch_created = iso_to_epoch_ms(&branch.created_at);
                    conn.execute(
                        "INSERT OR IGNORE INTO branches (id, session_id, name, parent_message_id, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![branch.id, conv.id, branch.name, branch.parent_message_id, branch_created],
                    )
                    .map_err(|e| format!("Insert branch error: {e}"))?;

                    for msg in &branch.messages {
                        let ts = iso_to_epoch_ms(&msg.timestamp);
                        let tokens_in = msg.token_info.as_ref().and_then(|ti| ti.input_tokens).unwrap_or(0);
                        let tokens_out = msg.token_info.as_ref().and_then(|ti| ti.output_tokens).unwrap_or(0);
                        let cost = msg.token_info.as_ref().and_then(|ti| ti.cost).unwrap_or(0.0);
                        let msg_model = msg.model_info.as_ref().map(|mi| mi.model_name.clone());

                        conn.execute(
                            "INSERT OR IGNORE INTO branch_messages (id, branch_id, role, content, tokens_in, tokens_out, cost, model, created_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                            params![msg.id, branch.id, msg.role, msg.content, tokens_in, tokens_out, cost, msg_model, ts],
                        )
                        .map_err(|e| format!("Insert branch message error: {e}"))?;
                    }
                }
            }

            migrated += 1;
        }
        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")
                .map_err(|e| format!("Commit migration: {e}"))?;
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }

    // Rebuild FTS index after migration
    let _ = conn.execute_batch("INSERT INTO messages_fts(messages_fts) VALUES('rebuild');");

    // Rename JSON to .migrated
    std::fs::rename(&json_path, &migrated_path)
        .map_err(|e| format!("Cannot rename JSON: {e}"))?;

    Ok(migrated)
}

// ---------------------------------------------------------------------------
// Internal helpers (non-command, avoid double-lock — BUG-003)
// ---------------------------------------------------------------------------

fn get_session_inner(conn: &Connection, session_id: &str) -> Result<SessionWithMessages, String> {
    // Session + active_branch_id in single query (PERF-006)
    let (session, active_branch_id) = conn
        .query_row(
            "SELECT id, title, provider, model, message_count, total_tokens, total_cost, project_path, created_at, updated_at, active_branch_id
             FROM sessions WHERE id = ?1",
            params![session_id],
            |row| {
                let s = ChatSession {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    provider: row.get(2)?,
                    model: row.get(3)?,
                    message_count: row.get(4)?,
                    total_tokens: row.get(5)?,
                    total_cost: row.get(6)?,
                    project_path: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                };
                let branch_id: Option<String> = row.get(10)?;
                Ok((s, branch_id))
            },
        )
        .map_err(|e| format!("Session not found: {e}"))?;

    // Messages (with 2000 limit — SEC-010)
    let mut msg_stmt = conn
        .prepare(
            "SELECT id, session_id, role, content, tool_calls, thinking, tokens_in, tokens_out, cost, model, created_at
             FROM messages WHERE session_id = ?1 ORDER BY created_at ASC LIMIT 2000",
        )
        .map_err(|e| format!("Prepare error: {e}"))?;
    let messages: Vec<ChatMessage> = msg_stmt
        .query_map(params![session_id], row_to_message)
        .map_err(|e| format!("Query error: {e}"))?
        .filter_map(log_filter_row)
        .collect();

    // Branches
    let mut branch_stmt = conn
        .prepare(
            "SELECT id, session_id, name, parent_message_id, created_at
             FROM branches WHERE session_id = ?1 ORDER BY created_at ASC",
        )
        .map_err(|e| format!("Prepare error: {e}"))?;
    let branches: Vec<ChatBranch> = branch_stmt
        .query_map(params![session_id], |row| {
            Ok(ChatBranch {
                id: row.get(0)?,
                session_id: row.get(1)?,
                name: row.get(2)?,
                parent_message_id: row.get(3)?,
                created_at: row.get(4)?,
            })
        })
        .map_err(|e| format!("Query error: {e}"))?
        .filter_map(log_filter_row)
        .collect();

    Ok(SessionWithMessages {
        session,
        messages,
        branches,
        active_branch_id,
    })
}

fn row_to_message(row: &rusqlite::Row) -> rusqlite::Result<ChatMessage> {
    Ok(ChatMessage {
        id: row.get(0)?,
        session_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        tool_calls: row.get(4)?,
        thinking: row.get(5)?,
        tokens_in: row.get(6)?,
        tokens_out: row.get(7)?,
        cost: row.get(8)?,
        model: row.get(9)?,
        created_at: row.get(10)?,
    })
}

fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<ChatSession> {
    Ok(ChatSession {
        id: row.get(0)?,
        title: row.get(1)?,
        provider: row.get(2)?,
        model: row.get(3)?,
        message_count: row.get(4)?,
        total_tokens: row.get(5)?,
        total_cost: row.get(6)?,
        project_path: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

// ---------------------------------------------------------------------------
// Tauri Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn chat_history_init(_app: AppHandle) -> Result<String, String> {
    // BUG-015: Migration handled in lib.rs setup, not here
    Ok("Chat history initialized.".to_string())
}

#[tauri::command]
pub async fn chat_history_list_sessions(
    app: AppHandle,
    limit: Option<i64>,
    offset: Option<i64>,
    project_path: Option<String>,
) -> Result<Vec<ChatSession>, String> {
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);

    let limit = limit.unwrap_or(100);
    let offset = offset.unwrap_or(0);

    if let Some(ref pp) = project_path {
        let mut s = conn
            .prepare(
                "SELECT id, title, provider, model, message_count, total_tokens, total_cost, project_path, created_at, updated_at
                 FROM sessions WHERE project_path = ?1 ORDER BY updated_at DESC LIMIT ?2 OFFSET ?3",
            )
            .map_err(|e| format!("Prepare error: {e}"))?;
        let results = s.query_map(params![pp, limit, offset], row_to_session)
            .map_err(|e| format!("Query error: {e}"))?
            .filter_map(log_filter_row)
            .collect::<Vec<_>>();
        Ok(results)
    } else {
        let mut s = conn
            .prepare(
                "SELECT id, title, provider, model, message_count, total_tokens, total_cost, project_path, created_at, updated_at
                 FROM sessions ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| format!("Prepare error: {e}"))?;
        let results = s.query_map(params![limit, offset], row_to_session)
            .map_err(|e| format!("Query error: {e}"))?
            .filter_map(log_filter_row)
            .collect::<Vec<_>>();
        Ok(results)
    }
}

#[tauri::command]
pub async fn chat_history_get_session(
    app: AppHandle,
    session_id: String,
) -> Result<SessionWithMessages, String> {
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);
    get_session_inner(&conn, &session_id)
}

#[tauri::command]
pub async fn chat_history_create_session(
    app: AppHandle,
    id: String,
    title: String,
    provider: Option<String>,
    model: Option<String>,
    project_path: Option<String>,
) -> Result<ChatSession, String> {
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);

    let now = chrono::Utc::now().timestamp_millis();

    conn.execute(
        "INSERT INTO sessions (id, title, provider, model, message_count, total_tokens, total_cost, project_path, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 0, 0, 0.0, ?5, ?6, ?7)",
        params![id, title, provider, model, project_path, now, now],
    )
    .map_err(|e| format!("Insert session: {e}"))?;

    Ok(ChatSession {
        id,
        title,
        provider,
        model,
        message_count: 0,
        total_tokens: 0,
        total_cost: 0.0,
        project_path,
        created_at: now,
        updated_at: now,
    })
}

#[tauri::command]
pub async fn chat_history_save_message(
    app: AppHandle,
    session_id: String,
    message: ChatMessage,
    provider: Option<String>,
    model: Option<String>,
) -> Result<(), String> {
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);

    // True upsert — preserves rowid for FTS sync (BUG-001/PERF-002)
    conn.execute(
        "INSERT INTO messages (id, session_id, role, content, tool_calls, thinking, tokens_in, tokens_out, cost, model, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(id) DO UPDATE SET
           content = excluded.content,
           tool_calls = excluded.tool_calls,
           thinking = excluded.thinking,
           tokens_in = excluded.tokens_in,
           tokens_out = excluded.tokens_out,
           cost = excluded.cost,
           model = excluded.model",
        params![
            message.id,
            session_id,
            message.role,
            message.content,
            message.tool_calls,
            message.thinking,
            message.tokens_in,
            message.tokens_out,
            message.cost,
            message.model,
            message.created_at,
        ],
    )
    .map_err(|e| format!("Insert message: {e}"))?;

    // Update session counters
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "UPDATE sessions SET
            message_count = (SELECT COUNT(*) FROM messages WHERE session_id = ?1),
            total_tokens = (SELECT COALESCE(SUM(tokens_in + tokens_out), 0) FROM messages WHERE session_id = ?1),
            total_cost = (SELECT COALESCE(SUM(cost), 0) FROM messages WHERE session_id = ?1),
            provider = COALESCE(?2, provider),
            model = COALESCE(?3, model),
            updated_at = ?4
         WHERE id = ?1",
        params![session_id, provider, model, now],
    )
    .map_err(|e| format!("Update session counters: {e}"))?;

    Ok(())
}

#[tauri::command]
pub async fn chat_history_update_session_title(
    app: AppHandle,
    session_id: String,
    title: String,
) -> Result<(), String> {
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);

    conn.execute(
        "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
        params![title, chrono::Utc::now().timestamp_millis(), session_id],
    )
    .map_err(|e| format!("Update title: {e}"))?;

    Ok(())
}

#[tauri::command]
pub async fn chat_history_delete_session(
    app: AppHandle,
    session_id: String,
) -> Result<(), String> {
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);

    conn.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])
        .map_err(|e| format!("Delete session: {e}"))?;

    Ok(())
}

#[tauri::command]
pub async fn chat_history_delete_sessions_bulk(
    app: AppHandle,
    session_ids: Option<Vec<String>>,
    older_than_days: Option<i64>,
) -> Result<i64, String> {
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);

    let deleted = if let Some(ids) = session_ids {
        // Batch delete in chunks of 500 to respect SQLITE_LIMIT_VARIABLE_NUMBER (SEC-002)
        let mut total_deleted: i64 = 0;
        for chunk in ids.chunks(500) {
            let placeholders: String = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("DELETE FROM sessions WHERE id IN ({placeholders})");
            let params: Vec<&dyn rusqlite::types::ToSql> =
                chunk.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
            total_deleted += conn.execute(&sql, params.as_slice())
                .map_err(|e| format!("Bulk delete: {e}"))? as i64;
        }
        total_deleted
    } else if let Some(days) = older_than_days {
        let cutoff = chrono::Utc::now().timestamp_millis()
            .checked_sub(days.checked_mul(86_400_000).ok_or("Overflow in days calculation")?)
            .ok_or("Overflow in cutoff calculation")?;
        conn.execute(
            "DELETE FROM sessions WHERE updated_at < ?1",
            params![cutoff],
        )
        .map_err(|e| format!("Delete old sessions: {e}"))? as i64
    } else {
        return Err("Provide session_ids or older_than_days".into());
    };

    Ok(deleted)
}

#[tauri::command]
pub async fn chat_history_search(
    app: AppHandle,
    query: String,
    limit: Option<i64>,
) -> Result<Vec<SearchResult>, String> {
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);

    if query.trim().is_empty() {
        return Ok(vec![]);
    }

    let limit = limit.unwrap_or(50);

    // Sanitize FTS5 query (SEC-003)
    let safe_query = sanitize_fts_query(&query);
    if safe_query.is_empty() {
        return Ok(vec![]);
    }

    // FTS5 search with snippet
    let mut stmt = conn
        .prepare(
            "SELECT m.id, m.session_id, s.title, m.role, m.content, m.created_at,
                    snippet(messages_fts, 0, '<mark>', '</mark>', '...', 40)
             FROM messages_fts
             JOIN messages m ON m.rowid = messages_fts.rowid
             JOIN sessions s ON s.id = m.session_id
             WHERE messages_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )
        .map_err(|e| format!("Prepare FTS: {e}"))?;

    let results: Vec<SearchResult> = stmt
        .query_map(params![safe_query, limit], |row| {
            let raw_snippet: String = row.get(6)?;
            Ok(SearchResult {
                message_id: row.get(0)?,
                session_id: row.get(1)?,
                session_title: row.get(2)?,
                role: row.get(3)?,
                content: row.get(4)?,
                created_at: row.get(5)?,
                snippet: sanitize_fts_snippet(&raw_snippet),
            })
        })
        .map_err(|e| format!("FTS query: {e}"))?
        .filter_map(log_filter_row)
        .collect();

    Ok(results)
}

#[tauri::command]
pub async fn chat_history_cleanup(
    app: AppHandle,
    retention_days: i64,
) -> Result<i64, String> {
    // Validate input (SEC-009)
    if retention_days <= 0 {
        return Err("retention_days must be positive".into());
    }

    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);

    let cutoff = chrono::Utc::now().timestamp_millis()
        .checked_sub(retention_days.checked_mul(86_400_000).ok_or("Overflow in days calculation")?)
        .ok_or("Overflow in cutoff calculation")?;

    let deleted = conn
        .execute(
            "DELETE FROM sessions WHERE updated_at < ?1",
            params![cutoff],
        )
        .map_err(|e| format!("Cleanup: {e}"))? as i64;

    // PERF-004: Only PRAGMA optimize, no VACUUM (VACUUM is blocking)
    if deleted > 0 {
        let _ = conn.execute_batch("PRAGMA optimize;");
    }

    Ok(deleted)
}

#[tauri::command]
pub async fn chat_history_stats(app: AppHandle) -> Result<ChatStats, String> {
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);

    // PERF-007: Combine session stats into single query
    let (total_sessions, total_tokens, total_cost) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(total_tokens), 0), COALESCE(SUM(total_cost), 0) FROM sessions",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?)),
        )
        .unwrap_or((0, 0, 0.0));

    let total_messages: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap_or(0);

    let db_size_bytes: i64 = conn
        .query_row("SELECT page_count * page_size FROM pragma_page_count, pragma_page_size", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);

    Ok(ChatStats {
        total_sessions,
        total_messages,
        total_tokens,
        total_cost,
        db_size_bytes,
    })
}

#[tauri::command]
pub async fn chat_history_export_session(
    app: AppHandle,
    session_id: String,
    format: String,
) -> Result<String, String> {
    // BUG-003: Use inner helper to avoid double-lock
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);
    let session_data = get_session_inner(&conn, &session_id)?;

    match format.as_str() {
        "json" => serde_json::to_string_pretty(&session_data)
            .map_err(|e| format!("JSON serialize: {e}")),
        "markdown" => {
            let mut md = format!(
                "# {}\n*Exported on {}*\n\n",
                session_data.session.title,
                chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
            );
            for msg in &session_data.messages {
                let role = if msg.role == "user" {
                    "User"
                } else {
                    "AeroAgent"
                };
                let model_tag = msg
                    .model
                    .as_ref()
                    .map(|m| format!(" *({m})*"))
                    .unwrap_or_default();
                md.push_str(&format!("### {role}{model_tag}\n{}\n", msg.content));
                let total = msg.tokens_in + msg.tokens_out;
                if total > 0 {
                    md.push_str(&format!(
                        "> {total} tokens{}\n",
                        if msg.cost > 0.0 {
                            format!(" · ${:.4}", msg.cost)
                        } else {
                            String::new()
                        }
                    ));
                }
                md.push('\n');
            }
            md.push_str("---\n*Exported from AeroFTP AeroAgent*\n");
            Ok(md)
        }
        _ => Err("Invalid format. Use 'json' or 'markdown'.".into()),
    }
}

#[tauri::command]
pub async fn chat_history_import(
    app: AppHandle,
    json_data: String,
) -> Result<String, String> {
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);

    let data: SessionWithMessages =
        serde_json::from_str(&json_data).map_err(|e| format!("Invalid JSON: {e}"))?;

    // Wrap import in transaction (SEC-005)
    conn.execute_batch("BEGIN TRANSACTION")
        .map_err(|e| format!("Begin import tx: {e}"))?;

    let result = (|| -> Result<(), String> {
        conn.execute(
            "INSERT INTO sessions (id, title, provider, model, message_count, total_tokens, total_cost, active_branch_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
               title = excluded.title, provider = excluded.provider, model = excluded.model,
               message_count = excluded.message_count, total_tokens = excluded.total_tokens,
               total_cost = excluded.total_cost, updated_at = excluded.updated_at",
            params![
                data.session.id, data.session.title, data.session.provider, data.session.model,
                data.session.message_count, data.session.total_tokens, data.session.total_cost,
                data.active_branch_id, data.session.created_at, data.session.updated_at,
            ],
        )
        .map_err(|e| format!("Import session: {e}"))?;

        for msg in &data.messages {
            conn.execute(
                "INSERT INTO messages (id, session_id, role, content, tool_calls, thinking, tokens_in, tokens_out, cost, model, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(id) DO UPDATE SET
                   content = excluded.content, tool_calls = excluded.tool_calls,
                   thinking = excluded.thinking, tokens_in = excluded.tokens_in,
                   tokens_out = excluded.tokens_out, cost = excluded.cost, model = excluded.model",
                params![
                    msg.id, msg.session_id, msg.role, msg.content, msg.tool_calls,
                    msg.thinking, msg.tokens_in, msg.tokens_out, msg.cost, msg.model, msg.created_at,
                ],
            )
            .map_err(|e| format!("Import message: {e}"))?;
        }
        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")
                .map_err(|e| format!("Commit import: {e}"))?;
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }

    Ok(format!(
        "Imported session '{}' with {} messages",
        data.session.title,
        data.messages.len()
    ))
}

// F4: Dedicated clear-all command — avoids semantic overload of `older_than_days = 0`
#[tauri::command]
pub async fn chat_history_clear_all(app: AppHandle) -> Result<i64, String> {
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap_or(0);

    conn.execute_batch(
        "DELETE FROM branch_messages;
         DELETE FROM branches;
         DELETE FROM messages;
         DELETE FROM sessions;",
    )
    .map_err(|e| format!("Clear all: {e}"))?;

    let _ = conn.execute_batch("PRAGMA optimize;");

    Ok(count)
}

// Branch management commands

#[tauri::command]
pub async fn chat_history_create_branch(
    app: AppHandle,
    session_id: String,
    branch_id: String,
    name: String,
    parent_message_id: String,
    messages: Vec<BranchMessage>,
) -> Result<(), String> {
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);

    let now = chrono::Utc::now().timestamp_millis();

    // Wrap in transaction (SEC-005)
    conn.execute_batch("BEGIN TRANSACTION")
        .map_err(|e| format!("Begin branch tx: {e}"))?;

    let result = (|| -> Result<(), String> {
        conn.execute(
            "INSERT INTO branches (id, session_id, name, parent_message_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![branch_id, session_id, name, parent_message_id, now],
        )
        .map_err(|e| format!("Create branch: {e}"))?;

        for msg in &messages {
            conn.execute(
                "INSERT INTO branch_messages (id, branch_id, role, content, tool_calls, thinking, tokens_in, tokens_out, cost, model, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(id) DO UPDATE SET
                   content = excluded.content, tool_calls = excluded.tool_calls,
                   thinking = excluded.thinking, tokens_in = excluded.tokens_in,
                   tokens_out = excluded.tokens_out, cost = excluded.cost, model = excluded.model",
                params![
                    msg.id, branch_id, msg.role, msg.content, msg.tool_calls,
                    msg.thinking, msg.tokens_in, msg.tokens_out, msg.cost, msg.model, msg.created_at,
                ],
            )
            .map_err(|e| format!("Insert branch message: {e}"))?;
        }

        conn.execute(
            "UPDATE sessions SET active_branch_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![branch_id, now, session_id],
        )
        .map_err(|e| format!("Update active branch: {e}"))?;

        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")
                .map_err(|e| format!("Commit branch: {e}"))?;
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn chat_history_switch_branch(
    app: AppHandle,
    session_id: String,
    branch_id: Option<String>,
) -> Result<Vec<BranchMessage>, String> {
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);

    let now = chrono::Utc::now().timestamp_millis();

    conn.execute(
        "UPDATE sessions SET active_branch_id = ?1, updated_at = ?2 WHERE id = ?3",
        params![branch_id, now, session_id],
    )
    .map_err(|e| format!("Switch branch: {e}"))?;

    if let Some(ref bid) = branch_id {
        let mut stmt = conn
            .prepare(
                "SELECT id, branch_id, role, content, tool_calls, thinking, tokens_in, tokens_out, cost, model, created_at
                 FROM branch_messages WHERE branch_id = ?1 ORDER BY created_at ASC",
            )
            .map_err(|e| format!("Prepare: {e}"))?;

        let messages: Vec<BranchMessage> = stmt
            .query_map(params![bid], |row| {
                Ok(BranchMessage {
                    id: row.get(0)?,
                    branch_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    tool_calls: row.get(4)?,
                    thinking: row.get(5)?,
                    tokens_in: row.get(6)?,
                    tokens_out: row.get(7)?,
                    cost: row.get(8)?,
                    model: row.get(9)?,
                    created_at: row.get(10)?,
                })
            })
            .map_err(|e| format!("Query: {e}"))?
            .filter_map(log_filter_row)
            .collect();

        Ok(messages)
    } else {
        Ok(vec![])
    }
}

#[tauri::command]
pub async fn chat_history_delete_branch(
    app: AppHandle,
    session_id: String,
    branch_id: String,
) -> Result<(), String> {
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);

    conn.execute("DELETE FROM branches WHERE id = ?1", params![branch_id])
        .map_err(|e| format!("Delete branch: {e}"))?;

    conn.execute(
        "UPDATE sessions SET active_branch_id = NULL, updated_at = ?1 WHERE id = ?2 AND active_branch_id = ?3",
        params![chrono::Utc::now().timestamp_millis(), session_id, branch_id],
    )
    .map_err(|e| format!("Clear active branch: {e}"))?;

    Ok(())
}

#[tauri::command]
pub async fn chat_history_save_branch_message(
    app: AppHandle,
    branch_id: String,
    message: BranchMessage,
) -> Result<(), String> {
    let db = app.state::<ChatHistoryDb>();
    let conn = acquire_lock(&db);

    // True upsert (BUG-001)
    conn.execute(
        "INSERT INTO branch_messages (id, branch_id, role, content, tool_calls, thinking, tokens_in, tokens_out, cost, model, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(id) DO UPDATE SET
           content = excluded.content, tool_calls = excluded.tool_calls,
           thinking = excluded.thinking, tokens_in = excluded.tokens_in,
           tokens_out = excluded.tokens_out, cost = excluded.cost, model = excluded.model",
        params![
            message.id,
            branch_id,
            message.role,
            message.content,
            message.tool_calls,
            message.thinking,
            message.tokens_in,
            message.tokens_out,
            message.cost,
            message.model,
            message.created_at,
        ],
    )
    .map_err(|e| format!("Save branch message: {e}"))?;

    Ok(())
}
