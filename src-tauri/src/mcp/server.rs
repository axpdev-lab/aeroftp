//! MCP server core: async event loop, JSON-RPC routing, capability negotiation
//!
//! Handles initialize handshake, tools/list, tools/call, resources/list,
//! resources/read, prompts/list, prompts/get, and notifications.
//!
//! Uses `tokio::select!` for cancellation support and periodic pool eviction.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::mcp::notifier::{extract_progress_token, McpNotifier};
use crate::mcp::pool::ConnectionPool;
use crate::mcp::security::RateLimiter;
use crate::mcp::transport::{StdinReader, StdoutWriter};
use crate::mcp::{prompts, resources, tools};

/// Interval for pool idle eviction (60 seconds).
const EVICTION_INTERVAL_SECS: u64 = 60;

/// Maximum time to wait for in-flight tool calls to complete on shutdown.
const SHUTDOWN_DRAIN_SECS: u64 = 10;

/// Hard wall-clock cap for a single MCP tool call. Prevents a wedged provider
/// (TCP half-open, dead SSH session, slow SFTP) from pinning a connection
/// pool slot indefinitely. Override with `AEROFTP_MCP_TOOL_TIMEOUT_SECS`.
const MCP_TOOL_TIMEOUT_DEFAULT_SECS: u64 = 600;

fn mcp_tool_timeout() -> Duration {
    let secs = std::env::var("AEROFTP_MCP_TOOL_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(MCP_TOOL_TIMEOUT_DEFAULT_SECS);
    Duration::from_secs(secs)
}

/// Key used for per-profile serialization when a tool call does not name a
/// specific server (non-mutating or cross-server operations).
const GLOBAL_SERIALIZATION_KEY: &str = "__aeroftp_global__";

/// Core MCP server that processes JSON-RPC messages.
pub struct McpServerCore {
    profiles: Arc<Vec<Value>>,
    vault_error: Option<String>,
    pool: Arc<ConnectionPool>,
    rate_limiter: Arc<RateLimiter>,
    in_flight: Arc<Mutex<HashMap<String, CancellationToken>>>,
    /// Per-profile serialization mutexes. Each profile has its own mutex so
    /// that concurrent tool calls against the same server are linearized
    /// (prevents e.g. upload racing a preceding `mkdir` on slow NAS targets),
    /// while different servers continue to run in parallel.
    ///
    /// Storage is `Weak<Mutex<()>>`: once every outstanding tool call releases
    /// its `Arc`, the `Weak` can no longer upgrade and the entry is pruned on
    /// next access. Previously a `HashMap<String, Arc<Mutex<()>>>` leaked one
    /// lock per unique server name for the lifetime of the process.
    profile_locks: Arc<Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>>,
    /// JoinSet of currently dispatched tool-call tasks. Used to drain pending
    /// work on shutdown so responses are not dropped when stdin closes.
    pending_tasks: Arc<Mutex<JoinSet<()>>>,
}

impl McpServerCore {
    pub fn new(
        profiles: Vec<Value>,
        vault_error: Option<String>,
        pool: ConnectionPool,
        rate_limiter: RateLimiter,
    ) -> Self {
        Self {
            profiles: Arc::new(profiles),
            vault_error,
            pool: Arc::new(pool),
            rate_limiter: Arc::new(rate_limiter),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            profile_locks: Arc::new(Mutex::new(HashMap::new())),
            pending_tasks: Arc::new(Mutex::new(JoinSet::new())),
        }
    }

    /// Acquire the per-profile serialization mutex. Tool calls against the
    /// same `profile_key` wait for each other, so e.g. a PUT cannot race a
    /// preceding MKDIR on the same server.
    ///
    /// Uses `Weak` storage: the returned strong `Arc` keeps the lock alive
    /// only as long as callers hold it; when the last caller drops it, the
    /// `Weak` stored in the map no longer upgrades and the entry is pruned.
    async fn profile_mutex(&self, profile_key: &str) -> Arc<Mutex<()>> {
        let mut locks = self.profile_locks.lock().await;

        // Try to upgrade the existing weak reference.
        if let Some(weak) = locks.get(profile_key) {
            if let Some(strong) = weak.upgrade() {
                return strong;
            }
        }

        // Opportunistic GC: prune dead Weak entries so the map does not grow
        // monotonically across many unique server names.
        locks.retain(|_, weak| weak.strong_count() > 0);

        let strong = Arc::new(Mutex::new(()));
        locks.insert(profile_key.to_string(), Arc::downgrade(&strong));
        strong
    }

    /// Run the server main loop. Returns exit code (0 = clean shutdown).
    pub async fn run(&mut self) -> i32 {
        let mut reader = StdinReader::new();
        let writer = Arc::new(StdoutWriter::new());
        let mut eviction_interval =
            tokio::time::interval(std::time::Duration::from_secs(EVICTION_INTERVAL_SECS));

        loop {
            tokio::select! {
                biased;

                maybe_line = reader.next_line() => {
                    match maybe_line {
                        None => {
                            eprintln!("[mcp] stdin closed, draining in-flight tasks");
                            self.drain_pending(Duration::from_secs(SHUTDOWN_DRAIN_SECS)).await;
                            return 0;
                        }
                        Some(Err(e)) => {
                            let error_resp = json!({
                                "jsonrpc": "2.0",
                                "id": null,
                                "error": { "code": -32600, "message": e.to_string() }
                            });
                            let _ = writer.write_message(&error_resp).await;
                            continue;
                        }
                        Some(Ok(line)) => {
                            if line.is_empty() {
                                continue;
                            }
                            self.handle_message(&line, Arc::clone(&writer)).await;
                        }
                    }
                }

                _ = eviction_interval.tick() => {
                    self.pool.evict_idle().await;
                }
            }
        }
    }

    /// Wait for all spawned tool-call tasks to finish, up to `timeout`.
    /// Called on shutdown so in-flight responses are not dropped when stdin
    /// closes. Tasks that exceed the timeout are abandoned.
    async fn drain_pending(&self, timeout: Duration) {
        let tasks = Arc::clone(&self.pending_tasks);
        let drain = async move {
            let mut set = tasks.lock().await;
            while set.join_next().await.is_some() {}
        };
        let _ = tokio::time::timeout(timeout, drain).await;
    }

    /// Parse and route a single JSON-RPC message.
    async fn handle_message(&self, line: &str, writer: Arc<StdoutWriter>) {
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("Parse error: {}", e) }
                });
                let _ = writer.write_message(&resp).await;
                return;
            }
        };

        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");

        match method {
            "notifications/initialized" => return,
            "notifications/cancelled" => {
                let reason = req
                    .get("params")
                    .and_then(|params| params.get("reason"))
                    .and_then(|value| value.as_str());

                let Some(request_id) = cancelled_request_key(&req) else {
                    return;
                };

                if let Some(token) = self.in_flight.lock().await.remove(&request_id) {
                    if let Some(reason) = reason {
                        eprintln!(
                            "[mcp] cancellation requested for {}: {}",
                            request_id, reason
                        );
                    } else {
                        eprintln!("[mcp] cancellation requested for {}", request_id);
                    }
                    token.cancel();
                }
                return;
            }
            _ => {}
        }

        if method == "initialize" {
            if let Some(resp) = process_request(
                req,
                Arc::clone(&self.profiles),
                self.vault_error.clone(),
                Arc::clone(&self.pool),
                Arc::clone(&self.rate_limiter),
                None,
            )
            .await
            {
                let _ = writer.write_message(&resp).await;
            }
            return;
        }

        let Some(request_id) = request_id_key(&id) else {
            return;
        };

        let token = CancellationToken::new();
        self.in_flight
            .lock()
            .await
            .insert(request_id.clone(), token.clone());

        // Pick a serialization key so that tool calls against the same server
        // run one at a time. Non-server operations use a shared global bucket
        // that does not block cross-server parallelism.
        let serialization_key = if method == "tools/call" {
            extract_tool_call_server(&req).unwrap_or_else(|| GLOBAL_SERIALIZATION_KEY.to_string())
        } else {
            GLOBAL_SERIALIZATION_KEY.to_string()
        };
        let profile_mutex = self.profile_mutex(&serialization_key).await;

        let profiles = Arc::clone(&self.profiles);
        let vault_error = self.vault_error.clone();
        let pool = Arc::clone(&self.pool);
        let rate_limiter = Arc::clone(&self.rate_limiter);
        let in_flight = Arc::clone(&self.in_flight);

        // Capture the MCP progress token (if any) so long-running tool calls
        // can stream `notifications/progress` back on the shared writer.
        let notifier = if method == "tools/call" {
            Some(McpNotifier::new(
                Arc::clone(&writer),
                extract_progress_token(&req),
            ))
        } else {
            None
        };

        let mut tasks = self.pending_tasks.lock().await;
        // Drain finished handles so the JoinSet does not accumulate completed
        // tasks across long server lifetimes. try_join_next is non-blocking.
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            // Serialize tool calls per-server. The global lock is effectively
            // a single-slot path shared by operations that do not name a
            // specific server (tools/list, resources/*, etc.).
            let _permit = profile_mutex.lock().await;

            let response_future =
                process_request(req, profiles, vault_error, pool, rate_limiter, notifier);
            tokio::pin!(response_future);

            tokio::select! {
                _ = token.cancelled() => {}
                resp = &mut response_future => {
                    if let Some(resp) = resp {
                        let _ = writer.write_message(&resp).await;
                    }
                }
            }

            in_flight.lock().await.remove(&request_id);
        });
    }
}

/// Extract the `server` argument from a tools/call request so that calls
/// against the same profile can be serialized. Returns `None` for tool calls
/// without a server argument (e.g. `aeroftp_list_servers`).
fn extract_tool_call_server(req: &Value) -> Option<String> {
    let params = req.get("params")?;
    let args = params.get("arguments")?;
    let server = args.get("server").and_then(|v| v.as_str())?;
    if server.is_empty() {
        None
    } else {
        Some(format!("server:{}", server.to_lowercase()))
    }
}

/// Validate that the arguments satisfy the tool's `required` field list.
/// Returns `Ok(())` if all required fields are present and non-null, otherwise
/// returns a descriptive error message suitable for the `-32602 Invalid params`
/// JSON-RPC error.
fn validate_required_fields(tool_name: &str, args: &Value, schema: &Value) -> Result<(), String> {
    let required = schema.get("required").and_then(|v| v.as_array());
    let Some(required) = required else {
        return Ok(());
    };
    let mut missing = Vec::new();
    for field in required {
        let Some(key) = field.as_str() else { continue };
        let present = args
            .get(key)
            .map(|v| !(v.is_null() || v.as_str() == Some("")))
            .unwrap_or(false);
        if !present {
            missing.push(key);
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Invalid params for '{}': missing required field{} {}",
            tool_name,
            if missing.len() == 1 { "" } else { "s" },
            missing
                .iter()
                .map(|k| format!("'{}'", k))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

fn request_id_key(id: &Value) -> Option<String> {
    if id.is_null() {
        None
    } else {
        serde_json::to_string(id).ok()
    }
}

fn cancelled_request_key(req: &Value) -> Option<String> {
    req.get("params")
        .and_then(|params| params.get("requestId"))
        .and_then(request_id_key)
}

async fn process_request(
    req: Value,
    profiles: Arc<Vec<Value>>,
    vault_error: Option<String>,
    pool: Arc<ConnectionPool>,
    rate_limiter: Arc<RateLimiter>,
    notifier: Option<McpNotifier>,
) -> Option<Value> {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");

    match method {
        "initialize" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": { "listChanged": false },
                    "resources": { "subscribe": false, "listChanged": false },
                    "prompts": { "listChanged": false }
                },
                "serverInfo": {
                    "name": "aeroftp-mcp",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }
        })),

        "tools/list" => {
            let tool_list: Vec<Value> = tools::tool_definitions()
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": t.input_schema,
                    })
                })
                .collect();
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tool_list }
            }))
        }

        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(json!({}));
            let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));

            let tool_def = tools::tool_definitions()
                .into_iter()
                .find(|t| t.name == tool_name);

            let Some(tool_def) = tool_def else {
                return Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("Unknown tool: {}", tool_name)
                    }
                }));
            };

            // Validate required arguments against the tool's declared schema
            // BEFORE dispatching. Without this, missing fields reach the
            // provider and surface as misleading errors like "bucket required"
            // (the provider checks its own config, not the caller's intent).
            if let Err(msg) = validate_required_fields(tool_name, &args, &tool_def.input_schema) {
                return Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32602,
                        "message": msg,
                    }
                }));
            }

            let exec_future =
                tools::execute_tool(tool_name, &args, &pool, &rate_limiter, notifier.as_ref());
            let (result, is_error) =
                match tokio::time::timeout(mcp_tool_timeout(), exec_future).await {
                    Ok(pair) => pair,
                    Err(_) => {
                        // Timeout: the dispatch task may still be running inside the
                        // provider (we cannot cancel it mid-IO without risking half-
                        // written state), but we release the caller's response slot
                        // so stdin keeps flowing. The pool connection is left in
                        // whatever state the provider produces; the pool eviction
                        // task will reap it on the next idle sweep.
                        return Some(json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": -32000,
                                "message": format!(
                                    "Tool call '{}' exceeded wall-clock timeout of {:?}",
                                    tool_name,
                                    mcp_tool_timeout()
                                )
                            }
                        }));
                    }
                };

            let text = serde_json::to_string_pretty(&result).unwrap_or_default();
            let content = json!([{
                "type": "text",
                "text": text
            }]);

            Some(if is_error {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": content,
                        "isError": true
                    }
                })
            } else {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": content
                    }
                })
            })
        }

        "resources/list" => {
            let listed = resources::resource_list(&profiles, &vault_error);
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "resources": listed }
            }))
        }

        "resources/templates/list" => {
            let templates = resources::resource_templates();
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "resourceTemplates": templates }
            }))
        }

        "resources/read" => {
            let params = req.get("params").cloned().unwrap_or(json!({}));
            let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");

            match resources::read_resource(uri, &profiles, &vault_error, &pool).await {
                Some((mime, text)) => Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "contents": [{
                            "uri": uri,
                            "mimeType": mime,
                            "text": text
                        }]
                    }
                })),
                None => Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32002,
                        "message": format!("Resource not found: {}", uri)
                    }
                })),
            }
        }

        "prompts/list" => {
            let prompt_list = prompts::prompts_list();
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "prompts": prompt_list }
            }))
        }

        "prompts/get" => {
            let params = req.get("params").cloned().unwrap_or(json!({}));
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

            match prompts::get_prompt(name, &arguments) {
                Some(messages) => Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "messages": messages }
                })),
                None => Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("Prompt not found: {}", name)
                    }
                })),
            }
        }

        "ping" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {}
        })),

        _ => {
            if id.is_null() {
                None
            } else {
                Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("Method not found: {}", method)
                    }
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::{cancelled_request_key, process_request, request_id_key};
    use crate::mcp::pool::ConnectionPool;
    use crate::mcp::security::RateLimiter;
    use serde_json::json;

    async fn dispatch(
        req: serde_json::Value,
        profiles: Vec<serde_json::Value>,
    ) -> serde_json::Value {
        process_request(
            req,
            Arc::new(profiles),
            None,
            Arc::new(ConnectionPool::new(10, Duration::from_secs(300))),
            Arc::new(RateLimiter::new()),
            None,
        )
        .await
        .expect("request should return a response")
    }

    #[test]
    fn request_id_key_serializes_non_null_ids() {
        assert_eq!(request_id_key(&json!(42)).as_deref(), Some("42"));
        assert_eq!(
            request_id_key(&json!("req-1")).as_deref(),
            Some("\"req-1\"")
        );
    }

    #[test]
    fn request_id_key_rejects_null() {
        assert_eq!(request_id_key(&json!(null)), None);
    }

    #[test]
    fn cancelled_request_key_extracts_nested_request_id() {
        let req = json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": "abc-123" }
        });

        assert_eq!(cancelled_request_key(&req).as_deref(), Some("\"abc-123\""));
    }

    #[test]
    fn cancelled_request_key_requires_request_id() {
        let req = json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "reason": "user cancelled" }
        });

        assert_eq!(cancelled_request_key(&req), None);
    }

    #[tokio::test]
    async fn initialize_response_advertises_expected_capabilities() {
        let response = dispatch(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "clientInfo": { "name": "test-client", "version": "0.1.0" }
                }
            }),
            vec![],
        )
        .await;

        assert_eq!(response["jsonrpc"], json!("2.0"));
        assert_eq!(response["id"], json!(1));
        assert_eq!(response["result"]["protocolVersion"], json!("2024-11-05"));
        assert_eq!(
            response["result"]["capabilities"]["tools"]["listChanged"],
            json!(false)
        );
        assert_eq!(
            response["result"]["capabilities"]["resources"]["subscribe"],
            json!(false)
        );
        assert_eq!(
            response["result"]["capabilities"]["prompts"]["listChanged"],
            json!(false)
        );
        assert_eq!(
            response["result"]["serverInfo"]["name"],
            json!("aeroftp-mcp")
        );
    }

    #[tokio::test]
    async fn transcript_lists_tools_resources_and_prompts() {
        let profiles = vec![json!({
            "id": "srv_123",
            "name": "Production",
            "protocol": "sftp",
            "host": "prod.example.com",
            "port": 22,
            "username": "deploy",
            "initialPath": "/var/www"
        })];

        let tools = dispatch(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            profiles.clone(),
        )
        .await;
        let tool_list = tools["result"]["tools"].as_array().expect("tools array");
        assert!(tool_list.len() >= 17);
        assert!(tool_list
            .iter()
            .any(|tool| tool["name"] == json!("aeroftp_list_servers")));
        assert!(tool_list
            .iter()
            .any(|tool| tool["name"] == json!("aeroftp_delete")));
        assert!(tool_list
            .iter()
            .any(|tool| tool["name"] == json!("aeroftp_close_connection")));

        let resources = dispatch(
            json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/list" }),
            profiles.clone(),
        )
        .await;
        let resource_list = resources["result"]["resources"]
            .as_array()
            .expect("resources array");
        assert_eq!(resource_list.len(), 5);
        assert!(resource_list
            .iter()
            .any(|resource| resource["uri"] == json!("aeroftp://profiles")));
        assert!(resource_list
            .iter()
            .any(|resource| resource["uri"] == json!("aeroftp://profiles/srv_123")));

        let profile_resource = dispatch(
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "resources/read",
                "params": { "uri": "aeroftp://profiles/srv_123" }
            }),
            profiles.clone(),
        )
        .await;
        let profile_text = profile_resource["result"]["contents"][0]["text"]
            .as_str()
            .expect("profile resource text");
        assert!(profile_text.contains("Production"));
        assert!(profile_text.contains("prod.example.com"));

        let prompts = dispatch(
            json!({ "jsonrpc": "2.0", "id": 5, "method": "prompts/list" }),
            profiles.clone(),
        )
        .await;
        let prompt_list = prompts["result"]["prompts"]
            .as_array()
            .expect("prompts array");
        assert_eq!(prompt_list.len(), 4);
        assert!(prompt_list
            .iter()
            .any(|prompt| prompt["name"] == json!("deploy_files")));

        let deploy_prompt = dispatch(
            json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "prompts/get",
                "params": {
                    "name": "deploy_files",
                    "arguments": {
                        "server": "Production",
                        "local_dir": "./dist",
                        "remote_dir": "/var/www"
                    }
                }
            }),
            profiles,
        )
        .await;
        let deploy_text = deploy_prompt["result"]["messages"][0]["content"]["text"]
            .as_str()
            .expect("deploy prompt text");
        assert!(deploy_text.contains("aeroftp_list_servers"));
        assert!(deploy_text.contains("aeroftp_upload_file"));
        assert!(deploy_text.contains("/var/www"));
    }

    #[tokio::test]
    async fn transcript_returns_expected_errors_for_unknown_entities() {
        let unknown_tool = dispatch(
            json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": {
                    "name": "aeroftp_not_real",
                    "arguments": {}
                }
            }),
            vec![],
        )
        .await;
        assert_eq!(unknown_tool["error"]["code"], json!(-32601));
        assert!(unknown_tool["error"]["message"]
            .as_str()
            .expect("tool error message")
            .contains("Unknown tool"));

        let unknown_resource = dispatch(
            json!({
                "jsonrpc": "2.0",
                "id": 8,
                "method": "resources/read",
                "params": { "uri": "aeroftp://profiles/does-not-exist" }
            }),
            vec![],
        )
        .await;
        assert_eq!(unknown_resource["error"]["code"], json!(-32002));

        let unknown_prompt = dispatch(
            json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "prompts/get",
                "params": { "name": "not-a-prompt", "arguments": {} }
            }),
            vec![],
        )
        .await;
        assert_eq!(unknown_prompt["error"]["code"], json!(-32601));
        assert!(unknown_prompt["error"]["message"]
            .as_str()
            .expect("prompt error message")
            .contains("Prompt not found"));
    }
}
