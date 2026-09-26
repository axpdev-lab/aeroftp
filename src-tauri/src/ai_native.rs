// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Foreground provider contracts and opaque turn replay. No durable storage.

use crate::ai::{AIError, AIProviderType, AIRequest, AIResponse, AIToolCall};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Serialize, Deserialize)]
pub struct NativeTurn {
    provider: AIProviderType,
    model: String,
    endpoint: String,
    scope: String,
    transport: String,
    payload: Value,
}

// Never print opaque thinking/signatures through AIRequest/AIResponse Debug.
impl std::fmt::Debug for NativeTurn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeTurn(<opaque>)")
    }
}

fn invalid(message: &str) -> AIError {
    AIError::InvalidResponse(message.to_owned())
}

fn complete_reason(reason: Option<&str>) -> bool {
    matches!(
        reason,
        Some("stop" | "tool_calls" | "end_turn" | "tool_use" | "stop_sequence")
    )
}

pub(crate) fn transport(request: &AIRequest) -> &'static str {
    match request.provider_type {
        AIProviderType::OpenAI if request.use_responses_api.unwrap_or(false) => "responses",
        AIProviderType::Anthropic => "anthropic",
        _ => "chat",
    }
}

pub(crate) fn capture(request: &AIRequest, payload: Value) -> Option<NativeTurn> {
    request
        .turn_scope
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|scope| NativeTurn {
            provider: request.provider_type.clone(),
            model: request.model.clone(),
            endpoint: request.base_url.trim_end_matches('/').to_owned(),
            scope: scope.clone(),
            transport: transport(request).to_owned(),
            payload,
        })
}

pub(crate) fn replay<'a>(request: &AIRequest, turn: &'a NativeTurn) -> Result<&'a Value, AIError> {
    if turn.provider != request.provider_type
        || turn.model != request.model
        || turn.endpoint != request.base_url.trim_end_matches('/')
        || Some(&turn.scope) != request.turn_scope.as_ref()
        || turn.scope.is_empty()
        || turn.transport != transport(request)
    {
        return Err(invalid(
            "Native turn belongs to a different model, endpoint or foreground turn",
        ));
    }
    Ok(&turn.payload)
}

pub(crate) fn validate_history(request: &AIRequest) -> Result<(), AIError> {
    let mut size = 0;
    let mut pending = std::collections::BTreeSet::new();
    let mut native_seen = false;
    for message in &request.messages {
        if message.role == "tool" && native_seen {
            let id = message
                .tool_call_id
                .as_ref()
                .ok_or_else(|| invalid("Missing native tool result ID"))?;
            if !pending.remove(id) {
                return Err(invalid("Unexpected or duplicate native tool result"));
            }
        } else if !pending.is_empty() {
            return Err(invalid(
                "Native tool results must immediately follow their assistant turn",
            ));
        }
        if let Some(turn) = &message.native_turn {
            if message.role != "assistant" {
                return Err(invalid("Native state is only valid on assistant messages"));
            }
            native_seen = true;
            let payload = replay(request, turn)?;
            let calls: Vec<&Value> = match transport(request) {
                "responses" => payload
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|v| v["type"] == "function_call")
                    .collect(),
                "anthropic" => payload
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|v| v["type"] == "tool_use")
                    .collect(),
                _ => payload["tool_calls"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .collect(),
            };
            for call in calls {
                let field = if transport(request) == "responses" {
                    "call_id"
                } else {
                    "id"
                };
                let id = call[field]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| invalid("Missing native tool ID"))?;
                if !pending.insert(id.to_owned()) {
                    return Err(invalid("Duplicate native tool call ID"));
                }
            }
            size += payload.to_string().len();
            if size > 16 * 1024 * 1024 {
                return Err(invalid("Native foreground history exceeds 16 MiB"));
            }
        }
    }
    if native_seen {
        for result in request.tool_results.iter().flatten() {
            if !pending.remove(&result.tool_call_id) {
                return Err(invalid("Unexpected or duplicate native tool result"));
            }
        }
    }
    if !pending.is_empty() {
        return Err(invalid("Missing native tool results"));
    }
    Ok(())
}

pub(crate) fn modern_anthropic(request: &AIRequest) -> bool {
    request.provider_type == AIProviderType::Anthropic
        && matches!(
            request.model.as_str(),
            "claude-opus-5-5" | "claude-fable-5-1"
        )
}

pub(crate) fn modern_chat(request: &AIRequest) -> bool {
    matches!(
        (&request.provider_type, request.model.as_str()),
        (AIProviderType::Kimi, "kimi-k3")
            | (AIProviderType::Xai, "grok-4.7")
            | (
                AIProviderType::OpenAI,
                "gpt-6-astra" | "gpt-6-sol" | "gpt-6-luna"
            )
    ) && !request.use_responses_api.unwrap_or(false)
}

/// Budget sliders are translated only for reviewed provider/model contracts.
/// Explicit effort is validated, never silently downgraded.
pub(crate) fn effort(request: &AIRequest) -> Result<Option<String>, AIError> {
    let model = request.model.as_str();
    let allowed: &[&str] = match (&request.provider_type, model) {
        (AIProviderType::OpenAI, "gpt-6-astra") => &["low", "medium", "high", "xhigh", "max"],
        (
            AIProviderType::OpenAI,
            "gpt-6-sol" | "gpt-6-luna" | "gpt-5.6" | "gpt-5.6-sol" | "gpt-5.6-terra"
            | "gpt-5.6-luna",
        ) => &["none", "low", "medium", "high", "xhigh", "max"],
        (AIProviderType::Kimi, "kimi-k3") => &["low", "high", "max"],
        (AIProviderType::Xai, "grok-4.7") => &["low", "medium", "high", "xhigh"],
        (AIProviderType::Anthropic, "claude-opus-5-5" | "claude-fable-5-1") => {
            &["low", "medium", "high", "xhigh", "max"]
        }
        _ => &[],
    };
    if let Some(explicit) = &request.reasoning_effort {
        if !allowed.contains(&explicit.as_str()) {
            return Err(invalid(
                "Unsupported reasoning effort for this provider/model",
            ));
        }
        return Ok(Some(explicit.clone()));
    }
    let Some(budget) = request.thinking_budget else {
        return Ok(None);
    };
    if allowed.is_empty() {
        return Ok(None);
    }
    let mapped = match budget {
        0 if allowed.contains(&"none") => "none",
        0..=5_000 => "low",
        5_001..=20_000 if allowed.contains(&"medium") => "medium",
        5_001..=50_000 => "high",
        50_001..=99_999 if allowed.contains(&"xhigh") => "xhigh",
        _ if allowed.contains(&"max") => "max",
        _ => "xhigh",
    };
    Ok(Some(mapped.to_owned()))
}

pub(crate) fn chat_body(request: &AIRequest, stream: bool) -> Result<Value, AIError> {
    validate_history(request)?;
    let reasoning = effort(request)?;
    let has_tools = request.tools.as_ref().is_some_and(|t| !t.is_empty());
    if request.provider_type == AIProviderType::OpenAI
        && has_tools
        && (request.model == "gpt-6-astra" || reasoning.as_deref() != Some("none"))
    {
        return Err(invalid(
            "This model requires Responses for reasoning with tools; enable OpenAI Responses",
        ));
    }
    if request.provider_type == AIProviderType::Kimi && request.web_search.unwrap_or(false) {
        return Err(invalid(
            "Kimi K3 hosted web search is not supported by this adapter",
        ));
    }
    let mut messages = Vec::new();
    for message in &request.messages {
        if let Some(turn) = &message.native_turn {
            let native = replay(request, turn)?;
            if native["role"] != "assistant" {
                return Err(invalid("Invalid native assistant message"));
            }
            messages.push(native.clone());
        } else {
            let mut value = json!({"role":message.role,"content":message.to_openai_content()});
            if let Some(id) = &message.tool_call_id {
                value["tool_call_id"] = json!(id);
            }
            if let Some(calls) = &message.tool_calls_echo {
                value["tool_calls"] = json!(calls.iter().map(|c| json!({"id":c.id,"type":"function","function":{"name":c.name,"arguments":c.arguments}})).collect::<Vec<_>>());
            }
            messages.push(value);
        }
    }
    if let Some(results) = &request.tool_results {
        messages.extend(
            results
                .iter()
                .map(|r| json!({"role":"tool","tool_call_id":r.tool_call_id,"content":r.content})),
        );
    }
    let mut body = json!({"model":request.model,"messages":messages,"stream":stream});
    if let Some(max) = request.max_tokens {
        let key = if request.provider_type == AIProviderType::Xai {
            "max_tokens"
        } else {
            "max_completion_tokens"
        };
        body[key] = json!(max);
    }
    if let Some(value) = reasoning {
        body["reasoning_effort"] = json!(value);
    }
    if let Some(tools) = &request.tools {
        // AeroAgent has optional schema fields: do not falsely claim strict mode.
        body["tools"] = json!(tools.iter().map(|t| json!({"type":"function","function":{"name":t.name,"description":t.description,"parameters":t.parameters}})).collect::<Vec<_>>());
    }
    if stream {
        body["stream_options"] = json!({"include_usage":true});
    }
    // All contracts here are reasoning models; fixed/incompatible sampling omitted.
    Ok(body)
}

pub(crate) fn anthropic_body(request: &AIRequest, stream: bool) -> Result<Value, AIError> {
    validate_history(request)?;
    let mut messages: Vec<Value> = Vec::new();
    for message in request
        .messages
        .iter()
        .filter(|m| m.role != "system" && m.role != "developer")
    {
        if message.role == "tool" {
            let id = message
                .tool_call_id
                .as_ref()
                .ok_or_else(|| invalid("Missing tool result ID"))?;
            let block = json!({"type":"tool_result","tool_use_id":id,"content":message.content});
            if let Some(last) = messages
                .last_mut()
                .filter(|m| m["role"] == "user" && m["content"].is_array())
            {
                last["content"].as_array_mut().unwrap().push(block);
            } else {
                messages.push(json!({"role":"user","content":[block]}));
            }
        } else {
            let content = if let Some(turn) = &message.native_turn {
                let value = replay(request, turn)?;
                if !value.is_array() {
                    return Err(invalid("Invalid native Anthropic content"));
                }
                value.clone()
            } else if message.role == "assistant" && message.tool_calls_echo.is_some() {
                // Earlier foreground runs can contain persisted tool records,
                // but never opaque thinking. Keep their call/result pairing.
                let mut blocks = Vec::new();
                if !message.content.is_empty() {
                    blocks.push(json!({"type":"text","text":message.content}));
                }
                for call in message.tool_calls_echo.iter().flatten() {
                    let input: Value = serde_json::from_str(&call.arguments)
                        .map_err(|_| invalid("Invalid historical tool arguments"))?;
                    blocks.push(
                        json!({"type":"tool_use","id":call.id,"name":call.name,"input":input}),
                    );
                }
                json!(blocks)
            } else {
                message.to_anthropic_content()
            };
            messages.push(json!({"role":message.role,"content":content}));
        }
    }
    if let Some(results) = &request.tool_results {
        if !results.is_empty() {
            messages.push(json!({"role":"user","content":results.iter().map(|r| json!({"type":"tool_result","tool_use_id":r.tool_call_id,"content":r.content})).collect::<Vec<_>>()}));
        }
    }
    let system = request
        .messages
        .iter()
        .filter(|m| m.role == "system" || m.role == "developer")
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut body = json!({"model":request.model,"messages":messages,"max_tokens":request.max_tokens.unwrap_or(4096),"stream":stream,"thinking":{"type":"adaptive","display":"summarized"}});
    if !system.is_empty() {
        body["system"] =
            json!([{"type":"text","text":system,"cache_control":{"type":"ephemeral"}}]);
    }
    if let Some(value) = effort(request)? {
        body["output_config"] = json!({"effort":value});
    }
    if let Some(tools) = &request.tools {
        let mut tools = tools
            .iter()
            .map(|t| json!({"name":t.name,"description":t.description,"input_schema":t.parameters}))
            .collect::<Vec<_>>();
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = json!({"type":"ephemeral"});
        }
        body["tools"] = json!(tools);
    }
    Ok(body)
}

pub(crate) fn parse(request: &AIRequest, value: &Value) -> Result<AIResponse, AIError> {
    if value.get("error").is_some() {
        return Err(invalid("Provider returned an error response"));
    }
    let anthropic = modern_anthropic(request);
    let (payload, reason) = if anthropic {
        (
            value
                .get("content")
                .filter(|v| v.is_array())
                .ok_or_else(|| invalid("Missing Anthropic content"))?,
            value["stop_reason"].as_str(),
        )
    } else {
        (
            value
                .pointer("/choices/0/message")
                .filter(|v| v.is_object())
                .ok_or_else(|| invalid("Missing assistant message"))?,
            value
                .pointer("/choices/0/finish_reason")
                .and_then(Value::as_str),
        )
    };
    let complete = complete_reason(reason);
    let mut content = String::new();
    let mut calls = Vec::new();
    if anthropic {
        for block in payload.as_array().unwrap() {
            if block["type"] == "text" {
                content.push_str(block["text"].as_str().unwrap_or(""));
            }
            if block["type"] == "tool_use" {
                calls.push(AIToolCall {
                    id: block["id"]
                        .as_str()
                        .ok_or_else(|| invalid("Missing tool ID"))?
                        .to_owned(),
                    name: block["name"]
                        .as_str()
                        .ok_or_else(|| invalid("Missing tool name"))?
                        .to_owned(),
                    arguments: block["input"].clone(),
                });
            }
        }
    } else {
        content = payload["content"]
            .as_str()
            .or_else(|| payload["refusal"].as_str())
            .unwrap_or("")
            .to_owned();
        if let Some(tools) = payload["tool_calls"].as_array() {
            for tool in tools {
                calls.push(AIToolCall {
                    id: tool["id"]
                        .as_str()
                        .ok_or_else(|| invalid("Missing tool ID"))?
                        .to_owned(),
                    name: tool["function"]["name"]
                        .as_str()
                        .ok_or_else(|| invalid("Missing tool name"))?
                        .to_owned(),
                    arguments: serde_json::from_str(
                        tool["function"]["arguments"].as_str().unwrap_or(""),
                    )
                    .map_err(|_| invalid("Invalid tool arguments JSON"))?,
                });
            }
        }
    }
    let mut ids = std::collections::BTreeSet::new();
    for call in &calls {
        if call.id.is_empty() || call.name.is_empty() || !ids.insert(&call.id) {
            return Err(invalid("Missing or duplicate native tool identity"));
        }
        if !call.arguments.is_object() {
            return Err(invalid("Native tool arguments must be an object"));
        }
    }
    if !complete {
        content.push_str(&format!(
            "\n\n[incomplete: {}]",
            reason.unwrap_or("missing stop reason")
        ));
    }
    let usage = &value["usage"];
    let count = |key: &str| usage[key].as_u64().and_then(|v| u32::try_from(v).ok());
    let input = count(if anthropic {
        "input_tokens"
    } else {
        "prompt_tokens"
    });
    let output = count(if anthropic {
        "output_tokens"
    } else {
        "completion_tokens"
    });
    Ok(AIResponse {
        native_turn: if complete {
            capture(request, payload.clone())
        } else {
            None
        },
        content,
        model: request.model.clone(),
        tokens_used: input.zip(output).map(|(a, b)| a.saturating_add(b)),
        input_tokens: input,
        output_tokens: output,
        finish_reason: reason.map(str::to_owned),
        tool_calls: (complete && !calls.is_empty()).then_some(calls),
        cache_creation_input_tokens: count("cache_creation_input_tokens"),
        cache_read_input_tokens: count("cache_read_input_tokens"),
    })
}

pub(crate) async fn call(
    client: &reqwest::Client,
    request: &AIRequest,
) -> Result<AIResponse, AIError> {
    let anthropic = modern_anthropic(request);
    let body = if anthropic {
        anthropic_body(request, false)?
    } else {
        chat_body(request, false)?
    };
    let path = if anthropic {
        "messages"
    } else {
        "chat/completions"
    };
    let mut builder = client.post(format!("{}/{path}", request.base_url.trim_end_matches('/')));
    if anthropic {
        builder = builder
            .header(
                "x-api-key",
                request.api_key.as_ref().ok_or(AIError::MissingApiKey)?,
            )
            .header("anthropic-version", "2023-06-01");
    } else if let Some(key) = &request.api_key {
        builder = builder.bearer_auth(key);
    }
    let response = builder.json(&body).send().await?;
    let status = response.status();
    let value: Value = response.json().await?;
    if !status.is_success() {
        return Err(AIError::Api(format!(
            "[{status}] {}",
            crate::ai::sanitize_error_message(
                value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Provider request failed")
            )
        )));
    }
    parse(request, &value)
}

#[derive(Default)]
pub(crate) struct StreamState {
    message: Value,
    blocks: std::collections::BTreeMap<u64, Value>,
    partial_json: std::collections::BTreeMap<u64, String>,
    open_blocks: std::collections::BTreeSet<u64>,
    reason: Option<String>,
    usage: Value,
    pub(crate) complete: bool,
    bytes: usize,
}

// Preserve unknown provider fields as well as reasoning_content. Indexed tool
// deltas are folded into the original assistant-message shape, never UI text.
fn merge_delta(target: &mut Value, delta: &Value) -> Result<(), String> {
    match delta {
        Value::Object(fields) => {
            if target.is_null() {
                *target = json!({});
            }
            let object = target
                .as_object_mut()
                .ok_or("Inconsistent assistant delta")?;
            for (key, value) in fields {
                if key == "index" {
                    continue;
                }
                let slot = object.entry(key.clone()).or_insert(Value::Null);
                if matches!(key.as_str(), "role" | "type") && !slot.is_null() {
                    if slot != value {
                        return Err("Conflicting assistant delta identity".into());
                    }
                } else {
                    merge_delta(slot, value)?;
                }
            }
        }
        Value::Array(items) => {
            if target.is_null() {
                *target = json!([]);
            }
            let array = target.as_array_mut().ok_or("Inconsistent array delta")?;
            for item in items {
                let index = item
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or("Unindexed assistant array delta")? as usize;
                if index > 1024 {
                    return Err("Excessive assistant delta index".into());
                }
                while array.len() <= index {
                    array.push(Value::Null);
                }
                merge_delta(&mut array[index], item)?;
            }
        }
        Value::String(text) => {
            if target.is_null() {
                *target = json!(text);
            } else if let Some(existing) = target.as_str() {
                *target = json!(format!("{existing}{text}"));
            } else {
                return Err("Inconsistent text delta".into());
            }
        }
        Value::Null => {}
        _ => {
            if !target.is_null() && target != delta {
                return Err("Conflicting opaque delta".into());
            }
            *target = delta.clone();
        }
    }
    Ok(())
}

impl StreamState {
    /// Returns display text and summarized thinking; opaque state stays private.
    pub(crate) fn ingest(
        &mut self,
        event: &Value,
        anthropic: bool,
    ) -> Result<(String, String), String> {
        self.bytes += event.to_string().len();
        if self.bytes > 16 * 1024 * 1024 {
            return Err("Native stream exceeds 16 MiB".into());
        }
        if event.get("error").is_some() || event["type"] == "error" {
            return Err("Provider stream failed".into());
        }
        let mut text = String::new();
        let mut thinking = String::new();
        if anthropic {
            let index = event["index"].as_u64().unwrap_or(0);
            match event["type"].as_str().unwrap_or("") {
                "message_start" => {
                    self.usage = event["message"]["usage"].clone();
                }
                "content_block_start" => {
                    if !self.open_blocks.insert(index) || self.blocks.contains_key(&index) {
                        return Err("Duplicate content block".into());
                    }
                    let block = event["content_block"].clone();
                    if block["type"] == "text" {
                        text = block["text"].as_str().unwrap_or("").to_owned();
                    }
                    self.blocks.insert(index, block);
                }
                "content_block_delta" => {
                    if !self.open_blocks.contains(&index) {
                        return Err("Delta outside an open content block".into());
                    }
                    let block = self.blocks.get_mut(&index).ok_or("Missing content block")?;
                    let delta = &event["delta"];
                    let field = match delta["type"].as_str() {
                        Some("text_delta") => "text",
                        Some("thinking_delta") => "thinking",
                        Some("signature_delta") => "signature",
                        Some("input_json_delta") => {
                            self.partial_json.entry(index).or_default().push_str(
                                delta["partial_json"]
                                    .as_str()
                                    .ok_or("Invalid tool JSON delta")?,
                            );
                            return Ok((text, thinking));
                        }
                        _ => {
                            return Err(
                                "Unsupported native content delta; refusing lossy replay".into()
                            )
                        }
                    };
                    let value = delta[field].as_str().ok_or("Invalid content delta")?;
                    let previous = block[field].as_str().unwrap_or("");
                    block[field] = json!(format!("{previous}{value}"));
                    if field == "text" {
                        text = value.to_owned();
                    }
                    if field == "thinking" {
                        thinking = value.to_owned();
                    }
                }
                "content_block_stop" => {
                    if !self.open_blocks.remove(&index) {
                        return Err("Content block stopped without start".into());
                    }
                    if let Some(input) = self.partial_json.remove(&index) {
                        self.blocks.get_mut(&index).ok_or("Missing tool block")?["input"] =
                            serde_json::from_str(&input)
                                .map_err(|_| "Invalid streamed tool JSON")?;
                    }
                }
                "message_delta" => {
                    self.reason = event["delta"]["stop_reason"].as_str().map(str::to_owned);
                    if let Some(fields) = event["usage"].as_object() {
                        if !self.usage.is_object() {
                            self.usage = json!({});
                        }
                        for (key, value) in fields {
                            self.usage[key] = value.clone();
                        }
                    }
                }
                "message_stop" => {
                    if !self.open_blocks.is_empty() || self.reason.is_none() {
                        return Err("Incomplete Anthropic message".into());
                    }
                    self.complete = true;
                }
                _ => {}
            }
        } else {
            if let Some(choice) = event.pointer("/choices/0") {
                if let Some(delta) = choice.get("delta") {
                    text = delta["content"]
                        .as_str()
                        .or_else(|| delta["refusal"].as_str())
                        .unwrap_or("")
                        .to_owned();
                    thinking = delta["reasoning_content"].as_str().unwrap_or("").to_owned();
                    merge_delta(&mut self.message, delta)?;
                }
                if let Some(reason) = choice["finish_reason"].as_str() {
                    self.reason = Some(reason.to_owned());
                }
            }
            if event["usage"].is_object() {
                self.usage = event["usage"].clone();
            }
        }
        Ok((text, thinking))
    }

    pub(crate) fn finish(&self, request: &AIRequest) -> Result<AIResponse, AIError> {
        if !self.complete {
            return Err(invalid("Stream ended before provider completion"));
        }
        let value = if modern_anthropic(request) {
            json!({"content":self.blocks.values().collect::<Vec<_>>(),"stop_reason":self.reason,"usage":self.usage})
        } else {
            if self.reason.is_none() {
                return Err(invalid("Missing stream finish reason"));
            }
            json!({"choices":[{"message":self.message,"finish_reason":self.reason}],"usage":self.usage})
        };
        parse(request, &value)
    }
}

pub(crate) async fn stream(
    client: &reqwest::Client,
    request: &AIRequest,
    sink: &dyn crate::ai_core::EventSink,
    stream_id: &str,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use crate::ai_stream::StreamChunk;
    use futures_util::StreamExt;
    use std::sync::atomic::Ordering;
    let anthropic = modern_anthropic(request);
    let body = if anthropic {
        anthropic_body(request, true)?
    } else {
        chat_body(request, true)?
    };
    let path = if anthropic {
        "messages"
    } else {
        "chat/completions"
    };
    let mut builder = client.post(format!("{}/{path}", request.base_url.trim_end_matches('/')));
    if anthropic {
        builder = builder
            .header(
                "x-api-key",
                request.api_key.as_ref().ok_or(AIError::MissingApiKey)?,
            )
            .header("anthropic-version", "2023-06-01");
    } else if let Some(key) = &request.api_key {
        builder = builder.bearer_auth(key);
    }
    let response = tokio::select! {
        biased;
        _ = crate::ai_stream::wait_for_cancel(cancel) => return Ok(()),
        result = builder.json(&body).send() => result?,
    };
    if !response.status().is_success() {
        return Err(format!("Provider HTTP {}", response.status()).into());
    }
    let mut bytes = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut state = StreamState::default();
    loop {
        let next = tokio::select! {
            biased;
            _ = crate::ai_stream::wait_for_cancel(cancel) => return Ok(()),
            next = bytes.next() => next,
        };
        let Some(next) = next else { break };
        buffer.extend_from_slice(&next?);
        if buffer.len() > 16 * 1024 * 1024 {
            return Err("Native SSE frame exceeds 16 MiB".into());
        }
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let line = String::from_utf8(buffer.drain(..=end).collect())?;
            let Some(data) = line.trim().strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" && !anthropic {
                state.complete = true;
                break;
            }
            let event: Value = serde_json::from_str(data)?;
            let (content, thinking) = state.ingest(&event, anthropic)?;
            if !content.is_empty() || !thinking.is_empty() {
                sink.emit_stream_chunk(
                    stream_id,
                    &StreamChunk {
                        native_turn: None,
                        content,
                        done: false,
                        tool_calls: None,
                        input_tokens: None,
                        output_tokens: None,
                        thinking: (!thinking.is_empty()).then_some(thinking),
                        thinking_done: None,
                        cache_creation_input_tokens: None,
                        cache_read_input_tokens: None,
                    },
                );
            }
        }
        if state.complete {
            break;
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    let result = state.finish(request)?;
    sink.emit_stream_chunk(
        stream_id,
        &StreamChunk {
            native_turn: result.native_turn,
            content: if !complete_reason(result.finish_reason.as_deref()) {
                format!(
                    "\n\n[incomplete: {}]",
                    result
                        .finish_reason
                        .as_deref()
                        .unwrap_or("missing stop reason")
                )
            } else {
                String::new()
            },
            done: true,
            tool_calls: result.tool_calls,
            input_tokens: result.input_tokens,
            output_tokens: result.output_tokens,
            thinking: None,
            thinking_done: Some(true),
            cache_creation_input_tokens: result.cache_creation_input_tokens,
            cache_read_input_tokens: result.cache_read_input_tokens,
        },
    );
    Ok(())
}

#[cfg(test)]
#[path = "ai_native_tests.rs"]
mod tests;
