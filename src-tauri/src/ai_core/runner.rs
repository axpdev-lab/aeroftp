//! Foreground agent loop shared by surface adapters. This is not a worker sandbox:
//! delegated authority, credentials and aggregate reservations are a later layer.
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::HashMap;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::ai::{AIRequest, AIResponse, AIToolCall, ChatMessage, ToolCallEcho};

pub const CANCELLED: &str = "Agent run cancelled";

pub fn check_cancelled(cancel: &CancellationToken) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err(CANCELLED.to_string())
    } else {
        Ok(())
    }
}

/// UI, approval, transport and accounting remain with the owning surface.
/// Transport futures must clean up on drop. Tool execution is instead awaited to
/// quiescence: dropping a future does not prove its blocking work has stopped.
#[async_trait]
pub trait RunnerAdapter: Sync {
    async fn complete(
        &self,
        request: AIRequest,
        cancel: &CancellationToken,
    ) -> Result<AIResponse, String>;
    fn account(&self, response: &AIResponse) -> Result<(), String>;
    /// Must recheck cancellation after any approval wait and before dispatch.
    /// Recoverable tool errors/denials are returned as text, fatal errors as Err.
    /// Err(CANCELLED) means no dispatch occurred. If work has already started,
    /// await it and return its real outcome even if cancelled, for the audit trail.
    async fn execute(
        &self,
        call: &AIToolCall,
        cancel: &CancellationToken,
    ) -> Result<String, String>;
    fn assistant_tools(&self, _response: &AIResponse) {}
    fn continuing(&self) {}
}

pub struct RunnerOptions {
    pub max_steps: u32,
    pub plan_only: bool,
}

fn message(role: &str, content: String) -> ChatMessage {
    ChatMessage {
        native_turn: None,
        role: role.to_string(),
        content,
        images: None,
        tool_calls_echo: None,
        tool_call_id: None,
    }
}

/// The template pins provider/model/endpoint/tools for this invocation. Its
/// messages form the fixed prefix (CLI system prompt); supplied history is plain
/// conversation only. Native envelopes live in this stack frame, never history.
pub async fn run(
    adapter: &impl RunnerAdapter,
    template: &AIRequest,
    messages: &mut Vec<ChatMessage>,
    options: RunnerOptions,
    cancel: &CancellationToken,
) -> Result<String, String> {
    check_cancelled(cancel)?;
    if template
        .messages
        .iter()
        .chain(messages.iter())
        .any(|m| m.native_turn.is_some())
    {
        return Err("A new agent run cannot inherit native continuation state".to_string());
    }
    let turn_scope = uuid::Uuid::new_v4().to_string();
    let mut native_turns: HashMap<usize, crate::ai_native::NativeTurn> = HashMap::new();
    let mut steps = 0u32;

    loop {
        check_cancelled(cancel)?;
        let mut request = template.clone();
        request.turn_scope = Some(turn_scope.clone());
        request.tool_results = None;
        request.messages.extend_from_slice(messages);
        for (index, native) in &native_turns {
            request.messages[template.messages.len() + index].native_turn = Some(native.clone());
        }
        // The token exists before the first poll/stream registration. Dropping a
        // pending HTTP future stops transport even before response headers arrive.
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(CANCELLED.to_string()),
            response = adapter.complete(request, cancel) => response?,
        };
        // Known usage must survive cancellation; only publication/dispatch stops.
        let accounting = adapter.account(&response);
        check_cancelled(cancel)?;
        accounting?;

        let Some(calls) = response
            .tool_calls
            .as_ref()
            .filter(|calls| !calls.is_empty())
        else {
            return Ok(response.content);
        };
        if options.plan_only {
            let lines: Vec<String> = calls
                .iter()
                .map(|call| {
                    format!(
                        "- {} {}",
                        call.name,
                        serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".to_string())
                    )
                })
                .collect();
            let separator = if response.content.is_empty() {
                ""
            } else {
                "\n\n"
            };
            return Ok(format!(
                "{}{separator}Planned tool calls:\n{}",
                response.content,
                lines.join("\n")
            ));
        }
        steps += 1;
        if steps > options.max_steps {
            if !response.content.is_empty() {
                messages.push(message("assistant", response.content.clone()));
            }
            return Ok(format!(
                "{}\n\n[Reached max steps limit ({}).]",
                response.content, options.max_steps
            ));
        }

        adapter.assistant_tools(&response);
        let mut assistant = message("assistant", response.content.clone());
        assistant.tool_calls_echo = Some(
            calls
                .iter()
                .map(|call| ToolCallEcho {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: serde_json::to_string(&call.arguments).unwrap_or_default(),
                })
                .collect(),
        );
        if let Some(native) = response.native_turn {
            native_turns.insert(messages.len(), native);
        }

        // Keep completed effects in the audit history, including on cancellation.
        // Every unexecuted call receives an explicit interruption result so a
        // later user turn cannot replay an orphaned ID or silently repeat a write.
        let mut group = vec![assistant];
        let mut interrupted = None;
        let mut has_outcome = false;
        for call in calls {
            if interrupted.is_none() && cancel.is_cancelled() {
                interrupted = Some(CANCELLED.to_string());
            }
            let content = if interrupted.is_some() {
                "Tool call not dispatched because the agent run was interrupted.".to_string()
            } else {
                match adapter.execute(call, cancel).await {
                    Ok(content) => {
                        has_outcome = true;
                        content
                    }
                    Err(error) => {
                        let content = if error == CANCELLED {
                            "Tool call not dispatched because the agent run was cancelled."
                        } else {
                            has_outcome = true;
                            "Tool execution interrupted; outcome unknown. Verify effects before retrying."
                        };
                        interrupted = Some(error);
                        content.to_string()
                    }
                }
            };
            let mut result = message("tool", content);
            result.tool_call_id = Some(call.id.clone());
            group.push(result);
        }
        if has_outcome {
            messages.extend(group);
        }
        check_cancelled(cancel)?;
        if let Some(error) = interrupted {
            return Err(error);
        }
        adapter.continuing();
    }
}

#[cfg(test)]
mod tests;
