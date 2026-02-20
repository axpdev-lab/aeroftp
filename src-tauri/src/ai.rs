// AI Provider Integration Module for AeroFTP
// Supports: Google Gemini, OpenAI, Anthropic, xAI, OpenRouter, Ollama

use serde::{Deserialize, Serialize};
use reqwest::Client;
use std::sync::LazyLock;
use std::time::Duration;

/// Shared HTTP client with connection pooling and timeouts for AI provider requests.
pub static AI_HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()
        .expect("Failed to create AI HTTP client")
});

/// Shared HTTP client for streaming (no read timeout, only connect timeout).
/// Pool idle timeout prevents zombie connections from stalled providers.
pub static AI_STREAM_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .pool_idle_timeout(Duration::from_secs(300))
        .build()
        .unwrap_or_default()
});

/// Safely truncate a string at a UTF-8 character boundary
pub(crate) fn truncate_safe(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Find the last valid char boundary at or before max_bytes
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Strip query parameters from URLs in error messages to prevent API key leakage.
pub(crate) fn sanitize_error_message(msg: &str) -> String {
    let re = regex::Regex::new(r"[?&]key=[^&\s\)]*").unwrap_or_else(|_| regex::Regex::new(r"$^").unwrap());
    re.replace_all(msg, "").to_string()
}

// Provider types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AIProviderType {
    Google,
    OpenAI,
    Anthropic,
    XAI,
    OpenRouter,
    Ollama,
    Custom,
    Kimi,
    Qwen,
    DeepSeek,
    Mistral,
    Groq,
    Perplexity,
    Cohere,
    Together,
}

// Image attachment for vision models
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageAttachment {
    /// Base64-encoded image data (no data URI prefix)
    pub data: String,
    /// MIME type: "image/jpeg", "image/png", "image/gif", "image/webp"
    pub media_type: String,
}

// Chat message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// Optional image attachments for vision-capable models
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageAttachment>>,
}

// AI Request from frontend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIRequest {
    pub provider_type: AIProviderType,
    pub model: String,
    pub api_key: Option<String>,
    pub base_url: String,
    pub messages: Vec<ChatMessage>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// Tool definitions for native function calling (optional)
    pub tools: Option<Vec<AIToolDefinition>>,
    /// Tool results for multi-turn conversations (optional)
    pub tool_results: Option<Vec<AIToolResult>>,
    /// Extended thinking token budget (Anthropic only, enables thinking blocks)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
    /// Top-P (nucleus) sampling parameter
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Top-K sampling parameter
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Gemini cached content name (e.g. "cachedContents/abc123")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_content: Option<String>,
    /// Enable provider web search (Kimi $web_search, Qwen enable_search)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<bool>,
}

// AI Response to frontend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIResponse {
    pub content: String,
    pub model: String,
    pub tokens_used: Option<u32>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub finish_reason: Option<String>,
    /// Native tool calls from providers that support function calling
    pub tool_calls: Option<Vec<AIToolCall>>,
    /// Anthropic prompt caching: tokens written to cache
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    /// Anthropic prompt caching: tokens read from cache
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
}

/// Tool definition sent to AI providers that support native function calling
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Tool call returned by AI provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Tool result to send back to the AI for multi-turn tool use
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIToolResult {
    pub tool_call_id: String,
    pub content: String,
}

/// Helper methods for provider-specific message serialization (vision support)
impl ChatMessage {
    /// OpenAI-compatible content: string or array with image_url blocks
    pub fn to_openai_content(&self) -> serde_json::Value {
        match &self.images {
            Some(images) if !images.is_empty() => {
                let mut parts = vec![serde_json::json!({"type": "text", "text": self.content})];
                for img in images {
                    parts.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{}", img.media_type, img.data)
                        }
                    }));
                }
                serde_json::Value::Array(parts)
            }
            _ => serde_json::Value::String(self.content.clone()),
        }
    }

    /// Anthropic content: string or array with image + text blocks
    pub fn to_anthropic_content(&self) -> serde_json::Value {
        match &self.images {
            Some(images) if !images.is_empty() => {
                let mut blocks: Vec<serde_json::Value> = images.iter().map(|img| {
                    serde_json::json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": img.media_type,
                            "data": img.data,
                        }
                    })
                }).collect();
                blocks.push(serde_json::json!({"type": "text", "text": self.content}));
                serde_json::Value::Array(blocks)
            }
            _ => serde_json::Value::String(self.content.clone()),
        }
    }

    /// Gemini parts: text + inlineData parts
    pub fn to_gemini_parts(&self) -> Vec<serde_json::Value> {
        let mut parts = vec![serde_json::json!({"text": self.content})];
        if let Some(images) = &self.images {
            for img in images {
                parts.push(serde_json::json!({
                    "inlineData": {
                        "mimeType": img.media_type,
                        "data": img.data,
                    }
                }));
            }
        }
        parts
    }

    /// Ollama message: content + optional images array
    pub fn to_ollama_json(&self) -> serde_json::Value {
        let mut msg = serde_json::json!({"role": self.role, "content": self.content});
        if let Some(images) = &self.images {
            if !images.is_empty() {
                let b64_list: Vec<&str> = images.iter().map(|img| img.data.as_str()).collect();
                msg["images"] = serde_json::json!(b64_list);
            }
        }
        msg
    }
}

// Error type
#[derive(Debug, thiserror::Error)]
pub enum AIError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("API error: {0}")]
    Api(String),
    #[error("Missing API key")]
    MissingApiKey,
    #[error("Invalid response: {0}")]
    InvalidResponse(String),
}

impl Serialize for AIError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&sanitize_error_message(&self.to_string()))
    }
}

// Google Gemini
mod gemini {
    use super::*;

    #[derive(Serialize)]
    pub struct GeminiRequest {
        pub contents: Vec<GeminiContent>,
        #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
        pub generation_config: Option<GeminiGenerationConfig>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tools: Option<Vec<GeminiToolConfig>>,
        #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
        pub system_instruction: Option<serde_json::Value>,
        #[serde(rename = "cachedContent", skip_serializing_if = "Option::is_none")]
        pub cached_content: Option<String>,
    }

    #[derive(Serialize)]
    pub struct GeminiToolConfig {
        #[serde(rename = "functionDeclarations")]
        pub function_declarations: Vec<GeminiFunctionDeclaration>,
    }

    #[derive(Serialize)]
    pub struct GeminiFunctionDeclaration {
        pub name: String,
        pub description: String,
        pub parameters: serde_json::Value,
    }

    #[derive(Serialize)]
    pub struct GeminiContent {
        pub role: String,
        pub parts: Vec<GeminiPart>,
    }

    #[derive(Serialize)]
    pub struct GeminiPart {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub text: Option<String>,
        #[serde(rename = "inlineData", skip_serializing_if = "Option::is_none")]
        pub inline_data: Option<GeminiInlineData>,
        #[serde(rename = "functionCall", skip_serializing_if = "Option::is_none")]
        pub function_call: Option<GeminiFunctionCallPart>,
        #[serde(rename = "functionResponse", skip_serializing_if = "Option::is_none")]
        pub function_response: Option<GeminiFunctionResponsePart>,
    }

    #[derive(Serialize)]
    pub struct GeminiInlineData {
        #[serde(rename = "mimeType")]
        pub mime_type: String,
        pub data: String,
    }

    #[derive(Serialize)]
    pub struct GeminiFunctionCallPart {
        pub name: String,
        pub args: serde_json::Value,
    }

    #[derive(Serialize)]
    pub struct GeminiFunctionResponsePart {
        pub name: String,
        pub response: serde_json::Value,
    }

    #[derive(Serialize)]
    pub struct GeminiGenerationConfig {
        #[serde(rename = "maxOutputTokens", skip_serializing_if = "Option::is_none")]
        pub max_output_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub temperature: Option<f32>,
        #[serde(rename = "topP", skip_serializing_if = "Option::is_none")]
        pub top_p: Option<f32>,
        #[serde(rename = "topK", skip_serializing_if = "Option::is_none")]
        pub top_k: Option<u32>,
        #[serde(rename = "thinkingConfig", skip_serializing_if = "Option::is_none")]
        pub thinking_config: Option<serde_json::Value>,
    }

    #[derive(Deserialize)]
    pub struct GeminiResponse {
        pub candidates: Option<Vec<GeminiCandidate>>,
        pub error: Option<GeminiError>,
        #[serde(rename = "usageMetadata")]
        pub usage_metadata: Option<GeminiUsageMetadata>,
    }

    #[derive(Deserialize)]
    pub struct GeminiUsageMetadata {
        #[serde(rename = "promptTokenCount")]
        pub prompt_token_count: Option<u32>,
        #[serde(rename = "candidatesTokenCount")]
        pub candidates_token_count: Option<u32>,
        #[serde(rename = "totalTokenCount")]
        pub total_token_count: Option<u32>,
    }

    #[derive(Deserialize)]
    pub struct GeminiCandidate {
        pub content: GeminiContentResponse,
        #[serde(rename = "finishReason")]
        #[allow(dead_code)]
        pub finish_reason: Option<String>,
    }

    #[derive(Deserialize)]
    pub struct GeminiContentResponse {
        pub parts: Vec<GeminiPartResponse>,
    }

    #[derive(Deserialize)]
    pub struct GeminiPartResponse {
        pub text: Option<String>,
        #[serde(rename = "functionCall")]
        pub function_call: Option<GeminiFunctionCallResponse>,
        #[serde(rename = "executableCode")]
        pub executable_code: Option<GeminiExecutableCode>,
        #[serde(rename = "codeExecutionResult")]
        pub code_execution_result: Option<GeminiCodeExecutionResult>,
    }

    #[derive(Deserialize)]
    pub struct GeminiExecutableCode {
        pub language: Option<String>,
        pub code: Option<String>,
    }

    #[derive(Deserialize)]
    pub struct GeminiCodeExecutionResult {
        pub outcome: Option<String>,
        pub output: Option<String>,
    }

    #[derive(Deserialize)]
    pub struct GeminiFunctionCallResponse {
        pub name: String,
        pub args: serde_json::Value,
    }

    #[derive(Deserialize)]
    pub struct GeminiError {
        pub message: String,
    }

    pub async fn call(client: &Client, request: &AIRequest) -> Result<AIResponse, AIError> {
        let api_key = request.api_key.as_ref().ok_or(AIError::MissingApiKey)?;
        
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            request.base_url, request.model, api_key
        );

        let gemini_tools = request.tools.as_ref().map(|tools| {
            vec![GeminiToolConfig {
                function_declarations: tools.iter().map(|t| GeminiFunctionDeclaration {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                }).collect(),
            }]
        });

        // Extract system message for system_instruction (if not using cached_content)
        let has_cache = request.cached_content.is_some();
        let system_instruction = if has_cache {
            None
        } else {
            request.messages.iter()
                .find(|m| m.role == "system")
                .map(|m| serde_json::json!({
                    "parts": [{ "text": m.content }]
                }))
        };

        let gemini_request = GeminiRequest {
            contents: request.messages.iter()
                .filter(|m| m.role != "system")
                .map(|m| {
                    let mut parts = vec![GeminiPart {
                        text: Some(m.content.clone()),
                        inline_data: None,
                        function_call: None,
                        function_response: None,
                    }];
                    if let Some(images) = &m.images {
                        for img in images {
                            parts.push(GeminiPart {
                                text: None,
                                inline_data: Some(GeminiInlineData {
                                    mime_type: img.media_type.clone(),
                                    data: img.data.clone(),
                                }),
                                function_call: None,
                                function_response: None,
                            });
                        }
                    }
                    GeminiContent {
                        role: if m.role == "user" { "user".to_string() } else { "model".to_string() },
                        parts,
                    }
                }).collect(),
            generation_config: Some(GeminiGenerationConfig {
                max_output_tokens: request.max_tokens,
                temperature: request.temperature,
                top_p: request.top_p,
                top_k: request.top_k,
                thinking_config: request.thinking_budget
                    .filter(|b| *b > 0)
                    .map(|budget| serde_json::json!({ "thinkingBudget": budget })),
            }),
            tools: gemini_tools,
            system_instruction,
            cached_content: request.cached_content.clone(),
        };

        let response = client
            .post(&url)
            .json(&gemini_request)
            .send()
            .await?;

        let gemini_response: GeminiResponse = response.json().await?;

        if let Some(error) = gemini_response.error {
            return Err(AIError::Api(error.message));
        }

        let candidate = gemini_response
            .candidates
            .and_then(|c| c.into_iter().next())
            .ok_or_else(|| AIError::InvalidResponse("No candidates in response".to_string()))?;

        // Extract text parts + executable code + code execution results
        let mut content_parts: Vec<String> = Vec::new();
        for part in &candidate.content.parts {
            if let Some(text) = &part.text {
                content_parts.push(text.clone());
            }
            if let Some(exec_code) = &part.executable_code {
                let lang = exec_code.language.as_deref().unwrap_or("python").to_lowercase();
                let code = exec_code.code.as_deref().unwrap_or("");
                content_parts.push(format!("\n```{}\n{}\n```\n", lang, code));
            }
            if let Some(exec_result) = &part.code_execution_result {
                let outcome = exec_result.outcome.as_deref().unwrap_or("OUTCOME_UNKNOWN");
                let output = exec_result.output.as_deref().unwrap_or("");
                content_parts.push(format!("\n**Execution Output** ({}):\n```\n{}\n```\n", outcome, output));
            }
        }
        let content = content_parts.join("");

        // Extract function call parts
        let tool_calls: Option<Vec<AIToolCall>> = {
            let calls: Vec<AIToolCall> = candidate.content.parts.iter()
                .filter_map(|p| p.function_call.as_ref())
                .map(|fc| AIToolCall {
                    id: format!("gemini_{}", fc.name),
                    name: fc.name.clone(),
                    arguments: fc.args.clone(),
                })
                .collect();
            if calls.is_empty() { None } else { Some(calls) }
        };

        let (input_tokens, output_tokens, total_tokens) = match &gemini_response.usage_metadata {
            Some(u) => (u.prompt_token_count, u.candidates_token_count, u.total_token_count),
            None => (None, None, None),
        };

        Ok(AIResponse {
            content: if content.is_empty() { String::new() } else { content },
            model: request.model.clone(),
            tokens_used: total_tokens,
            input_tokens,
            output_tokens,
            finish_reason: None,
            tool_calls,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        })
    }
}

// OpenAI Compatible (OpenAI, xAI, OpenRouter, Ollama)
mod openai_compat {
    use super::*;

    #[derive(Serialize)]
    pub struct OpenAIRequest {
        pub model: String,
        pub messages: Vec<OpenAIMessage>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub max_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub temperature: Option<f32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tools: Option<Vec<OpenAITool>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub top_p: Option<f32>,
    }

    #[derive(Serialize)]
    pub struct OpenAIMessage {
        pub role: String,
        /// String for text-only, Array for multimodal (vision)
        pub content: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tool_call_id: Option<String>,
    }

    #[derive(Serialize)]
    pub struct OpenAITool {
        #[serde(rename = "type")]
        pub tool_type: String,
        pub function: OpenAIFunction,
    }

    #[derive(Serialize)]
    pub struct OpenAIFunction {
        pub name: String,
        pub description: String,
        pub parameters: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub strict: Option<bool>,
    }

    #[derive(Deserialize)]
    pub struct OpenAIResponse {
        pub choices: Option<Vec<OpenAIChoice>>,
        pub error: Option<OpenAIError>,
        pub usage: Option<OpenAIUsage>,
    }

    #[derive(Deserialize)]
    pub struct OpenAIChoice {
        pub message: OpenAIMessageResponse,
        pub finish_reason: Option<String>,
    }

    #[derive(Deserialize)]
    pub struct OpenAIMessageResponse {
        pub content: Option<String>,
        pub tool_calls: Option<Vec<OpenAIToolCallResponse>>,
    }

    #[derive(Deserialize)]
    pub struct OpenAIToolCallResponse {
        pub id: String,
        pub function: OpenAIFunctionCallResponse,
    }

    #[derive(Deserialize)]
    pub struct OpenAIFunctionCallResponse {
        pub name: String,
        pub arguments: String,
    }

    #[derive(Deserialize)]
    pub struct OpenAIError {
        pub message: String,
    }

    #[derive(Deserialize)]
    pub struct OpenAIUsage {
        pub total_tokens: Option<u32>,
        pub prompt_tokens: Option<u32>,
        pub completion_tokens: Option<u32>,
    }

    pub async fn call(client: &Client, request: &AIRequest, endpoint: &str) -> Result<AIResponse, AIError> {
        let url = format!("{}{}", request.base_url, endpoint);

        let mut headers = reqwest::header::HeaderMap::new();

        if let Some(api_key) = &request.api_key {
            let val = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", api_key))
                .map_err(|e| AIError::InvalidResponse(format!("Invalid API key for header: {}", e)))?;
            headers.insert(reqwest::header::AUTHORIZATION, val);
        }

        // OpenRouter requires additional headers
        if request.provider_type == AIProviderType::OpenRouter {
            headers.insert(
                "HTTP-Referer",
                reqwest::header::HeaderValue::from_static("https://aeroftp.app"),
            );
            headers.insert(
                "X-Title",
                reqwest::header::HeaderValue::from_static("AeroFTP"),
            );
        }

        // Build messages with vision support
        let mut messages: Vec<OpenAIMessage> = request.messages.iter().map(|m| OpenAIMessage {
            role: m.role.clone(),
            content: m.to_openai_content(),
            tool_call_id: None,
        }).collect();

        // Append tool results as "tool" role messages
        if let Some(ref results) = request.tool_results {
            for r in results {
                messages.push(OpenAIMessage {
                    role: "tool".to_string(),
                    content: serde_json::Value::String(r.content.clone()),
                    tool_call_id: Some(r.tool_call_id.clone()),
                });
            }
        }

        // Convert tool definitions
        // For OpenAI, xAI, OpenRouter: enable structured outputs (strict: true + additionalProperties: false)
        let supports_strict = matches!(
            request.provider_type,
            AIProviderType::OpenAI | AIProviderType::XAI | AIProviderType::OpenRouter
        );
        let tools = request.tools.as_ref().map(|defs| {
            defs.iter().map(|d| {
                let mut params = d.parameters.clone();
                if supports_strict {
                    if let Some(obj) = params.as_object_mut() {
                        obj.insert("additionalProperties".to_string(), serde_json::json!(false));
                    }
                }
                OpenAITool {
                    tool_type: "function".to_string(),
                    function: OpenAIFunction {
                        name: d.name.clone(),
                        description: d.description.clone(),
                        parameters: params,
                        strict: if supports_strict { Some(true) } else { None },
                    },
                }
            }).collect()
        });

        let openai_request = OpenAIRequest {
            model: request.model.clone(),
            messages,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            tools,
            top_p: request.top_p,
        };

        // Convert to Value so we can inject reasoning_effort for o3
        let mut body = serde_json::to_value(&openai_request)
            .map_err(|e| AIError::InvalidResponse(format!("Failed to serialize request: {}", e)))?;

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

        let response = client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            // Try to extract error message from JSON body
            if let Ok(err_resp) = serde_json::from_str::<OpenAIResponse>(&body) {
                if let Some(error) = err_resp.error {
                    return Err(AIError::Api(format!("[{}] {}", status, error.message)));
                }
            }
            return Err(AIError::Api(format!("HTTP {} — {}", status, truncate_safe(&body, 500))));
        }

        let openai_response: OpenAIResponse = serde_json::from_str(&body)
            .map_err(|e| AIError::InvalidResponse(format!("JSON parse error: {} — body: {}", e, truncate_safe(&body, 200))))?;

        if let Some(error) = openai_response.error {
            return Err(AIError::Api(error.message));
        }

        let choice = openai_response
            .choices
            .and_then(|c| c.into_iter().next())
            .ok_or_else(|| AIError::InvalidResponse("No choices in response".to_string()))?;

        // Parse native tool calls if present
        let tool_calls = choice.message.tool_calls.map(|tcs| {
            tcs.into_iter().map(|tc| AIToolCall {
                id: tc.id,
                name: tc.function.name,
                arguments: serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::json!({})),
            }).collect()
        });

        Ok(AIResponse {
            content: choice.message.content.unwrap_or_default(),
            model: request.model.clone(),
            tokens_used: openai_response.usage.as_ref().and_then(|u| u.total_tokens),
            input_tokens: openai_response.usage.as_ref().and_then(|u| u.prompt_tokens),
            output_tokens: openai_response.usage.as_ref().and_then(|u| u.completion_tokens),
            finish_reason: choice.finish_reason,
            tool_calls,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        })
    }
}

// Anthropic Claude
mod anthropic {
    use super::*;

    #[derive(Serialize)]
    pub struct AnthropicRequest {
        pub model: String,
        pub messages: Vec<AnthropicMessage>,
        pub max_tokens: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub temperature: Option<f32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tools: Option<Vec<AnthropicToolDef>>,
    }

    #[derive(Serialize)]
    pub struct AnthropicMessage {
        pub role: String,
        /// String for text-only, Array for multimodal (vision)
        pub content: serde_json::Value,
    }

    #[derive(Serialize)]
    pub struct AnthropicToolDef {
        pub name: String,
        pub description: String,
        pub input_schema: serde_json::Value,
    }

    #[derive(Deserialize)]
    pub struct AnthropicResponse {
        pub content: Option<Vec<AnthropicContent>>,
        pub error: Option<AnthropicError>,
        pub stop_reason: Option<String>,
        pub usage: Option<AnthropicUsage>,
    }

    #[derive(Deserialize)]
    pub struct AnthropicContent {
        pub text: Option<String>,
        #[serde(rename = "type")]
        pub content_type: Option<String>,
        /// For tool_use blocks
        pub id: Option<String>,
        pub name: Option<String>,
        pub input: Option<serde_json::Value>,
    }

    #[derive(Deserialize)]
    pub struct AnthropicError {
        pub message: String,
    }

    #[derive(Deserialize)]
    pub struct AnthropicUsage {
        pub input_tokens: Option<u32>,
        pub output_tokens: Option<u32>,
        pub cache_creation_input_tokens: Option<u32>,
        pub cache_read_input_tokens: Option<u32>,
    }

    pub async fn call(client: &Client, request: &AIRequest) -> Result<AIResponse, AIError> {
        let api_key = request.api_key.as_ref().ok_or(AIError::MissingApiKey)?;

        let url = format!("{}/messages", request.base_url);

        // Convert tool definitions for Anthropic format
        let tools = request.tools.as_ref().map(|defs| {
            defs.iter().map(|d| AnthropicToolDef {
                name: d.name.clone(),
                description: d.description.clone(),
                input_schema: d.parameters.clone(),
            }).collect()
        });

        // Extract system message and separate from conversation messages
        // Anthropic requires system as a top-level parameter, not in messages array
        let system_text: Option<String> = request.messages.iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.clone());

        let anthropic_request = AnthropicRequest {
            model: request.model.clone(),
            messages: request.messages.iter()
                .filter(|m| m.role != "system")
                .map(|m| AnthropicMessage {
                    role: m.role.clone(),
                    content: m.to_anthropic_content(),
                }).collect(),
            max_tokens: request.max_tokens.unwrap_or(4096),
            temperature: request.temperature,
            tools,
        };

        // Convert to Value so we can inject thinking config and system prompt with cache_control
        let mut body = serde_json::to_value(&anthropic_request)
            .map_err(|e| AIError::InvalidResponse(format!("Failed to serialize request: {}", e)))?;

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

        let response = client
            .post(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", anthropic_version)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let anthropic_response: AnthropicResponse = response.json().await?;

        if let Some(error) = anthropic_response.error {
            return Err(AIError::Api(error.message));
        }

        let blocks = anthropic_response.content.as_ref();

        // Extract ALL text content blocks (not just the first)
        let content = blocks
            .map(|b| {
                b.iter()
                    .filter(|c| c.content_type.as_deref() == Some("text") || c.content_type.is_none())
                    .filter_map(|c| c.text.as_ref())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();

        // Extract tool_use blocks
        let tool_calls: Option<Vec<AIToolCall>> = blocks.and_then(|b| {
            let calls: Vec<AIToolCall> = b.iter()
                .filter(|c| c.content_type.as_deref() == Some("tool_use"))
                .filter_map(|c| {
                    Some(AIToolCall {
                        id: c.id.clone()?,
                        name: c.name.clone()?,
                        arguments: c.input.clone().unwrap_or(serde_json::json!({})),
                    })
                })
                .collect();
            if calls.is_empty() { None } else { Some(calls) }
        });

        let input_tokens = anthropic_response.usage.as_ref().and_then(|u| u.input_tokens);
        let output_tokens = anthropic_response.usage.as_ref().and_then(|u| u.output_tokens);
        let cache_creation = anthropic_response.usage.as_ref().and_then(|u| u.cache_creation_input_tokens);
        let cache_read = anthropic_response.usage.as_ref().and_then(|u| u.cache_read_input_tokens);
        let total = match (input_tokens, output_tokens) {
            (Some(i), Some(o)) => Some(i + o),
            _ => None,
        };

        Ok(AIResponse {
            content,
            model: request.model.clone(),
            tokens_used: total,
            input_tokens,
            output_tokens,
            finish_reason: anthropic_response.stop_reason,
            tool_calls,
            cache_creation_input_tokens: cache_creation,
            cache_read_input_tokens: cache_read,
        })
    }
}

// Main AI call function
pub async fn call_ai(request: AIRequest) -> Result<AIResponse, AIError> {
    // Clamp top_p to [0.0, 1.0], top_k to [1, 500], and thinking_budget to [0, 128000]
    let request = AIRequest {
        top_p: request.top_p.map(|v| v.clamp(0.0, 1.0)),
        top_k: request.top_k.map(|v| v.clamp(1, 500)),
        thinking_budget: request.thinking_budget.map(|v| v.clamp(0, 128_000)),
        ..request
    };

    let client = &*AI_HTTP_CLIENT;

    match request.provider_type {
        AIProviderType::Google => gemini::call(client, &request).await,
        AIProviderType::Anthropic => anthropic::call(client, &request).await,
        // Ollama 0.5+ supports OpenAI-compat format at /v1/chat/completions
        AIProviderType::Ollama => openai_compat::call(client, &request, "/v1/chat/completions").await,
        // OpenAI-compatible providers: OpenAI, xAI, OpenRouter, Kimi, Qwen, DeepSeek, Custom
        _ => openai_compat::call(client, &request, "/chat/completions").await,
    }
}

// Test provider connection
pub async fn test_provider(provider_type: AIProviderType, base_url: String, api_key: Option<String>) -> Result<bool, AIError> {
    let client = &*AI_HTTP_CLIENT;

    match provider_type {
        AIProviderType::Ollama => {
            // Just check if Ollama is running
            let url = format!("{}/api/tags", base_url);
            let response = client.get(&url).send().await?;
            Ok(response.status().is_success())
        }
        AIProviderType::Google => {
            // List models endpoint
            let api_key = api_key.ok_or(AIError::MissingApiKey)?;
            let url = format!("{}/models?key={}", base_url, api_key);
            let response = client.get(&url).send().await?;
            Ok(response.status().is_success())
        }
        _ => {
            // For OpenAI-compatible, try to list models
            let api_key = api_key.ok_or(AIError::MissingApiKey)?;
            let url = format!("{}/models", base_url);
            let response = client
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
                .await?;
            Ok(response.status().is_success())
        }
    }
}

/// List available models from a provider API
pub async fn list_models(provider_type: AIProviderType, base_url: String, api_key: Option<String>) -> Result<Vec<String>, AIError> {
    let client = &*AI_HTTP_CLIENT;

    match provider_type {
        AIProviderType::Ollama => {
            let url = format!("{}/api/tags", base_url);
            let response = client.get(&url).send().await?;
            if !response.status().is_success() {
                return Err(AIError::Api("Ollama not reachable".to_string()));
            }
            let body: serde_json::Value = response.json().await?;
            let models = body["models"].as_array()
                .map(|arr| arr.iter().filter_map(|m| m["name"].as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            Ok(models)
        }
        AIProviderType::Google => {
            let api_key = api_key.ok_or(AIError::MissingApiKey)?;
            let url = format!("{}/models?key={}", base_url, api_key);
            let response = client.get(&url).send().await?;
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(AIError::Api(format!("HTTP {} — {}", status, truncate_safe(&body, 200))));
            }
            let body: serde_json::Value = response.json().await?;
            let models = body["models"].as_array()
                .map(|arr| arr.iter().filter_map(|m| {
                    m["name"].as_str().map(|s| s.strip_prefix("models/").unwrap_or(s).to_string())
                }).collect())
                .unwrap_or_default();
            Ok(models)
        }
        AIProviderType::Anthropic => {
            let api_key = api_key.ok_or(AIError::MissingApiKey)?;
            let url = format!("{}/models", base_url);
            let response = client
                .get(&url)
                .header("x-api-key", &api_key)
                .header("anthropic-version", "2025-04-15")
                .send()
                .await?;
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(AIError::Api(format!("HTTP {} — {}", status, truncate_safe(&body, 200))));
            }
            let body: serde_json::Value = response.json().await?;
            let models = body["data"].as_array()
                .map(|arr| arr.iter().filter_map(|m| m["id"].as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            Ok(models)
        }
        _ => {
            // OpenAI-compatible (OpenAI, xAI, OpenRouter, Custom)
            let api_key = api_key.ok_or(AIError::MissingApiKey)?;
            let url = format!("{}/models", base_url);
            let response = client
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
                .await?;
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(AIError::Api(format!("HTTP {} — {}", status, truncate_safe(&body, 200))));
            }
            let body: serde_json::Value = response.json().await?;
            let models = body["data"].as_array()
                .map(|arr| arr.iter().filter_map(|m| m["id"].as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            Ok(models)
        }
    }
}

/// Pull (download) an Ollama model with streaming progress events
#[tauri::command]
pub async fn ollama_pull_model(
    app_handle: tauri::AppHandle,
    base_url: String,
    model_name: String,
    stream_id: String,
) -> Result<(), String> {
    use tauri::Emitter;
    use futures_util::StreamExt;

    #[derive(Clone, Serialize)]
    struct PullProgress {
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        completed: Option<u64>,
        done: bool,
    }

    let event_name = format!("ollama-pull-{}", stream_id);
    let client = &*AI_STREAM_CLIENT;
    let url = format!("{}/api/pull", base_url);

    let response = client
        .post(&url)
        .json(&serde_json::json!({
            "name": model_name,
            "stream": true
        }))
        .send()
        .await
        .map_err(|e| format!("Failed to connect to Ollama: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {} — {}", status, truncate_safe(&body, 500)));
    }

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut raw_buffer: Vec<u8> = Vec::new();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| format!("Stream error: {}", e))?;
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

        while let Some(line_end) = buffer.find('\n') {
            let line = buffer[..line_end].trim().to_string();
            buffer = buffer[line_end + 1..].to_string();

            if line.is_empty() { continue; }

            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&line) {
                let status = parsed["status"].as_str().unwrap_or("").to_string();
                let total = parsed["total"].as_u64();
                let completed = parsed["completed"].as_u64();
                let is_done = status == "success";

                let _ = app_handle.emit(&event_name, PullProgress {
                    status,
                    total,
                    completed,
                    done: is_done,
                });
            }
        }
    }

    // Process remaining buffer after stream closes
    if !buffer.trim().is_empty() {
        let line = buffer.trim().to_string();
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&line) {
            let status = parsed["status"].as_str().unwrap_or("").to_string();
            let total = parsed["total"].as_u64();
            let completed = parsed["completed"].as_u64();
            let is_done = status == "success";

            let _ = app_handle.emit(&event_name, PullProgress {
                status,
                total,
                completed,
                done: is_done,
            });
        }
    }

    Ok(())
}

/// Gemini cached content info returned to frontend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiCacheInfo {
    pub name: String,
    pub model: String,
    pub expire_time: String,
    pub token_count: u32,
}

/// Create a Gemini cached content for context caching
#[tauri::command]
pub async fn gemini_create_cache(
    api_key: String,
    base_url: String,
    model: String,
    system_prompt: String,
    context_content: String,
    ttl_seconds: u32,
) -> Result<GeminiCacheInfo, String> {
    let client = &*AI_HTTP_CLIENT;
    let url = format!("{}/cachedContents?key={}", base_url, api_key);

    let body = serde_json::json!({
        "model": format!("models/{}", model),
        "systemInstruction": {
            "parts": [{ "text": system_prompt }]
        },
        "contents": [{
            "role": "user",
            "parts": [{ "text": context_content }]
        }],
        "ttl": format!("{}s", ttl_seconds)
    });

    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Failed to create cache: {}", e))?;

    let status = response.status();
    let resp_body: serde_json::Value = response.json().await
        .map_err(|e| format!("Failed to parse cache response: {}", e))?;

    if !status.is_success() {
        let msg = resp_body["error"]["message"].as_str().unwrap_or("Unknown error");
        return Err(format!("HTTP {} — {}", status, msg));
    }

    let name = resp_body["name"].as_str().unwrap_or("").to_string();
    let expire_time = resp_body["expireTime"].as_str().unwrap_or("").to_string();
    let token_count = resp_body["usageMetadata"]["totalTokenCount"]
        .as_u64()
        .unwrap_or(0) as u32;

    Ok(GeminiCacheInfo {
        name,
        model: model.clone(),
        expire_time,
        token_count,
    })
}

/// Ollama running model info returned to frontend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaRunningModel {
    pub name: String,
    pub size: u64,
    pub vram_size: u64,
    pub expires_at: String,
}

/// List currently running Ollama models with GPU/VRAM info
#[tauri::command]
pub async fn ollama_list_running(base_url: String) -> Result<Vec<OllamaRunningModel>, String> {
    let client = &*AI_HTTP_CLIENT;
    let url = format!("{}/api/ps", base_url);

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Failed to connect to Ollama: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {} — {}", status, truncate_safe(&body, 500)));
    }

    let body: serde_json::Value = response.json().await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    let models = body["models"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|m| OllamaRunningModel {
                    name: m["name"].as_str().unwrap_or("").to_string(),
                    size: m["size"].as_u64().unwrap_or(0),
                    vram_size: m["size_vram"].as_u64().unwrap_or(0),
                    expires_at: m["expires_at"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(models)
}

/// Kimi context caching: create a reusable context cache for long conversations
#[tauri::command]
pub async fn kimi_create_cache(
    api_key: String,
    base_url: String,
    model: String,
    messages: Vec<ChatMessage>,
    ttl: Option<u64>,
) -> Result<String, String> {
    let client = &*AI_HTTP_CLIENT;
    let url = format!("{}/caching", base_url);

    let openai_messages: Vec<serde_json::Value> = messages.iter().map(|m| {
        serde_json::json!({ "role": m.role, "content": m.to_openai_content() })
    }).collect();

    let mut body = serde_json::json!({
        "model": model,
        "messages": openai_messages,
    });
    if let Some(ttl_val) = ttl {
        body["ttl"] = serde_json::json!(ttl_val);
    }

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Kimi cache request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("Kimi cache creation failed [{}]: {}", status, text));
    }

    let result: serde_json::Value = response.json().await
        .map_err(|e| format!("Failed to parse cache response: {}", e))?;

    result["id"].as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "No cache ID in response".to_string())
}

/// Kimi file analysis: upload a file for context in conversations
#[tauri::command]
pub async fn kimi_upload_file(
    api_key: String,
    base_url: String,
    file_path: String,
    purpose: Option<String>,
) -> Result<String, String> {
    let client = &*AI_HTTP_CLIENT;
    let url = format!("{}/files", base_url);

    // Validate path: reject null bytes, traversal, and sensitive system paths
    if file_path.contains('\0') {
        return Err("Invalid file path: contains null bytes".to_string());
    }
    let fp = std::path::Path::new(&file_path);
    for component in fp.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err("Invalid file path: parent directory traversal not allowed".to_string());
        }
    }
    let denied = ["/proc", "/sys", "/dev", "/etc/shadow", "/etc/passwd", "/etc/ssh"];
    if let Ok(canonical) = fp.canonicalize() {
        let cs = canonical.to_string_lossy();
        if denied.iter().any(|d| cs.starts_with(d)) {
            return Err("Access to system path denied".to_string());
        }
    }

    // Size pre-check before reading into memory
    let metadata = tokio::fs::metadata(&file_path).await
        .map_err(|_| "Failed to read file metadata".to_string())?;
    if metadata.len() > 100 * 1024 * 1024 {
        return Err("File too large (max 100MB)".to_string());
    }

    let file_name = std::path::Path::new(&file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let file_bytes = tokio::fs::read(&file_path).await
        .map_err(|e| format!("Failed to read file: {}", e))?;

    let part = reqwest::multipart::Part::bytes(file_bytes)
        .file_name(file_name)
        .mime_str("application/octet-stream")
        .map_err(|e| format!("MIME error: {}", e))?;

    let form = reqwest::multipart::Form::new()
        .text("purpose", purpose.unwrap_or_else(|| "file-extract".to_string()))
        .part("file", part);

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("Kimi file upload failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("Kimi file upload failed [{}]: {}", status, text));
    }

    let result: serde_json::Value = response.json().await
        .map_err(|e| format!("Failed to parse file response: {}", e))?;

    result["id"].as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "No file ID in response".to_string())
}

/// DeepSeek FIM (Fill-In-the-Middle) code completion
#[tauri::command]
pub async fn deepseek_fim_complete(
    api_key: String,
    base_url: String,
    model: String,
    prompt: String,
    suffix: String,
    max_tokens: Option<u32>,
) -> Result<String, String> {
    let client = &*AI_HTTP_CLIENT;
    // FIM uses the beta completions endpoint
    let url = format!("{}/beta/completions", base_url.trim_end_matches("/v1"));

    let body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "suffix": suffix,
        "max_tokens": max_tokens.unwrap_or(128),
        "temperature": 0,
        "stop": ["\n\n", "\r\n\r\n"],
    });

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("DeepSeek FIM request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("DeepSeek FIM failed [{}]: {}", status, text));
    }

    let result: serde_json::Value = response.json().await
        .map_err(|e| format!("Failed to parse FIM response: {}", e))?;

    result["choices"][0]["text"].as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "No completion text in response".to_string())
}
