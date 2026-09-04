// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! OpenAI Responses API adapter.
//!
//! This first slice is deliberately stateless (`store: false`). It supports
//! foreground text/image input, custom function tools, non-streaming output and
//! the corresponding SSE events. Durable response chaining belongs to the
//! conversation-state lane; callers must not pretend Chat Completions history
//! preserves opaque Responses reasoning items.

use std::collections::BTreeMap;

use reqwest::Client;
use serde_json::{json, Value};

use crate::ai::{truncate_safe, AIError, AIRequest, AIResponse, AIToolCall};

fn reasoning_effort(thinking_budget: Option<u32>) -> Option<&'static str> {
    Some(match thinking_budget? {
        0 => "none",
        1..=5_000 => "low",
        5_001..=20_000 => "medium",
        20_001..=50_000 => "high",
        50_001..=99_999 => "xhigh",
        _ => "max",
    })
}

fn append_output_part(text: &mut String, part: &Value) {
    match part.get("type").and_then(Value::as_str) {
        Some("output_text") => {
            if let Some(delta) = part.get("text").and_then(Value::as_str) {
                text.push_str(delta);
            }
        }
        Some("refusal") => {
            if let Some(refusal) = part
                .get("refusal")
                .and_then(Value::as_str)
                .or_else(|| part.get("text").and_then(Value::as_str))
            {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(refusal);
            }
        }
        _ => {}
    }
}

fn incomplete_notice(response: &Value) -> String {
    let reason = response
        .pointer("/incomplete_details/reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    format!("[incomplete: {reason}]")
}

/// Terminal outcome after the Responses SSE reader stops.
pub(crate) fn responses_stream_end(cancelled: bool, completed: bool) -> Result<(), String> {
    if cancelled {
        return Ok(());
    }
    if !completed {
        return Err("Responses stream ended before response.completed".into());
    }
    Ok(())
}

fn input_message(message: &crate::ai::ChatMessage) -> Value {
    if let Some(images) = message.images.as_ref().filter(|images| !images.is_empty()) {
        let mut content = Vec::with_capacity(images.len() + 1);
        if !message.content.is_empty() {
            content.push(json!({ "type": "input_text", "text": message.content }));
        }
        content.extend(images.iter().map(|image| {
            json!({
                "type": "input_image",
                "image_url": format!("data:{};base64,{}", image.media_type, image.data),
                "detail": "auto",
            })
        }));
        json!({ "role": message.role, "content": content })
    } else {
        json!({ "role": message.role, "content": message.content })
    }
}

/// Build the provider-native request without logging credentials or content.
pub(crate) fn build_request_body(request: &AIRequest, stream: bool) -> Result<Value, AIError> {
    let instructions = request
        .messages
        .iter()
        .filter(|message| message.role == "system" || message.role == "developer")
        .map(|message| message.content.as_str())
        .filter(|content| !content.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut input = Vec::new();
    for message in request
        .messages
        .iter()
        .filter(|message| message.role != "system" && message.role != "developer")
    {
        if message.role == "tool" {
            if let Some(call_id) = message.tool_call_id.as_ref() {
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": message.content,
                }));
            } else {
                input.push(input_message(message));
            }
            continue;
        }

        if !message.content.is_empty() || message.images.as_ref().is_some_and(|i| !i.is_empty()) {
            input.push(input_message(message));
        }
        if let Some(tool_calls) = &message.tool_calls_echo {
            input.extend(tool_calls.iter().map(|tool_call| {
                json!({
                    "type": "function_call",
                    "call_id": tool_call.id,
                    "name": tool_call.name,
                    "arguments": tool_call.arguments,
                })
            }));
        }
    }

    if let Some(results) = &request.tool_results {
        input.extend(results.iter().map(|result| {
            json!({
                "type": "function_call_output",
                "call_id": result.tool_call_id,
                "output": result.content,
            })
        }));
    }

    let tools = request.tools.as_ref().map(|definitions| {
        definitions
            .iter()
            .map(|definition| {
                let mut parameters = definition.parameters.clone();
                if let Some(object) = parameters.as_object_mut() {
                    object.insert("additionalProperties".to_string(), json!(false));
                }
                json!({
                    "type": "function",
                    "name": definition.name,
                    "description": definition.description,
                    "parameters": parameters,
                })
            })
            .collect::<Vec<_>>()
    });

    let mut body = json!({
        "model": request.model,
        "input": input,
        "stream": stream,
        // AA26-02 does not yet define lifecycle/redaction rules for persisted
        // response IDs, so the provider must not retain these foreground turns.
        "store": false,
        "max_output_tokens": request.max_tokens,
        "parallel_tool_calls": true,
        "tools": tools,
    });

    if !instructions.is_empty() {
        body["instructions"] = json!(instructions);
    }
    if let Some(effort) = reasoning_effort(request.thinking_budget) {
        body["reasoning"] = json!({ "effort": effort, "context": "current_turn" });
    }

    // GPT reasoning models do not consistently accept sampling controls. The
    // Responses lane prioritizes capability-safe reasoning over Chat-era knobs.
    if let Some(object) = body.as_object_mut() {
        object.retain(|_, value| !value.is_null());
    }
    Ok(body)
}

pub(crate) async fn call(client: &Client, request: &AIRequest) -> Result<AIResponse, AIError> {
    let api_key = request.api_key.as_ref().ok_or(AIError::MissingApiKey)?;
    let url = format!("{}/responses", request.base_url.trim_end_matches('/'));
    let body = build_request_body(request, false)?;
    let response = client
        .post(url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let response_body = response.text().await?;
    if !status.is_success() {
        let detail = serde_json::from_str::<Value>(&response_body)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| truncate_safe(&response_body, 500).to_string());
        return Err(AIError::Api(format!("[{}] {}", status, detail)));
    }
    parse_response_body(&response_body, &request.model)
}

pub(crate) fn parse_response_body(body: &str, fallback_model: &str) -> Result<AIResponse, AIError> {
    let response: Value = serde_json::from_str(body).map_err(|error| {
        AIError::InvalidResponse(format!(
            "Responses JSON parse error: {}: body: {}",
            error,
            truncate_safe(body, 200)
        ))
    })?;

    if let Some(message) = response.pointer("/error/message").and_then(Value::as_str) {
        return Err(AIError::Api(message.to_string()));
    }

    let status = response.get("status").and_then(Value::as_str).unwrap_or("");
    if status == "failed" {
        let detail = response
            .pointer("/error/message")
            .or_else(|| response.pointer("/incomplete_details/reason"))
            .and_then(Value::as_str)
            .unwrap_or("OpenAI Responses request failed");
        return Err(AIError::Api(detail.to_string()));
    }

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    if let Some(output) = response.get("output").and_then(Value::as_array) {
        for item in output {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    if let Some(content) = item.get("content").and_then(Value::as_array) {
                        for part in content {
                            append_output_part(&mut text, part);
                        }
                    }
                }
                Some("refusal") => {
                    append_output_part(&mut text, item);
                }
                Some("function_call") => {
                    if let (Some(id), Some(name)) = (
                        item.get("call_id").and_then(Value::as_str),
                        item.get("name").and_then(Value::as_str),
                    ) {
                        let arguments = item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .and_then(|arguments| serde_json::from_str(arguments).ok())
                            .unwrap_or_else(|| json!({}));
                        tool_calls.push(AIToolCall {
                            id: id.to_string(),
                            name: name.to_string(),
                            arguments,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    if status == "incomplete" {
        let notice = incomplete_notice(&response);
        if text.is_empty() {
            text = notice;
        } else {
            text.push_str("\n\n");
            text.push_str(&notice);
        }
    }

    let input_tokens = response
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let output_tokens = response
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let tokens_used = response
        .pointer("/usage/total_tokens")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .or_else(|| {
            input_tokens
                .zip(output_tokens)
                .map(|(input, output)| input + output)
        });

    Ok(AIResponse {
        content: text,
        model: response
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(fallback_model)
            .to_string(),
        tokens_used,
        input_tokens,
        output_tokens,
        finish_reason: response
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned),
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    })
}

#[derive(Debug, Default)]
struct PartialFunctionCall {
    call_id: String,
    name: String,
    arguments: String,
}

#[derive(Debug, Default)]
pub(crate) struct ResponsesStreamUpdate {
    pub content: Option<String>,
    pub done: bool,
    pub tool_calls: Option<Vec<AIToolCall>>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

#[derive(Debug, Default)]
pub(crate) struct ResponsesStreamAccumulator {
    function_calls: BTreeMap<u64, PartialFunctionCall>,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    completed: bool,
}

impl ResponsesStreamAccumulator {
    fn collected_tool_calls(&self) -> Option<Vec<AIToolCall>> {
        let calls = self
            .function_calls
            .values()
            .filter(|call| !call.call_id.is_empty() && !call.name.is_empty())
            .map(|call| AIToolCall {
                id: call.call_id.clone(),
                name: call.name.clone(),
                arguments: serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({})),
            })
            .collect::<Vec<_>>();
        (!calls.is_empty()).then_some(calls)
    }

    fn read_usage(&mut self, response: &Value) {
        self.input_tokens = response
            .pointer("/usage/input_tokens")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        self.output_tokens = response
            .pointer("/usage/output_tokens")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok());
    }

    pub(crate) fn ingest(
        &mut self,
        event: &Value,
    ) -> Result<Option<ResponsesStreamUpdate>, String> {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match event_type {
            "response.output_text.delta" | "response.refusal.delta" => Ok(event
                .get("delta")
                .and_then(Value::as_str)
                .or_else(|| event.get("refusal").and_then(Value::as_str))
                .filter(|delta| !delta.is_empty())
                .map(|delta| ResponsesStreamUpdate {
                    content: Some(delta.to_string()),
                    ..Default::default()
                })),
            "response.output_item.added" | "response.output_item.done" => {
                let Some(item) = event.get("item") else {
                    return Ok(None);
                };
                if item.get("type").and_then(Value::as_str) != Some("function_call") {
                    return Ok(None);
                }
                let index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let call = self.function_calls.entry(index).or_default();
                if let Some(call_id) = item.get("call_id").and_then(Value::as_str) {
                    call.call_id = call_id.to_string();
                }
                if let Some(name) = item.get("name").and_then(Value::as_str) {
                    call.name = name.to_string();
                }
                if event_type.ends_with(".done") {
                    if let Some(arguments) = item.get("arguments").and_then(Value::as_str) {
                        call.arguments = arguments.to_string();
                    }
                }
                Ok(None)
            }
            "response.function_call_arguments.delta" => {
                let index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    self.function_calls
                        .entry(index)
                        .or_default()
                        .arguments
                        .push_str(delta);
                }
                Ok(None)
            }
            "response.function_call_arguments.done" => {
                let index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if let Some(arguments) = event.get("arguments").and_then(Value::as_str) {
                    self.function_calls.entry(index).or_default().arguments = arguments.to_string();
                }
                Ok(None)
            }
            "response.completed" => {
                if let Some(response) = event.get("response") {
                    self.read_usage(response);
                }
                self.completed = true;
                Ok(Some(self.finish()))
            }
            "response.incomplete" => {
                if let Some(response) = event.get("response") {
                    self.read_usage(response);
                }
                self.completed = true;
                let mut update = self.finish();
                let notice = incomplete_notice(event.get("response").unwrap_or(event));
                update.content = Some(format!("\n\n{notice}"));
                Ok(Some(update))
            }
            "response.failed" | "error" => {
                let message = event
                    .pointer("/response/error/message")
                    .or_else(|| event.pointer("/error/message"))
                    .or_else(|| event.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("OpenAI Responses stream failed");
                Err(message.to_string())
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn finish(&self) -> ResponsesStreamUpdate {
        ResponsesStreamUpdate {
            content: None,
            done: true,
            tool_calls: self.collected_tool_calls(),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        }
    }

    pub(crate) fn completed(&self) -> bool {
        self.completed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{AIProviderType, AIToolDefinition, ChatMessage, ImageAttachment, ToolCallEcho};

    fn request() -> AIRequest {
        AIRequest {
            provider_type: AIProviderType::OpenAI,
            model: "gpt-5.6-sol".to_string(),
            api_key: Some("secret".to_string()),
            base_url: "https://api.openai.com/v1".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: "Be precise.".to_string(),
                    images: None,
                    tool_calls_echo: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: "Inspect this".to_string(),
                    images: Some(vec![ImageAttachment {
                        data: "aGVsbG8=".to_string(),
                        media_type: "image/png".to_string(),
                    }]),
                    tool_calls_echo: None,
                    tool_call_id: None,
                },
            ],
            max_tokens: Some(32_000),
            temperature: Some(0.7),
            tools: Some(vec![AIToolDefinition {
                name: "read_file".to_string(),
                description: "Read a file".to_string(),
                parameters: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
            }]),
            tool_results: None,
            thinking_budget: Some(30_000),
            top_p: Some(0.9),
            top_k: None,
            cached_content: None,
            web_search: None,
            use_responses_api: Some(true),
        }
    }

    #[test]
    fn builds_native_stateless_body_for_text_images_tools_and_reasoning() {
        let body = build_request_body(&request(), true).unwrap();
        assert_eq!(body["model"], "gpt-5.6-sol");
        assert_eq!(body["instructions"], "Be precise.");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["max_output_tokens"], 32_000);
        assert_eq!(
            body["reasoning"],
            json!({"effort":"high","context":"current_turn"})
        );
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
        assert_eq!(body["tools"][0]["name"], "read_file");
        // AeroAgent schemas contain optional fields. Strict mode requires every
        // property to be required (nullable for optional semantics), which is a
        // separate schema migration; do not make a false strictness claim.
        assert!(body["tools"][0].get("strict").is_none());
        assert_eq!(
            body["tools"][0]["parameters"]["additionalProperties"],
            false
        );
    }

    #[test]
    fn serializes_function_echo_and_output_with_responses_call_ids() {
        let mut request = request();
        request.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: String::new(),
            images: None,
            tool_calls_echo: Some(vec![ToolCallEcho {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: "{\"path\":\"README.md\"}".to_string(),
            }]),
            tool_call_id: None,
        });
        request.messages.push(ChatMessage {
            role: "tool".to_string(),
            content: "contents".to_string(),
            images: None,
            tool_calls_echo: None,
            tool_call_id: Some("call_1".to_string()),
        });
        let body = build_request_body(&request, false).unwrap();
        assert!(body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item["type"] == "function_call" && item["call_id"] == "call_1" }));
        assert!(body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item["type"] == "function_call_output" && item["call_id"] == "call_1" }));
    }

    #[test]
    fn parses_text_function_calls_and_usage() {
        let body = json!({
            "model":"gpt-5.6-sol",
            "status":"completed",
            "output":[
                {"type":"message","content":[{"type":"output_text","text":"I will inspect."}]},
                {"type":"function_call","call_id":"call_7","name":"read_file","arguments":"{\"path\":\"a.txt\"}"}
            ],
            "usage":{"input_tokens":50,"output_tokens":20,"total_tokens":70}
        });
        let parsed = parse_response_body(&body.to_string(), "fallback").unwrap();
        assert_eq!(parsed.content, "I will inspect.");
        assert_eq!(parsed.tokens_used, Some(70));
        assert_eq!(parsed.tool_calls.unwrap()[0].id, "call_7");
    }

    #[test]
    fn accumulates_streamed_function_arguments_and_completion_usage() {
        let mut state = ResponsesStreamAccumulator::default();
        state
            .ingest(&json!({
                "type":"response.output_item.added","output_index":1,
                "item":{"type":"function_call","call_id":"call_9","name":"read_file","arguments":""}
            }))
            .unwrap();
        state.ingest(&json!({
            "type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"path\":"
        })).unwrap();
        state.ingest(&json!({
            "type":"response.function_call_arguments.delta","output_index":1,"delta":"\"b.txt\"}"
        })).unwrap();
        let delta = state
            .ingest(&json!({
                "type":"response.output_text.delta","delta":"Checking"
            }))
            .unwrap()
            .unwrap();
        assert_eq!(delta.content.as_deref(), Some("Checking"));
        let done = state.ingest(&json!({
            "type":"response.completed","response":{"usage":{"input_tokens":11,"output_tokens":6}}
        })).unwrap().unwrap();
        assert!(done.done);
        assert_eq!(done.input_tokens, Some(11));
        assert_eq!(
            done.tool_calls.unwrap()[0].arguments,
            json!({"path":"b.txt"})
        );
    }

    #[test]
    fn parses_refusal_content_instead_of_a_blank_success() {
        let body = json!({
            "model":"gpt-5.6-sol",
            "status":"completed",
            "output":[{"type":"message","content":[{"type":"refusal","refusal":"I cannot help with that."}]}]
        });
        let parsed = parse_response_body(&body.to_string(), "fallback").unwrap();
        assert_eq!(parsed.content, "I cannot help with that.");
    }

    #[test]
    fn surfaces_incomplete_status_and_reason() {
        let body = json!({
            "model":"gpt-5.6-sol",
            "status":"incomplete",
            "incomplete_details":{"reason":"max_output_tokens"},
            "output":[{"type":"message","content":[{"type":"output_text","text":"Partial"}]}]
        });
        let parsed = parse_response_body(&body.to_string(), "fallback").unwrap();
        assert!(parsed.content.contains("Partial"));
        assert!(parsed.content.contains("[incomplete: max_output_tokens]"));
        assert_eq!(parsed.finish_reason.as_deref(), Some("incomplete"));
    }

    #[test]
    fn failed_status_is_an_error() {
        let body = json!({
            "status":"failed",
            "error":{"message":"provider refused"}
        });
        let err = parse_response_body(&body.to_string(), "fallback").unwrap_err();
        assert!(format!("{err:?}").contains("provider refused"));
    }

    #[test]
    fn streams_refusal_deltas_and_distinguishes_incomplete_from_completed() {
        let mut state = ResponsesStreamAccumulator::default();
        let delta = state
            .ingest(&json!({"type":"response.refusal.delta","delta":"No."}))
            .unwrap()
            .unwrap();
        assert_eq!(delta.content.as_deref(), Some("No."));
        let done = state
            .ingest(&json!({
                "type":"response.incomplete",
                "response":{"incomplete_details":{"reason":"content_filter"},"usage":{"input_tokens":3,"output_tokens":1}}
            }))
            .unwrap()
            .unwrap();
        assert!(done.done);
        assert!(done
            .content
            .unwrap()
            .contains("[incomplete: content_filter]"));
        assert!(state.completed());
    }

    #[test]
    fn reasoning_off_is_none_and_maximum_is_max() {
        let mut off = request();
        off.thinking_budget = Some(0);
        let body = build_request_body(&off, false).unwrap();
        assert_eq!(body["reasoning"]["effort"], "none");

        let mut maximum = request();
        maximum.thinking_budget = Some(100_000);
        let body = build_request_body(&maximum, false).unwrap();
        assert_eq!(body["reasoning"]["effort"], "max");

        let mut xhigh = request();
        xhigh.thinking_budget = Some(50_001);
        let body = build_request_body(&xhigh, false).unwrap();
        assert_eq!(body["reasoning"]["effort"], "xhigh");
    }

    #[test]
    fn stream_end_errors_on_drop_but_not_on_cancel() {
        assert!(responses_stream_end(false, false).is_err());
        assert!(responses_stream_end(true, false).is_ok());
        assert!(responses_stream_end(false, true).is_ok());
    }
}
