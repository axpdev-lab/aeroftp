// AI Streaming Module for AeroFTP
// SSE/chunked streaming for OpenAI, Anthropic, Gemini, Ollama

use serde::Serialize;
use reqwest::Client;
use tauri::{AppHandle, Emitter};
use futures_util::StreamExt;

use crate::ai::{AIRequest, AIProviderType, AIToolCall, truncate_safe};

/// Maximum SSE buffer size (50 MB) to prevent unbounded memory growth
const MAX_BUFFER_SIZE: usize = 50 * 1024 * 1024;

/// Stream chunk emitted to frontend
#[derive(Debug, Clone, Serialize)]
pub struct StreamChunk {
    pub content: String,
    pub done: bool,
    pub tool_calls: Option<Vec<AIToolCall>>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    /// Extended thinking delta text (Anthropic only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// True when a thinking block has finished
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_done: Option<bool>,
    /// Anthropic prompt caching: tokens written to cache
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    /// Anthropic prompt caching: tokens read from cache
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
}

/// Start streaming AI response, emitting chunks via Tauri events
#[tauri::command]
pub async fn ai_chat_stream(
    app: AppHandle,
    request: AIRequest,
    stream_id: String,
) -> Result<(), String> {
    let event_name = format!("ai-stream-{}", stream_id);

    // Clamp top_p to [0.0, 1.0], top_k to [1, 500], and thinking_budget to [0, 128000]
    let request = AIRequest {
        top_p: request.top_p.map(|v| v.clamp(0.0, 1.0)),
        top_k: request.top_k.map(|v| v.clamp(1, 500)),
        thinking_budget: request.thinking_budget.map(|v| v.clamp(0, 128_000)),
        ..request
    };

    let client = Client::new();

    let result = match request.provider_type {
        AIProviderType::Google => stream_gemini(&client, &request, &app, &event_name).await,
        AIProviderType::Anthropic => stream_anthropic(&client, &request, &app, &event_name).await,
        AIProviderType::Ollama => stream_ollama(&client, &request, &app, &event_name).await,
        // OpenAI, xAI, OpenRouter, Custom all use OpenAI-compatible SSE
        _ => stream_openai(&client, &request, &app, &event_name).await,
    };

    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = app.emit(&event_name, StreamChunk {
                content: format!("Error: {}", e),
                done: true,
                tool_calls: None,
                input_tokens: None,
                output_tokens: None,
                thinking: None,
                thinking_done: None,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            });
            Err(e.to_string())
        }
    }
}

// OpenAI-compatible SSE streaming (OpenAI, xAI, OpenRouter, Custom)
async fn stream_openai(
    client: &Client,
    request: &AIRequest,
    app: &AppHandle,
    event_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/chat/completions", request.base_url);
    let api_key = request.api_key.as_ref().ok_or("Missing API key")?;

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::AUTHORIZATION, format!("Bearer {}", api_key).parse()?);

    if request.provider_type == AIProviderType::OpenRouter {
        headers.insert("HTTP-Referer", "https://aeroftp.app".parse()?);
        headers.insert("X-Title", "AeroFTP".parse()?);
    }

    #[derive(Serialize)]
    struct OpenAIStreamRequest {
        model: String,
        messages: Vec<serde_json::Value>,
        stream: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        temperature: Option<f32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<serde_json::Value>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        top_p: Option<f32>,
    }

    let messages: Vec<serde_json::Value> = request.messages.iter().map(|m| {
        serde_json::json!({ "role": m.role, "content": m.to_openai_content() })
    }).collect();

    // Build tools with strict mode for providers that support structured outputs
    let supports_strict = matches!(
        request.provider_type,
        AIProviderType::OpenAI | AIProviderType::XAI | AIProviderType::OpenRouter
        | AIProviderType::Kimi | AIProviderType::Qwen | AIProviderType::DeepSeek
    );
    let tools = request.tools.as_ref().map(|defs| {
        defs.iter().map(|d| {
            let mut params = d.parameters.clone();
            if supports_strict {
                if let Some(obj) = params.as_object_mut() {
                    obj.insert("additionalProperties".to_string(), serde_json::json!(false));
                }
            }
            let mut func = serde_json::json!({
                "name": d.name,
                "description": d.description,
                "parameters": params,
            });
            if supports_strict {
                func["strict"] = serde_json::json!(true);
            }
            serde_json::json!({
                "type": "function",
                "function": func,
            })
        }).collect()
    });

    let stream_req = OpenAIStreamRequest {
        model: request.model.clone(),
        messages,
        stream: true,
        max_tokens: request.max_tokens,
        temperature: request.temperature,
        tools,
        top_p: request.top_p,
    };

    // Convert to Value so we can inject reasoning_effort for o3
    let mut body = serde_json::to_value(&stream_req)
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { format!("Failed to serialize request: {}", e).into() })?;

    // OpenAI o3/o3-mini thinking support: map budget to reasoning_effort levels
    if let Some(budget) = request.thinking_budget {
        if budget > 0 {
            let effort = if budget <= 5000 { "low" } else if budget <= 20000 { "medium" } else { "high" };
            body["reasoning_effort"] = serde_json::json!(effort);
            // Reasoning models do not support temperature or top_p
            body.as_object_mut().map(|o| {
                o.remove("temperature");
                o.remove("top_p");
            });
        }
    }

    // Qwen thinking mode: enable_thinking + thinking_budget parameters
    if matches!(request.provider_type, AIProviderType::Qwen) {
        if let Some(budget) = request.thinking_budget {
            if budget > 0 {
                body["enable_thinking"] = serde_json::json!(true);
                body["thinking_budget"] = serde_json::json!(budget);
            }
        }
    }

    // DeepSeek thinking mode: enable_thinking parameter
    // Response uses reasoning_content field (already parsed in stream_openai)
    if matches!(request.provider_type, AIProviderType::DeepSeek) {
        if let Some(budget) = request.thinking_budget {
            if budget > 0 {
                body["enable_thinking"] = serde_json::json!(true);
            }
        }
    }

    // Kimi web search: inject $web_search as builtin_function tool
    if matches!(request.provider_type, AIProviderType::Kimi) {
        if request.web_search.unwrap_or(false) {
            let web_tool = serde_json::json!({
                "type": "builtin_function",
                "function": { "name": "$web_search" }
            });
            if let Some(tools_arr) = body["tools"].as_array_mut() {
                tools_arr.push(web_tool);
            } else {
                body["tools"] = serde_json::json!([web_tool]);
            }
        }
    }

    // Kimi context caching: inject cache_id if provided
    if matches!(request.provider_type, AIProviderType::Kimi) {
        if let Some(ref cache_id) = request.cached_content {
            if !cache_id.is_empty() {
                body["context"] = serde_json::json!({ "cache_id": cache_id });
            }
        }
    }

    // Qwen web search: enable_search + search_options
    if matches!(request.provider_type, AIProviderType::Qwen) {
        if request.web_search.unwrap_or(false) {
            body["enable_search"] = serde_json::json!(true);
            body["search_options"] = serde_json::json!({
                "search_strategy": "pro"
            });
        }
    }

    // DeepSeek prefix completion: add prefix:true to last assistant message
    if matches!(request.provider_type, AIProviderType::DeepSeek) {
        if let Some(msgs) = body["messages"].as_array_mut() {
            if let Some(last) = msgs.last_mut() {
                if last.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                    if let Some(obj) = last.as_object_mut() {
                        obj.insert("prefix".to_string(), serde_json::json!(true));
                    }
                }
            }
        }
    }

    let response = client.post(&url).headers(headers).json(&body).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {} — {}", status, truncate_safe(&body, 500)).into());
    }
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut raw_buffer: Vec<u8> = Vec::new();
    let mut accumulated_tool_calls: Vec<PartialToolCall> = Vec::new();
    let mut done_emitted = false;
    let mut had_reasoning = false;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        raw_buffer.extend_from_slice(&bytes);
        match String::from_utf8(std::mem::take(&mut raw_buffer)) {
            Ok(s) => buffer.push_str(&s),
            Err(e) => {
                let valid_up_to = e.utf8_error().valid_up_to();
                let bytes = e.into_bytes();
                buffer.push_str(std::str::from_utf8(&bytes[..valid_up_to]).unwrap_or_default());
                raw_buffer = bytes[valid_up_to..].to_vec();
            }
        }
        if buffer.len() > MAX_BUFFER_SIZE {
            let _ = app.emit(event_name, StreamChunk {
                content: "\n\n[Error: Stream buffer exceeded 50MB limit]".to_string(),
                done: true,
                tool_calls: None,
                input_tokens: None,
                output_tokens: None,
                thinking: None,
                thinking_done: None,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            });
            break;
        }

        while let Some(line_end) = buffer.find('\n') {
            let line = buffer[..line_end].trim().to_string();
            buffer = buffer[line_end + 1..].to_string();

            if line.is_empty() || line == "data: [DONE]" {
                if line == "data: [DONE]" {
                    // If reasoning was emitted, signal thinking_done before final done
                    if had_reasoning {
                        let _ = app.emit(event_name, StreamChunk {
                            content: String::new(),
                            done: false,
                            tool_calls: None,
                            input_tokens: None,
                            output_tokens: None,
                            thinking: None,
                            thinking_done: Some(true),
                            cache_creation_input_tokens: None,
                            cache_read_input_tokens: None,
                        });
                    }
                    let tool_calls = if accumulated_tool_calls.is_empty() {
                        None
                    } else {
                        Some(accumulated_tool_calls.iter().map(|tc| AIToolCall {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            arguments: serde_json::from_str(&tc.arguments).unwrap_or(serde_json::json!({})),
                        }).collect())
                    };
                    let _ = app.emit(event_name, StreamChunk {
                        content: String::new(),
                        done: true,
                        tool_calls,
                        input_tokens: None,
                        output_tokens: None,
                        thinking: None,
                        thinking_done: None,
                        cache_creation_input_tokens: None,
                        cache_read_input_tokens: None,
                    });
                    done_emitted = true;
                }
                continue;
            }

            if let Some(data) = line.strip_prefix("data: ") {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(delta) = parsed["choices"][0]["delta"].as_object() {
                        // Text content
                        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                            let _ = app.emit(event_name, StreamChunk {
                                content: content.to_string(),
                                done: false,
                                tool_calls: None,
                                input_tokens: None,
                                output_tokens: None,
                                thinking: None,
                                thinking_done: None,
                                cache_creation_input_tokens: None,
                                cache_read_input_tokens: None,
                            });
                        }
                        // Detect OpenAI o3 reasoning/thinking content
                        if let Some(reasoning) = delta.get("reasoning_content")
                            .or_else(|| delta.get("reasoning"))
                            .and_then(|r| r.as_str())
                        {
                            if !reasoning.is_empty() {
                                had_reasoning = true;
                                let _ = app.emit(event_name, StreamChunk {
                                    content: String::new(),
                                    done: false,
                                    tool_calls: None,
                                    input_tokens: None,
                                    output_tokens: None,
                                    thinking: Some(reasoning.to_string()),
                                    thinking_done: None,
                                    cache_creation_input_tokens: None,
                                    cache_read_input_tokens: None,
                                });
                            }
                        }
                        // Tool call chunks
                        if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                            for tc in tcs {
                                let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                                while accumulated_tool_calls.len() <= idx {
                                    accumulated_tool_calls.push(PartialToolCall::default());
                                }
                                if let Some(id) = tc["id"].as_str() {
                                    accumulated_tool_calls[idx].id = id.to_string();
                                }
                                if let Some(name) = tc["function"]["name"].as_str() {
                                    accumulated_tool_calls[idx].name = name.to_string();
                                }
                                if let Some(args) = tc["function"]["arguments"].as_str() {
                                    accumulated_tool_calls[idx].arguments.push_str(args);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Process remaining buffer data after stream closes
    if !buffer.trim().is_empty() {
        let line = buffer.trim().to_string();
        if line == "data: [DONE]" {
            done_emitted = true;
        } else if let Some(data) = line.strip_prefix("data: ") {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) {
                if let Some(delta) = parsed["choices"][0]["delta"].as_object() {
                    if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                        let _ = app.emit(event_name, StreamChunk {
                            content: content.to_string(),
                            done: false,
                            tool_calls: None,
                            input_tokens: None,
                            output_tokens: None,
                            thinking: None,
                            thinking_done: None,
                            cache_creation_input_tokens: None,
                            cache_read_input_tokens: None,
                        });
                    }
                }
            }
        }
    }

    // Ensure done is always emitted even if stream closes without [DONE]
    if !done_emitted {
        // If reasoning was emitted, signal thinking_done before final done
        if had_reasoning {
            let _ = app.emit(event_name, StreamChunk {
                content: String::new(),
                done: false,
                tool_calls: None,
                input_tokens: None,
                output_tokens: None,
                thinking: None,
                thinking_done: Some(true),
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            });
        }
        let _ = app.emit(event_name, StreamChunk {
            content: String::new(),
            done: true,
            tool_calls: if accumulated_tool_calls.is_empty() {
                None
            } else {
                Some(accumulated_tool_calls.iter().map(|tc| AIToolCall {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    arguments: serde_json::from_str(&tc.arguments).unwrap_or(serde_json::json!({})),
                }).collect())
            },
            input_tokens: None,
            output_tokens: None,
            thinking: None,
            thinking_done: None,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
    }

    Ok(())
}

// Anthropic streaming (content_block_delta events)
async fn stream_anthropic(
    client: &Client,
    request: &AIRequest,
    app: &AppHandle,
    event_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let api_key = request.api_key.as_ref().ok_or("Missing API key")?;
    let url = format!("{}/messages", request.base_url);

    let tools: Option<Vec<serde_json::Value>> = request.tools.as_ref().map(|defs| {
        defs.iter().map(|d| serde_json::json!({
            "name": d.name,
            "description": d.description,
            "input_schema": d.parameters,
        })).collect()
    });

    // Extract system message for top-level system parameter with cache_control
    let system_text: Option<String> = request.messages.iter()
        .find(|m| m.role == "system")
        .map(|m| m.content.clone());

    // Build base body — filter system messages from the messages array
    let mut body = serde_json::json!({
        "model": request.model,
        "messages": request.messages.iter()
            .filter(|m| m.role != "system")
            .map(|m| serde_json::json!({
                "role": m.role,
                "content": m.to_anthropic_content(),
            })).collect::<Vec<_>>(),
        "max_tokens": request.max_tokens.unwrap_or(4096),
        "temperature": request.temperature,
        "stream": true,
        "tools": tools,
    });

    // Set system prompt as top-level parameter with cache_control for prompt caching
    if let Some(sys) = &system_text {
        body["system"] = serde_json::json!([{
            "type": "text",
            "text": sys,
            "cache_control": { "type": "ephemeral" }
        }]);
    }

    // Extended thinking support: inject thinking config + force temperature 1.0
    if let Some(budget) = request.thinking_budget {
        if budget > 0 {
            body["thinking"] = serde_json::json!({
                "type": "enabled",
                "budget_tokens": budget
            });
            body["temperature"] = serde_json::json!(1.0);
        }
    }

    // Anthropic supports both top_p and top_k (skip when thinking is enabled)
    let thinking_enabled = request.thinking_budget.filter(|b| *b > 0).is_some();
    if !thinking_enabled {
        if let Some(top_p) = request.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(top_k) = request.top_k {
            body["top_k"] = serde_json::json!(top_k);
        }
    }

    // Use 2025-04-15 for all Anthropic calls (required for prompt caching and thinking)
    let anthropic_version = "2025-04-15";

    let response = client.post(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", anthropic_version)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {} — {}", status, truncate_safe(&body, 500)).into());
    }
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut raw_buffer: Vec<u8> = Vec::new();
    let mut tool_calls: Vec<PartialToolCall> = Vec::new();
    let mut current_tool_index: Option<usize> = None;
    let mut input_tokens: Option<u32> = None;
    let mut output_tokens: Option<u32> = None;
    let mut cache_creation_input_tokens: Option<u32> = None;
    let mut cache_read_input_tokens: Option<u32> = None;
    let mut is_thinking = false;
    let mut done_emitted = false;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        raw_buffer.extend_from_slice(&bytes);
        match String::from_utf8(std::mem::take(&mut raw_buffer)) {
            Ok(s) => buffer.push_str(&s),
            Err(e) => {
                let valid_up_to = e.utf8_error().valid_up_to();
                let bytes = e.into_bytes();
                buffer.push_str(std::str::from_utf8(&bytes[..valid_up_to]).unwrap_or_default());
                raw_buffer = bytes[valid_up_to..].to_vec();
            }
        }
        if buffer.len() > MAX_BUFFER_SIZE {
            let _ = app.emit(event_name, StreamChunk {
                content: "\n\n[Error: Stream buffer exceeded 50MB limit]".to_string(),
                done: true,
                tool_calls: None,
                input_tokens: None,
                output_tokens: None,
                thinking: None,
                thinking_done: None,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            });
            break;
        }

        while let Some(line_end) = buffer.find('\n') {
            let line = buffer[..line_end].trim().to_string();
            buffer = buffer[line_end + 1..].to_string();

            if line.is_empty() { continue; }

            if let Some(data) = line.strip_prefix("data: ") {
                if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                    let event_type = event["type"].as_str().unwrap_or("");

                    match event_type {
                        "content_block_start" => {
                            let block = &event["content_block"];
                            match block["type"].as_str() {
                                Some("tool_use") => {
                                    tool_calls.push(PartialToolCall {
                                        id: block["id"].as_str().unwrap_or("").to_string(),
                                        name: block["name"].as_str().unwrap_or("").to_string(),
                                        arguments: String::new(),
                                    });
                                    current_tool_index = Some(tool_calls.len() - 1);
                                }
                                Some("thinking") => {
                                    is_thinking = true;
                                }
                                Some("text") | _ => {
                                    if is_thinking {
                                        let _ = app.emit(event_name, StreamChunk {
                                            content: String::new(),
                                            done: false,
                                            tool_calls: None,
                                            input_tokens: None,
                                            output_tokens: None,
                                            thinking: None,
                                            thinking_done: Some(true),
                                            cache_creation_input_tokens: None,
                                            cache_read_input_tokens: None,
                                        });
                                        is_thinking = false;
                                    }
                                }
                            }
                        }
                        "content_block_delta" => {
                            let delta = &event["delta"];
                            match delta["type"].as_str() {
                                Some("text_delta") => {
                                    if let Some(text) = delta["text"].as_str() {
                                        let _ = app.emit(event_name, StreamChunk {
                                            content: text.to_string(),
                                            done: false,
                                            tool_calls: None,
                                            input_tokens: None,
                                            output_tokens: None,
                                            thinking: None,
                                            thinking_done: None,
                                            cache_creation_input_tokens: None,
                                            cache_read_input_tokens: None,
                                        });
                                    }
                                }
                                Some("thinking_delta") => {
                                    if let Some(thinking_text) = delta["thinking"].as_str() {
                                        let _ = app.emit(event_name, StreamChunk {
                                            content: String::new(),
                                            done: false,
                                            tool_calls: None,
                                            input_tokens: None,
                                            output_tokens: None,
                                            thinking: Some(thinking_text.to_string()),
                                            thinking_done: None,
                                            cache_creation_input_tokens: None,
                                            cache_read_input_tokens: None,
                                        });
                                    }
                                }
                                Some("input_json_delta") => {
                                    if let (Some(idx), Some(json)) = (current_tool_index, delta["partial_json"].as_str()) {
                                        if let Some(tc) = tool_calls.get_mut(idx) {
                                            tc.arguments.push_str(json);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        "content_block_stop" => {
                            if is_thinking {
                                let _ = app.emit(event_name, StreamChunk {
                                    content: String::new(),
                                    done: false,
                                    tool_calls: None,
                                    input_tokens: None,
                                    output_tokens: None,
                                    thinking: None,
                                    thinking_done: Some(true),
                                    cache_creation_input_tokens: None,
                                    cache_read_input_tokens: None,
                                });
                                is_thinking = false;
                            }
                            current_tool_index = None;
                        }
                        "message_delta" => {
                            if let Some(usage) = event["usage"].as_object() {
                                output_tokens = usage.get("output_tokens").and_then(|v| v.as_u64()).map(|v| v as u32);
                            }
                        }
                        "message_start" => {
                            if let Some(usage) = event["message"]["usage"].as_object() {
                                input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64()).map(|v| v as u32);
                                // Extract prompt caching tokens from message_start usage
                                cache_creation_input_tokens = usage.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).map(|v| v as u32);
                                cache_read_input_tokens = usage.get("cache_read_input_tokens").and_then(|v| v.as_u64()).map(|v| v as u32);
                            }
                        }
                        "message_stop" => {
                            let tc = if tool_calls.is_empty() {
                                None
                            } else {
                                Some(tool_calls.iter().map(|t| AIToolCall {
                                    id: t.id.clone(),
                                    name: t.name.clone(),
                                    arguments: serde_json::from_str(&t.arguments).unwrap_or(serde_json::json!({})),
                                }).collect())
                            };
                            let _ = app.emit(event_name, StreamChunk {
                                content: String::new(),
                                done: true,
                                tool_calls: tc,
                                input_tokens,
                                output_tokens,
                                thinking: None,
                                thinking_done: None,
                                cache_creation_input_tokens,
                                cache_read_input_tokens,
                            });
                            done_emitted = true;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Process remaining buffer data after stream closes
    if !buffer.trim().is_empty() {
        let line = buffer.trim().to_string();
        if let Some(data) = line.strip_prefix("data: ") {
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                let event_type = event["type"].as_str().unwrap_or("");
                if event_type == "message_stop" {
                    done_emitted = true;
                }
                // Emit final text delta if present
                if event_type == "content_block_delta" {
                    if let Some(text) = event["delta"]["text"].as_str() {
                        let _ = app.emit(event_name, StreamChunk {
                            content: text.to_string(),
                            done: false,
                            tool_calls: None,
                            input_tokens: None,
                            output_tokens: None,
                            thinking: None,
                            thinking_done: None,
                            cache_creation_input_tokens: None,
                            cache_read_input_tokens: None,
                        });
                    }
                }
            }
        }
    }

    // Ensure done is always emitted even if stream closes without message_stop
    if !done_emitted {
        // If thinking was active, signal thinking_done before final done
        if is_thinking {
            let _ = app.emit(event_name, StreamChunk {
                content: String::new(),
                done: false,
                tool_calls: None,
                input_tokens: None,
                output_tokens: None,
                thinking: None,
                thinking_done: Some(true),
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            });
        }
        let tc = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls.iter().map(|t| AIToolCall {
                id: t.id.clone(),
                name: t.name.clone(),
                arguments: serde_json::from_str(&t.arguments).unwrap_or(serde_json::json!({})),
            }).collect())
        };
        let _ = app.emit(event_name, StreamChunk {
            content: String::new(),
            done: true,
            tool_calls: tc,
            input_tokens,
            output_tokens,
            thinking: None,
            thinking_done: None,
            cache_creation_input_tokens,
            cache_read_input_tokens,
        });
    }

    Ok(())
}

// Gemini streaming (streamGenerateContent)
async fn stream_gemini(
    client: &Client,
    request: &AIRequest,
    app: &AppHandle,
    event_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let api_key = request.api_key.as_ref().ok_or("Missing API key")?;
    let url = format!(
        "{}/models/{}:streamGenerateContent?alt=sse&key={}",
        request.base_url, request.model, api_key
    );

    let tools: Option<Vec<serde_json::Value>> = request.tools.as_ref().map(|defs| {
        vec![serde_json::json!({
            "functionDeclarations": defs.iter().map(|d| serde_json::json!({
                "name": d.name,
                "description": d.description,
                "parameters": d.parameters,
            })).collect::<Vec<_>>()
        })]
    });

    let mut gen_config = serde_json::Map::new();
    if let Some(max) = request.max_tokens {
        gen_config.insert("maxOutputTokens".to_string(), serde_json::json!(max));
    }
    if let Some(temp) = request.temperature {
        gen_config.insert("temperature".to_string(), serde_json::json!(temp));
    }
    if let Some(top_p) = request.top_p {
        gen_config.insert("topP".to_string(), serde_json::json!(top_p));
    }
    if let Some(top_k) = request.top_k {
        gen_config.insert("topK".to_string(), serde_json::json!(top_k));
    }
    if let Some(budget) = request.thinking_budget {
        if budget > 0 {
            gen_config.insert("thinkingConfig".to_string(), serde_json::json!({
                "thinkingBudget": budget
            }));
        }
    }

    // Extract system message for systemInstruction (if not using cached content)
    let has_cache = request.cached_content.is_some();

    let mut body = serde_json::json!({
        "contents": request.messages.iter()
            .filter(|m| m.role != "system")
            .map(|m| serde_json::json!({
                "role": if m.role == "user" { "user" } else { "model" },
                "parts": m.to_gemini_parts(),
            })).collect::<Vec<_>>(),
        "generationConfig": gen_config,
    });
    if let Some(t) = &tools {
        body["tools"] = serde_json::json!(t);
    }
    // Set system_instruction as top-level field (skip if cached content provides it)
    if !has_cache {
        if let Some(sys_msg) = request.messages.iter().find(|m| m.role == "system") {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{ "text": sys_msg.content }]
            });
        }
    }
    // Set cached content name if provided
    if let Some(ref cache_name) = request.cached_content {
        body["cachedContent"] = serde_json::json!(cache_name);
    }

    let response = client.post(&url).json(&body).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {} — {}", status, truncate_safe(&body, 500)).into());
    }
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut raw_buffer: Vec<u8> = Vec::new();
    let mut final_tool_calls: Vec<AIToolCall> = Vec::new();
    let mut tool_call_counter: usize = 0;
    let mut input_tokens: Option<u32> = None;
    let mut output_tokens: Option<u32> = None;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        raw_buffer.extend_from_slice(&bytes);
        match String::from_utf8(std::mem::take(&mut raw_buffer)) {
            Ok(s) => buffer.push_str(&s),
            Err(e) => {
                let valid_up_to = e.utf8_error().valid_up_to();
                let bytes = e.into_bytes();
                buffer.push_str(std::str::from_utf8(&bytes[..valid_up_to]).unwrap_or_default());
                raw_buffer = bytes[valid_up_to..].to_vec();
            }
        }
        if buffer.len() > MAX_BUFFER_SIZE {
            let _ = app.emit(event_name, StreamChunk {
                content: "\n\n[Error: Stream buffer exceeded 50MB limit]".to_string(),
                done: true,
                tool_calls: None,
                input_tokens: None,
                output_tokens: None,
                thinking: None,
                thinking_done: None,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            });
            break;
        }

        while let Some(line_end) = buffer.find('\n') {
            let line = buffer[..line_end].trim().to_string();
            buffer = buffer[line_end + 1..].to_string();

            if line.is_empty() { continue; }

            if let Some(data) = line.strip_prefix("data: ") {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) {
                    // Extract text parts + code execution parts
                    if let Some(parts) = parsed["candidates"][0]["content"]["parts"].as_array() {
                        for part in parts {
                            // Detect Gemini thinking/reasoning parts
                            if let Some(thought) = part.get("thought").and_then(|t| t.as_bool()) {
                                if thought {
                                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                        let _ = app.emit(event_name, StreamChunk {
                                            content: String::new(),
                                            done: false,
                                            tool_calls: None,
                                            input_tokens: None,
                                            output_tokens: None,
                                            thinking: Some(text.to_string()),
                                            thinking_done: None,
                                            cache_creation_input_tokens: None,
                                            cache_read_input_tokens: None,
                                        });
                                        continue; // Skip adding to regular content
                                    }
                                }
                            }
                            if let Some(text) = part["text"].as_str() {
                                let _ = app.emit(event_name, StreamChunk {
                                    content: text.to_string(),
                                    done: false,
                                    tool_calls: None,
                                    input_tokens: None,
                                    output_tokens: None,
                                    thinking: None,
                                    thinking_done: None,
                                    cache_creation_input_tokens: None,
                                    cache_read_input_tokens: None,
                                });
                            }
                            // Handle executableCode parts
                            if let Some(exec_code) = part.get("executableCode") {
                                let lang = exec_code["language"].as_str().unwrap_or("python").to_lowercase();
                                let code = exec_code["code"].as_str().unwrap_or("");
                                let formatted = format!("\n```{}\n{}\n```\n", lang, code);
                                let _ = app.emit(event_name, StreamChunk {
                                    content: formatted,
                                    done: false,
                                    tool_calls: None,
                                    input_tokens: None,
                                    output_tokens: None,
                                    thinking: None,
                                    thinking_done: None,
                                    cache_creation_input_tokens: None,
                                    cache_read_input_tokens: None,
                                });
                            }
                            // Handle codeExecutionResult parts
                            if let Some(exec_result) = part.get("codeExecutionResult") {
                                let outcome = exec_result["outcome"].as_str().unwrap_or("OUTCOME_UNKNOWN");
                                let output = exec_result["output"].as_str().unwrap_or("");
                                let formatted = format!("\n**Execution Output** ({}):\n```\n{}\n```\n", outcome, output);
                                let _ = app.emit(event_name, StreamChunk {
                                    content: formatted,
                                    done: false,
                                    tool_calls: None,
                                    input_tokens: None,
                                    output_tokens: None,
                                    thinking: None,
                                    thinking_done: None,
                                    cache_creation_input_tokens: None,
                                    cache_read_input_tokens: None,
                                });
                            }
                            if let Some(fc) = part.get("functionCall") {
                                if let (Some(name), Some(args)) = (fc["name"].as_str(), fc.get("args")) {
                                    tool_call_counter += 1;
                                    final_tool_calls.push(AIToolCall {
                                        id: format!("gemini_{}_{}", name, tool_call_counter),
                                        name: name.to_string(),
                                        arguments: args.clone(),
                                    });
                                }
                            }
                        }
                    }
                    // Usage metadata
                    if let Some(usage) = parsed.get("usageMetadata") {
                        input_tokens = usage["promptTokenCount"].as_u64().map(|v| v as u32);
                        output_tokens = usage["candidatesTokenCount"].as_u64().map(|v| v as u32);
                    }
                }
            }
        }
    }

    // Process remaining buffer data after stream closes
    if !buffer.trim().is_empty() {
        let line = buffer.trim().to_string();
        if let Some(data) = line.strip_prefix("data: ") {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) {
                if let Some(parts) = parsed["candidates"][0]["content"]["parts"].as_array() {
                    for part in parts {
                        // Detect Gemini thinking/reasoning parts
                        if let Some(thought) = part.get("thought").and_then(|t| t.as_bool()) {
                            if thought {
                                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                    let _ = app.emit(event_name, StreamChunk {
                                        content: String::new(),
                                        done: false,
                                        tool_calls: None,
                                        input_tokens: None,
                                        output_tokens: None,
                                        thinking: Some(text.to_string()),
                                        thinking_done: None,
                                        cache_creation_input_tokens: None,
                                        cache_read_input_tokens: None,
                                    });
                                    continue;
                                }
                            }
                        }
                        if let Some(text) = part["text"].as_str() {
                            let _ = app.emit(event_name, StreamChunk {
                                content: text.to_string(),
                                done: false,
                                tool_calls: None,
                                input_tokens: None,
                                output_tokens: None,
                                thinking: None,
                                thinking_done: None,
                                cache_creation_input_tokens: None,
                                cache_read_input_tokens: None,
                            });
                        }
                        // Handle executableCode parts (remaining buffer)
                        if let Some(exec_code) = part.get("executableCode") {
                            let lang = exec_code["language"].as_str().unwrap_or("python").to_lowercase();
                            let code = exec_code["code"].as_str().unwrap_or("");
                            let formatted = format!("\n```{}\n{}\n```\n", lang, code);
                            let _ = app.emit(event_name, StreamChunk {
                                content: formatted,
                                done: false,
                                tool_calls: None,
                                input_tokens: None,
                                output_tokens: None,
                                thinking: None,
                                thinking_done: None,
                                cache_creation_input_tokens: None,
                                cache_read_input_tokens: None,
                            });
                        }
                        // Handle codeExecutionResult parts (remaining buffer)
                        if let Some(exec_result) = part.get("codeExecutionResult") {
                            let outcome = exec_result["outcome"].as_str().unwrap_or("OUTCOME_UNKNOWN");
                            let output = exec_result["output"].as_str().unwrap_or("");
                            let formatted = format!("\n**Execution Output** ({}):\n```\n{}\n```\n", outcome, output);
                            let _ = app.emit(event_name, StreamChunk {
                                content: formatted,
                                done: false,
                                tool_calls: None,
                                input_tokens: None,
                                output_tokens: None,
                                thinking: None,
                                thinking_done: None,
                                cache_creation_input_tokens: None,
                                cache_read_input_tokens: None,
                            });
                        }
                        if let Some(fc) = part.get("functionCall") {
                            if let (Some(name), Some(args)) = (fc["name"].as_str(), fc.get("args")) {
                                tool_call_counter += 1;
                                final_tool_calls.push(AIToolCall {
                                    id: format!("gemini_{}_{}", name, tool_call_counter),
                                    name: name.to_string(),
                                    arguments: args.clone(),
                                });
                            }
                        }
                    }
                }
                if let Some(usage) = parsed.get("usageMetadata") {
                    input_tokens = usage["promptTokenCount"].as_u64().map(|v| v as u32);
                    output_tokens = usage["candidatesTokenCount"].as_u64().map(|v| v as u32);
                }
            }
        }
    }

    // Final done event (always emitted for Gemini)
    let _ = app.emit(event_name, StreamChunk {
        content: String::new(),
        done: true,
        tool_calls: if final_tool_calls.is_empty() { None } else { Some(final_tool_calls) },
        input_tokens,
        output_tokens,
        thinking: None,
        thinking_done: None,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    });

    Ok(())
}

// Ollama streaming (/api/chat with stream:true, NDJSON)
async fn stream_ollama(
    client: &Client,
    request: &AIRequest,
    app: &AppHandle,
    event_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/api/chat", request.base_url);

    let mut options = serde_json::json!({
        "num_predict": request.max_tokens,
        "temperature": request.temperature,
    });
    if let Some(top_p) = request.top_p {
        options["top_p"] = serde_json::json!(top_p);
    }
    if let Some(top_k) = request.top_k {
        options["top_k"] = serde_json::json!(top_k);
    }

    let body = serde_json::json!({
        "model": request.model,
        "messages": request.messages.iter().map(|m| m.to_ollama_json()).collect::<Vec<_>>(),
        "stream": true,
        "options": options,
    });

    let response = client.post(&url).json(&body).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {} — {}", status, truncate_safe(&body, 500)).into());
    }
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut raw_buffer: Vec<u8> = Vec::new();
    let mut done_emitted = false;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        raw_buffer.extend_from_slice(&bytes);
        match String::from_utf8(std::mem::take(&mut raw_buffer)) {
            Ok(s) => buffer.push_str(&s),
            Err(e) => {
                let valid_up_to = e.utf8_error().valid_up_to();
                let bytes = e.into_bytes();
                buffer.push_str(std::str::from_utf8(&bytes[..valid_up_to]).unwrap_or_default());
                raw_buffer = bytes[valid_up_to..].to_vec();
            }
        }
        if buffer.len() > MAX_BUFFER_SIZE {
            let _ = app.emit(event_name, StreamChunk {
                content: "\n\n[Error: Stream buffer exceeded 50MB limit]".to_string(),
                done: true,
                tool_calls: None,
                input_tokens: None,
                output_tokens: None,
                thinking: None,
                thinking_done: None,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            });
            break;
        }

        while let Some(line_end) = buffer.find('\n') {
            let line = buffer[..line_end].trim().to_string();
            buffer = buffer[line_end + 1..].to_string();

            if line.is_empty() { continue; }

            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&line) {
                let done = parsed["done"].as_bool().unwrap_or(false);
                let content = parsed["message"]["content"].as_str().unwrap_or("").to_string();

                let _ = app.emit(event_name, StreamChunk {
                    content,
                    done,
                    tool_calls: None,
                    input_tokens: if done { parsed["prompt_eval_count"].as_u64().map(|v| v as u32) } else { None },
                    output_tokens: if done { parsed["eval_count"].as_u64().map(|v| v as u32) } else { None },
                    thinking: None,
                    thinking_done: None,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                });
                if done {
                    done_emitted = true;
                }
            }
        }
    }

    // Process remaining buffer data after stream closes
    if !buffer.trim().is_empty() {
        let line = buffer.trim().to_string();
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&line) {
            let done = parsed["done"].as_bool().unwrap_or(false);
            let content = parsed["message"]["content"].as_str().unwrap_or("").to_string();

            let _ = app.emit(event_name, StreamChunk {
                content,
                done,
                tool_calls: None,
                input_tokens: if done { parsed["prompt_eval_count"].as_u64().map(|v| v as u32) } else { None },
                output_tokens: if done { parsed["eval_count"].as_u64().map(|v| v as u32) } else { None },
                thinking: None,
                thinking_done: None,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            });
            if done {
                done_emitted = true;
            }
        }
    }

    // Ensure done is always emitted even if stream closes prematurely
    if !done_emitted {
        let _ = app.emit(event_name, StreamChunk {
            content: String::new(),
            done: true,
            tool_calls: None,
            input_tokens: None,
            output_tokens: None,
            thinking: None,
            thinking_done: None,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
    }

    Ok(())
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}
