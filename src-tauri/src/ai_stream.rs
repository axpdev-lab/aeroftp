// AI Streaming Module for AeroFTP
// SSE/chunked streaming for OpenAI, Anthropic, Gemini, Ollama

use serde::Serialize;
use reqwest::Client;
use tauri::{AppHandle, Emitter};
use futures_util::StreamExt;

use crate::ai::{AIRequest, AIProviderType, AIToolCall};

/// Stream chunk emitted to frontend
#[derive(Debug, Clone, Serialize)]
pub struct StreamChunk {
    pub content: String,
    pub done: bool,
    pub tool_calls: Option<Vec<AIToolCall>>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

/// Start streaming AI response, emitting chunks via Tauri events
#[tauri::command]
pub async fn ai_chat_stream(
    app: AppHandle,
    request: AIRequest,
    stream_id: String,
) -> Result<(), String> {
    let event_name = format!("ai-stream-{}", stream_id);
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
    }

    let messages: Vec<serde_json::Value> = request.messages.iter().map(|m| {
        serde_json::json!({ "role": m.role, "content": m.content })
    }).collect();

    let tools = request.tools.as_ref().map(|defs| {
        defs.iter().map(|d| serde_json::json!({
            "type": "function",
            "function": {
                "name": d.name,
                "description": d.description,
                "parameters": d.parameters,
            }
        })).collect()
    });

    let body = OpenAIStreamRequest {
        model: request.model.clone(),
        messages,
        stream: true,
        max_tokens: request.max_tokens,
        temperature: request.temperature,
        tools,
    };

    let response = client.post(&url).headers(headers).json(&body).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {} — {}", status, body).into());
    }
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut accumulated_tool_calls: Vec<PartialToolCall> = Vec::new();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(line_end) = buffer.find('\n') {
            let line = buffer[..line_end].trim().to_string();
            buffer = buffer[line_end + 1..].to_string();

            if line.is_empty() || line == "data: [DONE]" {
                if line == "data: [DONE]" {
                    let tool_calls = if accumulated_tool_calls.is_empty() {
                        None
                    } else {
                        Some(accumulated_tool_calls.iter().map(|tc| AIToolCall {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            arguments: serde_json::from_str(&tc.arguments).unwrap_or_default(),
                        }).collect())
                    };
                    let _ = app.emit(event_name, StreamChunk {
                        content: String::new(),
                        done: true,
                        tool_calls,
                        input_tokens: None,
                        output_tokens: None,
                    });
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
                            });
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

    let body = serde_json::json!({
        "model": request.model,
        "messages": request.messages.iter().map(|m| serde_json::json!({
            "role": m.role,
            "content": m.content,
        })).collect::<Vec<_>>(),
        "max_tokens": request.max_tokens.unwrap_or(4096),
        "temperature": request.temperature,
        "stream": true,
        "tools": tools,
    });

    let response = client.post(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {} — {}", status, body).into());
    }
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut tool_calls: Vec<PartialToolCall> = Vec::new();
    let mut current_tool_index: Option<usize> = None;
    let mut input_tokens: Option<u32> = None;
    let mut output_tokens: Option<u32> = None;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));

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
                            if block["type"].as_str() == Some("tool_use") {
                                tool_calls.push(PartialToolCall {
                                    id: block["id"].as_str().unwrap_or("").to_string(),
                                    name: block["name"].as_str().unwrap_or("").to_string(),
                                    arguments: String::new(),
                                });
                                current_tool_index = Some(tool_calls.len() - 1);
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
                            }
                        }
                        "message_stop" => {
                            let tc = if tool_calls.is_empty() {
                                None
                            } else {
                                Some(tool_calls.iter().map(|t| AIToolCall {
                                    id: t.id.clone(),
                                    name: t.name.clone(),
                                    arguments: serde_json::from_str(&t.arguments).unwrap_or_default(),
                                }).collect())
                            };
                            let _ = app.emit(event_name, StreamChunk {
                                content: String::new(),
                                done: true,
                                tool_calls: tc,
                                input_tokens,
                                output_tokens,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
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

    let body = serde_json::json!({
        "contents": request.messages.iter().map(|m| serde_json::json!({
            "role": if m.role == "user" { "user" } else { "model" },
            "parts": [{ "text": m.content }],
        })).collect::<Vec<_>>(),
        "generationConfig": {
            "maxOutputTokens": request.max_tokens,
            "temperature": request.temperature,
        },
        "tools": tools,
    });

    let response = client.post(&url).json(&body).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {} — {}", status, body).into());
    }
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut final_tool_calls: Vec<AIToolCall> = Vec::new();
    let mut input_tokens: Option<u32> = None;
    let mut output_tokens: Option<u32> = None;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(line_end) = buffer.find('\n') {
            let line = buffer[..line_end].trim().to_string();
            buffer = buffer[line_end + 1..].to_string();

            if line.is_empty() { continue; }

            if let Some(data) = line.strip_prefix("data: ") {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) {
                    // Extract text parts
                    if let Some(parts) = parsed["candidates"][0]["content"]["parts"].as_array() {
                        for part in parts {
                            if let Some(text) = part["text"].as_str() {
                                let _ = app.emit(event_name, StreamChunk {
                                    content: text.to_string(),
                                    done: false,
                                    tool_calls: None,
                                    input_tokens: None,
                                    output_tokens: None,
                                });
                            }
                            if let Some(fc) = part.get("functionCall") {
                                if let (Some(name), Some(args)) = (fc["name"].as_str(), fc.get("args")) {
                                    final_tool_calls.push(AIToolCall {
                                        id: format!("gemini_{}", name),
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

    // Final done event
    let _ = app.emit(event_name, StreamChunk {
        content: String::new(),
        done: true,
        tool_calls: if final_tool_calls.is_empty() { None } else { Some(final_tool_calls) },
        input_tokens,
        output_tokens,
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

    let body = serde_json::json!({
        "model": request.model,
        "messages": request.messages.iter().map(|m| serde_json::json!({
            "role": m.role,
            "content": m.content,
        })).collect::<Vec<_>>(),
        "stream": true,
        "options": {
            "num_predict": request.max_tokens,
            "temperature": request.temperature,
        },
    });

    let response = client.post(&url).json(&body).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {} — {}", status, body).into());
    }
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));

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
                });
            }
        }
    }

    Ok(())
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}
