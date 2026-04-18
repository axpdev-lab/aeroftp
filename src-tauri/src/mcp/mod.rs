//! MCP Server module — Model Context Protocol for AeroFTP
//!
//! Exposes AeroFTP's 27 storage providers via JSON-RPC 2.0 over stdio,
//! compatible with Claude Desktop, Cursor, VS Code, and any MCP client.
//!
//! Architecture:
//! ```text
//! MCP Client (Claude Desktop, Cursor, etc.)
//!       |  JSON-RPC 2.0 (async stdio)
//!       v
//!   McpServer::run()
//!       |
//!   server.rs  — request routing, capability negotiation
//!   tools.rs   — 16 curated tools (12 core + 4 extended)
//!   resources.rs — profiles, status, capabilities, connections
//!   prompts.rs — 4 prompt templates
//!   pool.rs    — connection pooling (HashMap<String, Mutex<Box<dyn StorageProvider>>>)
//!   security.rs — path validation, rate limiting, audit logging
//!   transport.rs — async stdio framing
//! ```

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

pub mod notifier;
pub mod pool;
pub mod prompts;
pub mod resources;
pub mod security;
pub mod server;
pub mod tools;
pub mod transport;

use crate::credential_store::CredentialStore;

/// Configuration for the MCP server.
pub struct McpConfig {
    /// Maximum concurrent pooled connections (default: 10).
    pub max_connections: usize,
    /// Idle timeout for pooled connections in seconds (default: 300).
    pub idle_timeout_secs: u64,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            max_connections: 10,
            idle_timeout_secs: 300,
        }
    }
}

/// Top-level MCP server entry point.
///
/// Call `McpServer::new(config).run().await` from the CLI binary.
pub struct McpServer {
    config: McpConfig,
}

impl McpServer {
    pub fn new(config: McpConfig) -> Self {
        Self { config }
    }

    /// Run the MCP server, reading JSON-RPC from stdin and writing to stdout.
    /// Blocks until stdin is closed (EOF) or a fatal error occurs.
    /// Returns exit code (0 = clean shutdown).
    pub async fn run(self) -> i32 {
        // Prime the credential-store cache. Without this step the cache is
        // empty for a freshly-spawned MCP subprocess and every tool call that
        // needs vault credentials returns "Vault not open" — even when the
        // user is in auto-unlock keyring mode. If a master password is
        // required and no AEROFTP_MASTER_PASSWORD is set, we fall through
        // with a descriptive vault_error so tools can still report the right
        // reason to the AI client.
        let init_error = match CredentialStore::init() {
            Ok(status) if status == "OK" => None,
            Ok(status) if status == "MASTER_PASSWORD_REQUIRED" => {
                if let Ok(master) = std::env::var("AEROFTP_MASTER_PASSWORD") {
                    match CredentialStore::unlock_with_master(&master) {
                        Ok(()) => None,
                        Err(e) => Some(format!("Failed to unlock vault with AEROFTP_MASTER_PASSWORD: {}", e)),
                    }
                } else {
                    Some(
                        "Vault requires master password but AEROFTP_MASTER_PASSWORD env var \
                         is not set. MCP tool calls that need credentials will fail."
                            .to_string(),
                    )
                }
            }
            Ok(other) => Some(format!("Unexpected vault init status: {}", other)),
            Err(e) => Some(format!("Vault init failed: {}", e)),
        };

        // Load vault profiles once at startup
        let (profiles, vault_error) = match load_safe_profiles() {
            Ok(p) => (p, init_error),
            Err(e) => {
                // If init itself failed, surface that reason instead of the
                // generic "vault not open" message.
                let combined = match init_error {
                    Some(reason) => reason,
                    None => e,
                };
                (vec![], Some(combined))
            }
        };

        let pool = pool::ConnectionPool::new(
            self.config.max_connections,
            std::time::Duration::from_secs(self.config.idle_timeout_secs),
        );

        let rate_limiter = security::RateLimiter::new();

        let mut srv = server::McpServerCore::new(profiles, vault_error, pool, rate_limiter);
        srv.run().await
    }
}

/// Load safe (no-password) profiles from the cached vault.
fn load_safe_profiles() -> Result<Vec<serde_json::Value>, String> {
    let store = CredentialStore::from_cache().ok_or_else(|| {
        "Vault not open. Set AEROFTP_MASTER_PASSWORD or open vault first.".to_string()
    })?;
    let profiles_json = store
        .get("config_server_profiles")
        .map_err(|e| format!("Failed to read profiles: {}", e))?;
    let profiles: Vec<serde_json::Value> = serde_json::from_str(&profiles_json)
        .map_err(|e| format!("Failed to parse profiles: {}", e))?;

    Ok(profiles
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                "name": p.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed"),
                "protocol": p.get("protocol").and_then(|v| v.as_str()).unwrap_or(""),
                "host": p.get("host").and_then(|v| v.as_str()).unwrap_or(""),
                "port": p.get("port").and_then(|v| v.as_u64()).unwrap_or(0),
                "username": p.get("username").and_then(|v| v.as_str()).unwrap_or(""),
                "initialPath": p.get("initialPath").and_then(|v| v.as_str()).unwrap_or("/"),
            })
        })
        .collect())
}
