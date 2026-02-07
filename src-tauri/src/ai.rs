// AI Provider Integration Module for AeroFTP
// Supports: Google Gemini, OpenAI, Anthropic, xAI, OpenRouter, Ollama

use serde::{Deserialize, Serialize};
use reqwest::Client;

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
        serializer.serialize_str(&self.to_string())
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

        let gemini_request = GeminiRequest {
            contents: request.messages.iter().map(|m| {
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
            }),
            tools: gemini_tools,
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

        // Extract text parts
        let content = candidate.content.parts.iter()
            .filter_map(|p| p.text.as_ref())
            .cloned()
            .collect::<Vec<_>>()
            .join("");

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
        let tools = request.tools.as_ref().map(|defs| {
            defs.iter().map(|d| OpenAITool {
                tool_type: "function".to_string(),
                function: OpenAIFunction {
                    name: d.name.clone(),
                    description: d.description.clone(),
                    parameters: d.parameters.clone(),
                },
            }).collect()
        });

        let openai_request = OpenAIRequest {
            model: request.model.clone(),
            messages,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            tools,
        };

        let response = client
            .post(&url)
            .headers(headers)
            .json(&openai_request)
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
            return Err(AIError::Api(format!("HTTP {} — {}", status, &body[..body.len().min(500)])));
        }

        let openai_response: OpenAIResponse = serde_json::from_str(&body)
            .map_err(|e| AIError::InvalidResponse(format!("JSON parse error: {} — body: {}", e, &body[..body.len().min(200)])))?;

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
                arguments: serde_json::from_str(&tc.function.arguments).unwrap_or_default(),
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

        let anthropic_request = AnthropicRequest {
            model: request.model.clone(),
            messages: request.messages.iter().map(|m| AnthropicMessage {
                role: m.role.clone(),
                content: m.to_anthropic_content(),
            }).collect(),
            max_tokens: request.max_tokens.unwrap_or(4096),
            temperature: request.temperature,
            tools,
        };

        let response = client
            .post(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&anthropic_request)
            .send()
            .await?;

        let anthropic_response: AnthropicResponse = response.json().await?;

        if let Some(error) = anthropic_response.error {
            return Err(AIError::Api(error.message));
        }

        let blocks = anthropic_response.content.as_ref();

        // Extract text content
        let content = blocks
            .and_then(|b| {
                b.iter()
                    .filter(|c| c.content_type.as_deref() == Some("text") || c.content_type.is_none())
                    .filter_map(|c| c.text.as_ref())
                    .next()
            })
            .cloned()
            .unwrap_or_default();

        // Extract tool_use blocks
        let tool_calls: Option<Vec<AIToolCall>> = blocks.and_then(|b| {
            let calls: Vec<AIToolCall> = b.iter()
                .filter(|c| c.content_type.as_deref() == Some("tool_use"))
                .filter_map(|c| {
                    Some(AIToolCall {
                        id: c.id.clone()?,
                        name: c.name.clone()?,
                        arguments: c.input.clone().unwrap_or_default(),
                    })
                })
                .collect();
            if calls.is_empty() { None } else { Some(calls) }
        });

        let input_tokens = anthropic_response.usage.as_ref().and_then(|u| u.input_tokens);
        let output_tokens = anthropic_response.usage.as_ref().and_then(|u| u.output_tokens);
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
        })
    }
}

// Main AI call function
pub async fn call_ai(request: AIRequest) -> Result<AIResponse, AIError> {
    let client = Client::new();

    match request.provider_type {
        AIProviderType::Google => gemini::call(&client, &request).await,
        AIProviderType::OpenAI => openai_compat::call(&client, &request, "/chat/completions").await,
        AIProviderType::XAI => openai_compat::call(&client, &request, "/chat/completions").await,
        AIProviderType::OpenRouter => openai_compat::call(&client, &request, "/chat/completions").await,
        AIProviderType::Ollama => openai_compat::call(&client, &request, "/api/chat").await,
        AIProviderType::Anthropic => anthropic::call(&client, &request).await,
        AIProviderType::Custom => openai_compat::call(&client, &request, "/chat/completions").await,
    }
}

// Test provider connection
pub async fn test_provider(provider_type: AIProviderType, base_url: String, api_key: Option<String>) -> Result<bool, AIError> {
    let client = Client::new();

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
