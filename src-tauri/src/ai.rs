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

// Chat message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
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
}

// AI Response to frontend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIResponse {
    pub content: String,
    pub model: String,
    pub tokens_used: Option<u32>,
    pub finish_reason: Option<String>,
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
    }

    #[derive(Serialize)]
    pub struct GeminiContent {
        pub role: String,
        pub parts: Vec<GeminiPart>,
    }

    #[derive(Serialize)]
    pub struct GeminiPart {
        pub text: String,
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
        pub text: String,
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

        let gemini_request = GeminiRequest {
            contents: request.messages.iter().map(|m| GeminiContent {
                role: if m.role == "user" { "user".to_string() } else { "model".to_string() },
                parts: vec![GeminiPart { text: m.content.clone() }],
            }).collect(),
            generation_config: Some(GeminiGenerationConfig {
                max_output_tokens: request.max_tokens,
                temperature: request.temperature,
            }),
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

        let content = gemini_response
            .candidates
            .and_then(|c| c.into_iter().next())
            .map(|c| {
                c.content.parts.iter().map(|p| p.text.clone()).collect::<Vec<_>>().join("")
            })
            .ok_or_else(|| AIError::InvalidResponse("No content in response".to_string()))?;

        Ok(AIResponse {
            content,
            model: request.model.clone(),
            tokens_used: None,
            finish_reason: None,
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
    }

    #[derive(Serialize)]
    pub struct OpenAIMessage {
        pub role: String,
        pub content: String,
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
    }

    #[derive(Deserialize)]
    pub struct OpenAIError {
        pub message: String,
    }

    #[derive(Deserialize)]
    pub struct OpenAIUsage {
        pub total_tokens: Option<u32>,
    }

    pub async fn call(client: &Client, request: &AIRequest, endpoint: &str) -> Result<AIResponse, AIError> {
        let url = format!("{}{}", request.base_url, endpoint);

        let mut headers = reqwest::header::HeaderMap::new();
        
        if let Some(api_key) = &request.api_key {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", api_key).parse().unwrap(),
            );
        }

        // OpenRouter requires additional headers
        if request.provider_type == AIProviderType::OpenRouter {
            headers.insert(
                "HTTP-Referer",
                "https://aeroftp.app".parse().unwrap(),
            );
            headers.insert(
                "X-Title",
                "AeroFTP".parse().unwrap(),
            );
        }

        let openai_request = OpenAIRequest {
            model: request.model.clone(),
            messages: request.messages.iter().map(|m| OpenAIMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            }).collect(),
            max_tokens: request.max_tokens,
            temperature: request.temperature,
        };

        let response = client
            .post(&url)
            .headers(headers)
            .json(&openai_request)
            .send()
            .await?;

        let openai_response: OpenAIResponse = response.json().await?;

        if let Some(error) = openai_response.error {
            return Err(AIError::Api(error.message));
        }

        let choice = openai_response
            .choices
            .and_then(|c| c.into_iter().next())
            .ok_or_else(|| AIError::InvalidResponse("No choices in response".to_string()))?;

        Ok(AIResponse {
            content: choice.message.content.unwrap_or_default(),
            model: request.model.clone(),
            tokens_used: openai_response.usage.and_then(|u| u.total_tokens),
            finish_reason: choice.finish_reason,
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
    }

    #[derive(Serialize)]
    pub struct AnthropicMessage {
        pub role: String,
        pub content: String,
    }

    #[derive(Deserialize)]
    pub struct AnthropicResponse {
        pub content: Option<Vec<AnthropicContent>>,
        pub error: Option<AnthropicError>,
        pub stop_reason: Option<String>,
    }

    #[derive(Deserialize)]
    pub struct AnthropicContent {
        pub text: String,
    }

    #[derive(Deserialize)]
    pub struct AnthropicError {
        pub message: String,
    }

    pub async fn call(client: &Client, request: &AIRequest) -> Result<AIResponse, AIError> {
        let api_key = request.api_key.as_ref().ok_or(AIError::MissingApiKey)?;
        
        let url = format!("{}/messages", request.base_url);

        let anthropic_request = AnthropicRequest {
            model: request.model.clone(),
            messages: request.messages.iter().map(|m| AnthropicMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            }).collect(),
            max_tokens: request.max_tokens.unwrap_or(4096),
            temperature: request.temperature,
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

        let content = anthropic_response
            .content
            .and_then(|c| c.into_iter().next())
            .map(|c| c.text)
            .ok_or_else(|| AIError::InvalidResponse("No content in response".to_string()))?;

        Ok(AIResponse {
            content,
            model: request.model.clone(),
            tokens_used: None,
            finish_reason: anthropic_response.stop_reason,
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
