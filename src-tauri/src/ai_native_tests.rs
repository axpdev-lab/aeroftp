// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::*;

fn request(provider: &str, model: &str) -> AIRequest {
    serde_json::from_value(json!({
        "provider_type":provider,"model":model,"base_url":"https://api.example.test/v1",
        "turn_scope":"foreground-1","messages":[{"role":"user","content":"inspect"}],
        "max_tokens":4096,"temperature":0.3,"top_p":0.4,"top_k":20,"thinking_budget":0,
        "tools":[{"name":"inspect","description":"Inspect","parameters":{"type":"object"}}]
    }))
    .unwrap()
}

fn assistant(request: &AIRequest, payload: Value) -> crate::ai::ChatMessage {
    let mut message: crate::ai::ChatMessage =
        serde_json::from_value(json!({"role":"assistant","content":""})).unwrap();
    message.native_turn = capture(request, payload);
    message
}

fn result(id: &str, content: &str) -> crate::ai::ChatMessage {
    serde_json::from_value(json!({"role":"tool","content":content,"tool_call_id":id})).unwrap()
}

#[test]
fn modern_efforts_and_sampling_are_model_aware() {
    for (provider, model, expected) in [
        ("kimi", "kimi-k3", "low"),
        ("xai", "grok-4.7", "low"),
        ("openai", "gpt-6-sol", "none"),
    ] {
        let mut req = request(provider, model);
        for stream in [false, true] {
            let body = chat_body(&req, stream).unwrap();
            assert_eq!(body["reasoning_effort"], expected);
            for field in ["temperature", "top_p", "top_k", "thinking_budget"] {
                assert!(body.get(field).is_none());
            }
        }
        req.reasoning_effort = Some("ultra".into());
        assert!(chat_body(&req, false).is_err());
    }
    let mut kimi = request("kimi", "kimi-k3");
    kimi.thinking_budget = Some(6000);
    assert_eq!(chat_body(&kimi, false).unwrap()["reasoning_effort"], "high");
    kimi.thinking_budget = Some(100000);
    assert_eq!(chat_body(&kimi, false).unwrap()["reasoning_effort"], "max");
    assert_eq!(
        chat_body(&kimi, false).unwrap()["max_completion_tokens"],
        4096
    );
    let mut grok = request("xai", "grok-4.7");
    grok.thinking_budget = Some(100000);
    assert_eq!(chat_body(&grok, true).unwrap()["reasoning_effort"], "xhigh");
}

#[test]
fn gpt6_tool_transport_never_silently_falls_back() {
    let astra = request("openai", "gpt-6-astra");
    assert!(chat_body(&astra, false).is_err());
    let mut sol = request("openai", "gpt-6-sol");
    sol.thinking_budget = Some(20000);
    assert!(chat_body(&sol, true).is_err());
    sol.use_responses_api = Some(true);
    assert_eq!(
        crate::openai_responses::build_request_body(&sol, true).unwrap()["reasoning"]["effort"],
        "medium"
    );
    let mut astra = astra;
    astra.use_responses_api = Some(true);
    assert_eq!(
        crate::openai_responses::build_request_body(&astra, false).unwrap()["reasoning"]["effort"],
        "low"
    );
}

#[test]
fn anthropic_always_on_adaptive_and_no_sampling_on_both_paths() {
    for model in ["claude-opus-5-5", "claude-fable-5-1"] {
        let req = request("anthropic", model);
        for stream in [false, true] {
            let body = anthropic_body(&req, stream).unwrap();
            assert_eq!(body["thinking"]["type"], "adaptive");
            assert_eq!(body["output_config"]["effort"], "low");
            for key in ["temperature", "top_p", "top_k", "tool_choice"] {
                assert!(body.get(key).is_none());
            }
        }
    }
}

#[test]
fn two_kimi_tool_round_trips_preserve_complete_assistant_and_errors() {
    let mut req = request("kimi", "kimi-k3");
    for id in ["call-1", "call-2"] {
        let payload = json!({"role":"assistant","content":null,"reasoning_content":"opaque thinking","vendor_extension":{"token":"keep"},"tool_calls":[{"id":id,"type":"function","function":{"name":"inspect","arguments":"{}"}}]});
        let parsed = parse(
            &req,
            &json!({"choices":[{"message":payload,"finish_reason":"tool_calls"}]}),
        )
        .unwrap();
        let mut message = assistant(&req, payload.clone());
        message.native_turn = parsed.native_turn;
        req.messages.push(message);
        assert!(chat_body(&req, false).is_err()); // Missing tool result.
        req.messages
            .push(result(id, "Error: file absent; choose another path"));
        let body = chat_body(&req, false).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[messages.len() - 2], payload);
        assert_eq!(messages.last().unwrap()["tool_call_id"], id);
    }
    assert_eq!(
        chat_body(&req, true).unwrap()["messages"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
}

#[test]
fn anthropic_parallel_results_follow_unchanged_thinking_and_tool_blocks() {
    let mut req = request("anthropic", "claude-opus-5-5");
    for n in [1, 2] {
        let a = format!("a{n}");
        let b = format!("b{n}");
        let payload = json!([
            {"type":"thinking","thinking":"","signature":"opaque-signed"},
            {"type":"redacted_thinking","data":"opaque-redacted"},
            {"type":"tool_use","id":a,"name":"inspect","input":{}},
            {"type":"tool_use","id":b,"name":"inspect","input":{}}
        ]);
        req.messages.push(assistant(&req, payload.clone()));
        req.messages.push(result(&a, "ok"));
        assert!(anthropic_body(&req, false).is_err());
        req.messages.push(result(&b, "Error: not found"));
        let body = anthropic_body(&req, true).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[messages.len() - 2]["content"], payload);
        assert_eq!(
            messages.last().unwrap()["content"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
}

#[test]
fn responses_replay_two_turns_including_encrypted_reasoning_and_phase() {
    let mut req = request("openai", "gpt-6-astra");
    req.use_responses_api = Some(true);
    for id in ["first", "second"] {
        let output = json!([
            {"type":"reasoning","id":"rs1","encrypted_content":"opaque","summary":[]},
            {"type":"message","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"checking"}]},
            {"type":"function_call","call_id":id,"name":"inspect","arguments":"{}"}
        ]);
        req.messages.push(assistant(&req, output.clone()));
        req.messages.push(result(id, "ok"));
        let body = crate::openai_responses::build_request_body(&req, false).unwrap();
        let input = body["input"].as_array().unwrap();
        assert_eq!(
            &input[input.len() - 4..input.len() - 1],
            output.as_array().unwrap()
        );
        assert_eq!(input.last().unwrap()["call_id"], id);
        assert_eq!(body["store"], false);
    }
}

#[test]
fn reject_scope_model_endpoint_transport_changes_and_orphan_results() {
    let mut req = request("kimi", "kimi-k3");
    req.messages.push(assistant(
        &req,
        json!({"role":"assistant","content":"hello"}),
    ));
    for field in ["scope", "model", "endpoint", "provider", "transport"] {
        let mut changed = req.clone();
        match field {
            "scope" => changed.turn_scope = Some("branch-2".into()),
            "model" => changed.model = "other".into(),
            "endpoint" => changed.base_url = "https://other.test/v1".into(),
            "provider" => changed.provider_type = AIProviderType::Custom,
            _ => {
                changed.provider_type = AIProviderType::OpenAI;
                changed.use_responses_api = Some(true);
            }
        }
        assert!(validate_history(&changed).is_err(), "{field}");
    }
    req.messages.push(result("orphan", "bad"));
    assert!(validate_history(&req).is_err());
}

#[test]
fn kimi_stream_retains_reasoning_unknown_fields_and_split_tool_arguments() {
    let req = request("kimi", "kimi-k3");
    let mut state = StreamState::default();
    for delta in [
        json!({"role":"assistant","reasoning_content":"reason","vendor_state":"opaque"}),
        json!({"reasoning_content":"ing","tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"inspect","arguments":"{\"path\":"}}]}),
        json!({"tool_calls":[{"index":0,"function":{"arguments":"\"file\"}"}}]}),
    ] {
        state
            .ingest(
                &json!({"choices":[{"delta":delta,"finish_reason":null}]}),
                false,
            )
            .unwrap();
    }
    assert!(state.finish(&req).is_err()); // Drop before terminal marker.
    state
        .ingest(
            &json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
            false,
        )
        .unwrap();
    assert!(state.finish(&req).is_err()); // Still missing [DONE].
    state.complete = true;
    let parsed = state.finish(&req).unwrap();
    let native = parsed.native_turn.unwrap();
    assert_eq!(native.payload["reasoning_content"], "reasoning");
    assert_eq!(native.payload["vendor_state"], "opaque");
    assert_eq!(
        parsed.tool_calls.unwrap()[0].arguments,
        json!({"path":"file"})
    );
}

#[test]
fn anthropic_stream_retains_signature_redacted_block_and_tool_input() {
    let req = request("anthropic", "claude-opus-5-5");
    let mut state = StreamState::default();
    let events = vec![
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"signed"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"hidden"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"c","name":"inspect","input":{}}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"f\"}"}}),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":3}}),
        json!({"type":"message_stop"}),
    ];
    for event in events {
        state.ingest(&event, true).unwrap();
    }
    let parsed = state.finish(&req).unwrap();
    let native = parsed.native_turn.unwrap();
    assert_eq!(native.payload[0]["signature"], "signed");
    assert_eq!(native.payload[1]["data"], "hidden");
    assert_eq!(native.payload[2]["input"], json!({"path":"f"}));
}

#[test]
fn incomplete_responses_never_publish_executable_tools_or_native_state() {
    let req = request("kimi", "kimi-k3");
    let value = json!({"choices":[{"message":{"role":"assistant","content":"partial","tool_calls":[{"id":"c","function":{"name":"inspect","arguments":"{}"}}]},"finish_reason":"length"}]});
    let parsed = parse(&req, &value).unwrap();
    assert!(parsed.native_turn.is_none());
    assert!(parsed.tool_calls.is_none());
    assert!(parsed.content.contains("incomplete"));
    let mut state = StreamState::default();
    assert!(state
        .ingest(&json!({"type":"error","error":{"message":"failed"}}), true)
        .is_err());
    assert!(state.finish(&req).is_err());
}

#[test]
fn native_debug_does_not_reveal_opaque_content() {
    let req = request("kimi", "kimi-k3");
    let turn = capture(&req, json!({"secret":"do-not-log"})).unwrap();
    assert!(!format!("{turn:?}").contains("do-not-log"));
}

#[derive(Default)]
struct Sink(std::sync::Mutex<Vec<crate::ai_stream::StreamChunk>>);
impl crate::ai_core::EventSink for Sink {
    fn emit_stream_chunk(&self, _: &str, chunk: &crate::ai_stream::StreamChunk) {
        self.0.lock().unwrap().push(chunk.clone());
    }
    fn emit_tool_progress(&self, _: &crate::ai_core::ToolProgress) {}
    fn emit_app_control(&self, _: &str, _: &Value) {}
}

#[tokio::test]
async fn cancelled_native_stream_never_emits_success_or_tools() {
    let req = request("kimi", "kimi-k3");
    let sink = Sink::default();
    let cancel = std::sync::atomic::AtomicBool::new(true);
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        stream(&reqwest::Client::new(), &req, &sink, "cancelled", &cancel),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(sink.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn actual_sse_reader_publishes_native_tools_only_after_terminal_marker() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for complete in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut chunk = [0u8; 4096];
            let (header_end, length) = loop {
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                received.extend_from_slice(&chunk[..n]);
                if let Some(index) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&received[..index]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap();
                    break (index + 4, length);
                }
            };
            while received.len() < header_end + length {
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                received.extend_from_slice(&chunk[..n]);
            }
            let body: Value =
                serde_json::from_slice(&received[header_end..header_end + length]).unwrap();
            assert_eq!(body["stream"], true);
            assert!(body.get("temperature").is_none());
            let delta = json!({"choices":[{"delta":{"role":"assistant","reasoning_content":"opaque","tool_calls":[{"index":0,"id":"c","type":"function","function":{"name":"inspect","arguments":"{}"}}]},"finish_reason":null}]});
            let terminal = json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":3}});
            let events = format!(
                "data: {delta}\n\ndata: {terminal}\n\n{}",
                if complete { "data: [DONE]\n\n" } else { "" }
            );
            let response=format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{events}",events.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let mut req = request("kimi", "kimi-k3");
        req.base_url = format!("http://{address}/v1");
        let sink = Sink::default();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream(&reqwest::Client::new(), &req, &sink, "fixture", &cancel),
        )
        .await
        .unwrap();
        server.await.unwrap();
        let chunks = sink.0.lock().unwrap();
        if complete {
            outcome.unwrap();
            let final_chunk = chunks.last().unwrap();
            assert!(final_chunk.done);
            assert_eq!(final_chunk.tool_calls.as_ref().unwrap()[0].id, "c");
            assert_eq!(
                final_chunk.native_turn.as_ref().unwrap().payload["reasoning_content"],
                "opaque"
            );
        } else {
            assert!(outcome.is_err());
            assert!(chunks.iter().all(|chunk| !chunk.done
                && chunk.tool_calls.is_none()
                && chunk.native_turn.is_none()));
        }
    }
}

#[test]
fn top_level_results_are_validated_like_message_results() {
    let mut req = request("kimi", "kimi-k3");
    req.messages.push(assistant(&req, json!({"role":"assistant","tool_calls":[{"id":"c1","type":"function","function":{"name":"inspect","arguments":"{}"}}]})));
    req.tool_results =
        Some(serde_json::from_value(json!([{"tool_call_id":"c1","content":"ok"}])).unwrap());
    assert!(chat_body(&req, false).is_ok());
    req.messages.push(result("c1", "duplicate"));
    assert!(chat_body(&req, false).is_err());
}

#[test]
fn invalid_tool_identity_or_arguments_cannot_be_executed() {
    let req = request("kimi", "kimi-k3");
    for (id, args) in [("", "{}"), ("c1", "[]"), ("c1", "null")] {
        let response = json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","tool_calls":[{"id":id,"function":{"name":"inspect","arguments":args}}]}}]});
        assert!(parse(&req, &response).is_err());
    }
}

#[test]
fn refusal_is_visible_without_exposing_reasoning() {
    let req = request("openai", "gpt-6-sol");
    let parsed = parse(&req, &json!({"choices":[{"message":{"role":"assistant","content":null,"refusal":"Cannot help with this","reasoning_content":"opaque"},"finish_reason":"stop"}]})).unwrap();
    assert_eq!(parsed.content, "Cannot help with this");
    assert!(parsed.tool_calls.is_none());
    let mut state = StreamState::default();
    let (text, thinking) = state
        .ingest(
            &json!({"choices":[{"delta":{"refusal":"Cannot help with this"}}]}),
            false,
        )
        .unwrap();
    assert_eq!(text, parsed.content);
    assert!(thinking.is_empty());
}

#[test]
fn historical_anthropic_tool_records_remain_paired_without_opaque_storage() {
    let mut req = request("anthropic", "claude-fable-5-1");
    req.messages.push(serde_json::from_value(json!({"role":"assistant","content":"Inspecting","tool_calls_echo":[{"id":"old","name":"inspect","arguments":"{}"}]})).unwrap());
    req.messages.push(result("old", "old result"));
    req.messages
        .push(serde_json::from_value(json!({"role":"user","content":"Next task"})).unwrap());
    let body = anthropic_body(&req, false).unwrap();
    assert_eq!(body["messages"][1]["content"][1]["type"], "tool_use");
    assert_eq!(body["messages"][1]["content"][1]["id"], "old");
    assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "old");
    assert!(!body.to_string().contains("signature"));
}
