//! CLI implementations of AI Core traits
//!
//! Used by the aeroftp-cli binary when running in agent mode.
//! No Tauri dependency: pure Rust.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use serde_json::Value;

use super::credential_provider::{
    CredentialProvider, ProviderExtraOptions, ServerCredentials, ServerProfile,
};
use super::event_sink::{EventSink, ToolProgress};
use super::remote_backend::RemoteBackend;
use crate::ai_stream::StreamChunk;

/// MUV-3: read a `server_<id>` secret for the active user (per-user partition
/// with fallback to the legacy vault). The CLI AI-core RemoteBackend has no
/// `Cli` in scope, so it resolves against the persisted active user.
fn read_active_server_secret(
    store: &crate::credential_store::CredentialStore,
    server_id: &str,
) -> Option<String> {
    let key = format!("server_{}", server_id);
    match crate::user_partitions::cli_get_active_user(store)
        .ok()
        .flatten()
        .map(|u| u.id)
    {
        Some(uid) => crate::user_partitions::cli_read_credential_with_fallback(store, uid, &key)
            .ok()
            .flatten()
            .map(|s| s.to_string()),
        None => store.get(&key).ok(),
    }
}

// ─── CliEventSink ─────────────────────────────────────────────────────

/// CLI event sink: writes streaming output to stdout/stderr.
pub struct CliEventSink {
    pub json_mode: bool,
}

impl EventSink for CliEventSink {
    fn emit_stream_chunk(&self, _stream_id: &str, chunk: &StreamChunk) {
        if self.json_mode {
            if let Ok(json) = serde_json::to_string(chunk) {
                println!("{}", json);
            }
        } else {
            if !chunk.content.is_empty() {
                print!("{}", chunk.content);
            }
            if let Some(ref thinking) = chunk.thinking {
                if !thinking.is_empty() {
                    eprint!("\x1b[2m{}\x1b[0m", thinking);
                }
            }
            if chunk.done {
                println!();
            }
        }
    }

    fn emit_tool_progress(&self, progress: &ToolProgress) {
        if self.json_mode {
            if let Ok(json) = serde_json::to_string(progress) {
                eprintln!("{}", json);
            }
        } else {
            eprint!(
                "\r  [{}/{}] {}",
                progress.current, progress.total, progress.item
            );
            if progress.current == progress.total {
                eprintln!();
            }
        }
    }

    fn emit_app_control(&self, _event_name: &str, _payload: &Value) {
        // CLI ignores GUI-only events (theme, sync control)
    }
}

// ─── StdioEventSink ───────────────────────────────────────────────────

/// JSON-RPC event sink for orchestration mode: line-delimited JSON on stdout.
pub struct StdioEventSink;

impl EventSink for StdioEventSink {
    fn emit_stream_chunk(&self, stream_id: &str, chunk: &StreamChunk) {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "stream/chunk",
            "params": {
                "stream_id": stream_id,
                "content": chunk.content,
                "done": chunk.done,
                "tool_calls": chunk.tool_calls,
                "thinking": chunk.thinking,
                "input_tokens": chunk.input_tokens,
                "output_tokens": chunk.output_tokens,
            }
        });
        println!("{}", msg);
    }

    fn emit_tool_progress(&self, progress: &ToolProgress) {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "progress",
            "params": progress
        });
        println!("{}", msg);
    }

    fn emit_app_control(&self, event_name: &str, payload: &Value) {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "app_control",
            "params": {
                "event": event_name,
                "payload": payload
            }
        });
        println!("{}", msg);
    }
}

// ─── CliCredentialProvider ─────────────────────────────────────────────

/// CLI credential provider: vault cache (if open) → env vars (AEROFTP_HOST/USER/PASS).
pub struct CliCredentialProvider;

/// The saved servers of the active user, the list every surface resolves a
/// server name against. AGENT-04: the active user partition (multi-user mode)
/// first, so discovery matches `profiles --json` and the connect path, then the
/// global profile list of a single-user install.
pub(crate) fn active_server_profiles(
    store: &crate::credential_store::CredentialStore,
) -> Result<Vec<ServerProfile>, String> {
    let profiles: Vec<Value> = match crate::user_partitions::cli_list_active_server_profiles(store)
    {
        Ok(p) => p,
        Err(e) if e == "USER_LOCKED" || e == "NO_ACTIVE_USER" => return Err(e),
        Err(_) => match store.get("config_server_profiles") {
            Ok(json_str) => serde_json::from_str::<Vec<Value>>(&json_str).unwrap_or_default(),
            Err(_) => return Ok(Vec::new()),
        },
    };
    Ok(profiles
        .iter()
        .filter_map(ServerProfile::from_profile_json)
        .collect())
}

impl CredentialProvider for CliCredentialProvider {
    fn list_servers(&self) -> Result<Vec<ServerProfile>, String> {
        // The vault may be open from a previous unlock; without it, no server.
        match crate::credential_store::CredentialStore::from_cache() {
            Some(store) => active_server_profiles(&store),
            None => Ok(Vec::new()),
        }
    }

    fn get_credentials(&self, server_id: &str) -> Result<ServerCredentials, String> {
        // 1. Try vault (MUV-3: active user's partition, vault fallback)
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            if let Some(json_str) = read_active_server_secret(&store, server_id) {
                #[derive(serde::Deserialize)]
                struct Creds {
                    #[serde(default)]
                    server: String,
                    #[serde(default)]
                    username: String,
                    #[serde(default)]
                    password: String,
                }
                if let Ok(c) = serde_json::from_str::<Creds>(&json_str) {
                    return Ok(ServerCredentials {
                        server: c.server,
                        username: c.username,
                        password: c.password,
                    });
                }
            }
        }
        // 2. Try env vars
        if let (Ok(host), Ok(user)) = (std::env::var("AEROFTP_HOST"), std::env::var("AEROFTP_USER"))
        {
            let pass = std::env::var("AEROFTP_PASS").unwrap_or_default();
            return Ok(ServerCredentials {
                server: host,
                username: user,
                password: pass,
            });
        }
        Err(format!("No credentials found for server '{}'. Open vault or set AEROFTP_HOST/AEROFTP_USER/AEROFTP_PASS env vars.", server_id))
    }

    fn get_extra_options(&self, server_id: &str) -> Result<ProviderExtraOptions, String> {
        const SENSITIVE_KEYS: &[&str] = &[
            "password",
            "access_token",
            "refresh_token",
            "api_key",
            "secret",
            "token",
            "oauth_token",
            "oauth_token_secret",
            "server",
            "username",
        ];

        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            if let Some(json_str) = read_active_server_secret(&store, server_id) {
                let val: Value = serde_json::from_str(&json_str).unwrap_or(serde_json::json!({}));
                let mut opts = ProviderExtraOptions::new();
                if let Some(obj) = val.as_object() {
                    for (k, v) in obj {
                        let lower = k.to_lowercase();
                        if SENSITIVE_KEYS.iter().any(|s| lower == *s) {
                            continue;
                        }
                        if let Some(s) = v.as_str() {
                            opts.insert(k.clone(), s.to_string());
                        }
                    }
                }
                return Ok(opts);
            }
        }
        Ok(ProviderExtraOptions::new())
    }
}

// ─── CliToolCtx ───────────────────────────────────────────────────────

use crate::ai_core::mcp_impl::McpRemoteBackend;
use crate::ai_core::tools::{Surfaces, ToolCtx};
use crate::mcp::pool::ConnectionPool;
use std::sync::{Arc, LazyLock};

/// The saved-server connections of the CLI agent, shared by every tool call
/// of the process the way the MCP server shares its own: the agent reaches a
/// saved server through the same vault and the same backend as `aeroftp-cli
/// mcp`, and a second call to one server reuses its connection.
static CLI_AGENT_POOL: LazyLock<Arc<ConnectionPool>> =
    LazyLock::new(|| Arc::new(ConnectionPool::new(10, std::time::Duration::from_secs(300))));

pub struct CliToolCtx {
    pub sink: CliEventSink,
    pub creds: CliCredentialProvider,
    /// Cwd snapshot al momento della costruzione, usato come base per
    /// risolvere path relativi. Mirror del legacy CLI `resolve_path`
    /// che faceva `std::env::current_dir().join(path)`.
    pub cwd: Option<String>,
    /// The connection identity each approved profile id had when the call
    /// was approved: the pool refuses to reach a profile that now connects
    /// elsewhere ([`crate::ai_core::tools::change_targets`]).
    pub approved_identities: std::collections::HashMap<String, String>,
}

impl CliToolCtx {
    /// Costruttore con `cwd` automatica. Se `current_dir()` fallisce
    /// (caso raro: dir cancellata), `cwd` resta `None` e il dispatcher
    /// userà il path inalterato (stesso comportamento del legacy CLI
    /// con `unwrap_or_else(|_| path.to_string())`).
    pub fn new(sink: CliEventSink, creds: CliCredentialProvider) -> Self {
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string());
        Self {
            sink,
            creds,
            cwd,
            approved_identities: std::collections::HashMap::new(),
        }
    }

    /// The context of a call approved for `targets`: each profile it names is
    /// reached only while it still connects where it did at the approval.
    pub fn approved_for(mut self, targets: &[crate::ai_core::tools::ApprovedTarget]) -> Self {
        self.approved_identities = targets
            .iter()
            .filter_map(|target| target.profile.clone())
            .collect();
        self
    }
}

#[async_trait]
impl ToolCtx for CliToolCtx {
    fn event_sink(&self) -> &dyn EventSink {
        &self.sink
    }
    fn credentials(&self) -> &dyn CredentialProvider {
        &self.creds
    }
    async fn remote_backend(&self, server_id: &str) -> Result<Arc<dyn RemoteBackend>, String> {
        // Nothing connects here: the pool opens the saved server when a tool
        // first uses the backend.
        Ok(Arc::new(
            McpRemoteBackend::new(Arc::clone(&CLI_AGENT_POOL), server_id.to_string())
                .expecting(self.approved_identities.get(server_id).cloned()),
        ))
    }
    fn context_local_path(&self) -> Option<&str> {
        self.cwd.as_deref()
    }
    fn approved_identity(&self, profile_id: &str) -> Option<&str> {
        self.approved_identities.get(profile_id).map(String::as_str)
    }
    fn surface(&self) -> Surfaces {
        Surfaces::CLI
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The remote tools the CLI agent offers found no backend in this context
    /// and answered "not available in CLI mode". Each now gets one for its
    /// server over the process-wide pool. The test does not connect: that
    /// would open the vault.
    #[tokio::test]
    async fn the_cli_agent_context_gives_the_remote_tools_a_backend() {
        let ctx = CliToolCtx::new(CliEventSink { json_mode: true }, CliCredentialProvider);
        assert!(ctx.remote_backend("srv-a").await.is_ok());
        assert!(ctx.remote_backend("srv-b").await.is_ok());
    }
}
