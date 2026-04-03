//! MCP server core — async event loop, JSON-RPC routing, capability negotiation
//!
//! Handles initialize handshake, tools/list, tools/call, resources/list,
//! resources/read, prompts/list, prompts/get, and notifications.
//!
//! Uses `tokio::select!` for cancellation support and periodic pool eviction.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::mcp::pool::ConnectionPool;
use crate::mcp::security::RateLimiter;
use crate::mcp::transport::{StdinReader, StdoutWriter};
use crate::mcp::{prompts, resources, tools};

/// Interval for pool idle eviction (60 seconds).
const EVICTION_INTERVAL_SECS: u64 = 60;

/// Core MCP server that processes JSON-RPC messages.
pub struct McpServerCore {
    profiles: Arc<Vec<Value>>,
    vault_error: Option<String>,
    pool: Arc<ConnectionPool>,
    rate_limiter: Arc<RateLimiter>,
    in_flight: Arc<Mutex<HashMap<String, CancellationToken>>>,
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
        }
    }

    /// Run the server main loop. Returns exit code (0 = clean shutdown).
    pub async fn run(&mut self) -> i32 {
        let mut reader = StdinReader::new();
        let writer = Arc::new(StdoutWriter::new());
        let mut eviction_interval = tokio::time::interval(
            std::time::Duration::from_secs(EVICTION_INTERVAL_SECS),
        );

        loop {
            tokio::select! {
                biased;

                maybe_line = reader.next_line() => {
                    match maybe_line {
                        None => {
                            eprintln!("[mcp] stdin closed, shutting down");
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
                        eprintln!("[mcp] cancellation requested for {}: {}", request_id, reason);
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
            ).await {
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

        let profiles = Arc::clone(&self.profiles);
        let vault_error = self.vault_error.clone();
        let pool = Arc::clone(&self.pool);
        let rate_limiter = Arc::clone(&self.rate_limiter);
        let in_flight = Arc::clone(&self.in_flight);

        tokio::spawn(async move {
            let response_future = process_request(req, profiles, vault_error, pool, rate_limiter);
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

            let tool_exists = tools::tool_definitions()
                .iter()
                .any(|t| t.name == tool_name);

            if !tool_exists {
                return Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("Unknown tool: {}", tool_name)
                    }
                }));
            }

            let (result, is_error) =
                tools::execute_tool(tool_name, &args, &pool, &rate_limiter).await;

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

    async fn dispatch(req: serde_json::Value, profiles: Vec<serde_json::Value>) -> serde_json::Value {
        process_request(
            req,
            Arc::new(profiles),
            None,
            Arc::new(ConnectionPool::new(10, Duration::from_secs(300))),
            Arc::new(RateLimiter::new()),
        )
        .await
        .expect("request should return a response")
    }

    #[test]
    fn request_id_key_serializes_non_null_ids() {
        assert_eq!(request_id_key(&json!(42)).as_deref(), Some("42"));
        assert_eq!(request_id_key(&json!("req-1")).as_deref(), Some("\"req-1\""));
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
        assert_eq!(response["result"]["capabilities"]["tools"]["listChanged"], json!(false));
        assert_eq!(response["result"]["capabilities"]["resources"]["subscribe"], json!(false));
        assert_eq!(response["result"]["capabilities"]["prompts"]["listChanged"], json!(false));
        assert_eq!(response["result"]["serverInfo"]["name"], json!("aeroftp-mcp"));
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
        assert_eq!(tool_list.len(), 16);
        assert!(tool_list.iter().any(|tool| tool["name"] == json!("aeroftp_list_servers")));
        assert!(tool_list.iter().any(|tool| tool["name"] == json!("aeroftp_delete")));

        let resources = dispatch(
            json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/list" }),
            profiles.clone(),
        )
        .await;
        let resource_list = resources["result"]["resources"].as_array().expect("resources array");
        assert_eq!(resource_list.len(), 5);
        assert!(resource_list.iter().any(|resource| resource["uri"] == json!("aeroftp://profiles")));
        assert!(resource_list.iter().any(|resource| resource["uri"] == json!("aeroftp://profiles/srv_123")));

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
        let prompt_list = prompts["result"]["prompts"].as_array().expect("prompts array");
        assert_eq!(prompt_list.len(), 4);
        assert!(prompt_list.iter().any(|prompt| prompt["name"] == json!("deploy_files")));

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
