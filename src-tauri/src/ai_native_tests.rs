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
fn model_studio_contract_is_endpoint_scoped_and_preserves_reasoning() {
    for provider in ["custom", "qwen"] {
        for model in [
            "qwen3.8-flash",
            "qwen3.8-max-0902",
            "qwen3.8-2.4t-a95b",
            "deepseek-v4-pro-0813",
            "deepseek-v4.1-flash",
            "kimi-k3",
        ] {
            let mut req = request(provider, model);
            req.base_url =
                "https://llm-test.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1".into();
            assert!(modern_chat(&req));
            let payload = json!({"role":"assistant","content":null,"reasoning_content":"opaque","tool_calls":[{"id":"read-1","type":"function","function":{"name":"inspect","arguments":"{}"}}]});
            req.messages.push(assistant(&req, payload.clone()));
            req.messages.push(result("read-1", "missing file"));
            for stream in [false, true] {
                let body = chat_body(&req, stream).unwrap();
                assert_eq!(body["messages"][1], payload);
                assert_eq!(body["max_tokens"], 4096);
                assert!(body.get("thinking_budget").is_none());
            }
            req.thinking_budget = Some(20000);
            assert_eq!(
                effort(&req).unwrap().unwrap(),
                if model.starts_with("qwen") {
                    "xhigh"
                } else {
                    "high"
                }
            );
            req.web_search = Some(true);
            let body = chat_body(&req, false).unwrap();
            if model == "kimi-k3" {
                // Hosted search is not part of the Kimi contract: run without it.
                assert!(body.get("enable_search").is_none());
            } else {
                assert_eq!(body["enable_search"], true);
            }
        }
    }
    let mut req = request("custom", "kimi-k3");
    for url in [
        "https://dashscope-intl.aliyuncs.com.evil.test/compatible-mode/v1",
        "http://dashscope-intl.aliyuncs.com/compatible-mode/v1",
        "https://user@dashscope-intl.aliyuncs.com/compatible-mode/v1",
        "https://dashscope-intl.aliyuncs.com/compatible-mode/v1?route=other",
        "https://dashscope-intl.aliyuncs.com:8443/compatible-mode/v1",
        "https://llm-test.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
        "https://nested.llm-test.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
    ] {
        req.base_url = url.into();
        assert!(!modern_chat(&req), "{url}");
    }
    req.base_url = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/".into();
    assert!(modern_chat(&req));
    req.model = "unknown".into();
    assert!(!modern_chat(&req));
}

#[test]
fn gateway_continuations_keep_reasoning_and_provider_identity() {
    for (provider, model) in [
        ("nvidia", "moonshotai/kimi-k3"),
        ("nvidia", "z-ai/glm-5.3"),
        ("openrouter", "qwen/qwen3.8-27b:free"),
    ] {
        let mut req = request(provider, model);
        assert!(modern_chat(&req));
        let payload = json!({"role":"assistant","content":null,"reasoning_content":"opaque","reasoning_details":[{"type":"reasoning.encrypted","data":"private"}],"tool_calls":[{"id":"read-1","type":"function","function":{"name":"inspect","arguments":"{}"}}]});
        req.messages.push(assistant(&req, payload.clone()));
        req.messages.push(result("read-1", "missing file"));
        for stream in [false, true] {
            let body = chat_body(&req, stream).unwrap();
            assert_eq!(body["messages"][1], payload);
            assert_eq!(body["messages"][2]["tool_call_id"], "read-1");
            assert_eq!(body["max_tokens"], 4096);
            assert!(body.get("max_completion_tokens").is_none());
            if provider == "nvidia" {
                assert_eq!(body["reasoning_effort"], "low");
            }
        }
        req.provider_type = crate::ai::AIProviderType::Custom;
        assert!(validate_history(&req).is_err());
    }
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
    // Under store:false a reasoning item is replayable only with its encrypted
    // content, which the provider returns only when the request asks for it.
    let first = crate::openai_responses::build_request_body(&req, true).unwrap();
    assert_eq!(first["include"], json!(["reasoning.encrypted_content"]));
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
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
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
fn openrouter_stream_replays_reasoning_detail_chunks_verbatim() {
    let mut req = request("openrouter", "anthropic/claude-fable-5.1");
    let mut state = StreamState::default();
    let details = vec![
        json!({"index":0,"id":"reasoning-text-1","type":"reasoning.text","format":"anthropic-claude-v1","text":"first","signature":null}),
        json!({"index":0,"id":"reasoning-text-1","type":"reasoning.text","format":"anthropic-claude-v1","text":"second","signature":"signed"}),
        json!({"id":null,"type":"reasoning.encrypted","format":"unknown","data":"opaque"}),
    ];
    for detail in &details {
        state.ingest(&json!({"choices":[{"delta":{"role":"assistant","reasoning_details":[detail]},"finish_reason":null}]}), false).unwrap();
    }
    state.ingest(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"inspect","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}), false).unwrap();
    state.complete = true;
    let response = state.finish(&req).unwrap();
    let turn = response.native_turn.unwrap();
    assert_eq!(turn.payload["reasoning_details"], json!(details));
    let mut message: crate::ai::ChatMessage =
        serde_json::from_value(json!({"role":"assistant","content":""})).unwrap();
    message.native_turn = Some(turn);
    req.messages.push(message);
    req.messages.push(result("c1", "missing file"));
    for stream in [false, true] {
        assert_eq!(
            chat_body(&req, stream).unwrap()["messages"][1]["reasoning_details"],
            json!(details)
        );
    }
    let mut invalid = StreamState::default();
    assert!(invalid
        .ingest(
            &json!({"choices":[{"delta":{"reasoning_details":["invalid"]}}]}),
            false
        )
        .is_err());
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

#[test]
fn responses_replay_drops_reasoning_without_encrypted_content() {
    // A model that reasons by default, with no effort requested (the CLI case).
    let mut req = request("openai", "gpt-6-sol");
    req.use_responses_api = Some(true);
    req.thinking_budget = None;
    let first = crate::openai_responses::build_request_body(&req, false).unwrap();
    assert_eq!(first["include"], json!(["reasoning.encrypted_content"]));
    let output = json!([
        {"type":"reasoning","id":"rs_plain","summary":[]},
        {"type":"reasoning","id":"rs_null","encrypted_content":null,"summary":[]},
        {"type":"reasoning","id":"rs_kept","encrypted_content":"opaque","summary":[]},
        {"type":"function_call","call_id":"c1","name":"inspect","arguments":"{}"}
    ]);
    req.messages.push(assistant(&req, output));
    req.messages.push(result("c1", "ok"));
    for stream in [false, true] {
        let body = crate::openai_responses::build_request_body(&req, stream).unwrap();
        let input = body["input"].as_array().unwrap();
        let reasoning: Vec<&Value> = input.iter().filter(|i| i["type"] == "reasoning").collect();
        assert_eq!(reasoning.len(), 1, "{input:?}");
        assert_eq!(reasoning[0]["id"], "rs_kept");
        assert_eq!(input[input.len() - 2]["call_id"], "c1");
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    }
    // A model with no reviewed reasoning contract is not asked for reasoning
    // content it may reject, unless it already produced reasoning to replay.
    let mut plain = request("openai", "gpt-4.1");
    plain.use_responses_api = Some(true);
    assert!(crate::openai_responses::build_request_body(&plain, false)
        .unwrap()
        .get("include")
        .is_none());
}

#[test]
fn chat_deltas_skip_null_identity_and_keep_repeated_ids_whole() {
    let req = request("nvidia", "z-ai/glm-5.3");
    let mut state = StreamState::default();
    for delta in [
        json!({"role":"assistant","content":"","tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"inspect","arguments":""}}]}),
        json!({"role":null,"content":null,"tool_calls":[{"index":0,"id":"call-1","type":null,"function":{"name":"inspect","arguments":"{\"path\":"}}]}),
        json!({"role":null,"tool_calls":[{"index":0,"id":"","type":"function","function":{"name":null,"arguments":"\"f\"}"}}]}),
    ] {
        state
            .ingest(
                &json!({"choices":[{"delta":delta,"finish_reason":null}]}),
                false,
            )
            .unwrap();
    }
    state
        .ingest(
            &json!({"choices":[{"delta":{"role":null},"finish_reason":"tool_calls"}]}),
            false,
        )
        .unwrap();
    state.complete = true;
    let parsed = state.finish(&req).unwrap();
    let call = &parsed.tool_calls.unwrap()[0];
    assert_eq!(
        (call.id.as_str(), call.name.as_str()),
        ("call-1", "inspect")
    );
    assert_eq!(call.arguments, json!({"path":"f"}));
    let native = parsed.native_turn.unwrap();
    assert_eq!(native.payload["role"], "assistant");
    assert_eq!(native.payload["tool_calls"][0]["id"], "call-1");

    // A different non-empty identity is still a conflict, never a merge.
    for conflicting in [
        json!({"role":"user"}),
        json!({"tool_calls":[{"index":0,"id":"call-2"}]}),
        json!({"tool_calls":[{"index":0,"function":{"name":"delete"}}]}),
    ] {
        let mut state = StreamState::default();
        state
            .ingest(&json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"inspect","arguments":"{}"}}]}}]}), false)
            .unwrap();
        assert!(
            state
                .ingest(&json!({"choices":[{"delta":conflicting}]}), false)
                .is_err(),
            "{conflicting}"
        );
    }
}

#[test]
fn kimi_k3_runs_without_hosted_search_when_the_global_toggle_is_on() {
    let mut req = request("kimi", "kimi-k3");
    req.web_search = Some(true);
    for stream in [false, true] {
        let body = chat_body(&req, stream).unwrap();
        assert!(body.get("enable_search").is_none());
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    }
}

/// One HTTP exchange on 127.0.0.1. Resolves to the lowercased request head
/// and the JSON request body.
async fn one_shot_server(
    status: &'static str,
    content_type: &'static str,
    reply: String,
) -> (String, tokio::task::JoinHandle<(String, Value)>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        let mut chunk = [0u8; 4096];
        let (head, header_end, length) = loop {
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            received.extend_from_slice(&chunk[..n]);
            if let Some(index) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&received[..index]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .map(|v| v.trim().parse::<usize>().unwrap())
                    .unwrap_or(0);
                break (head, index + 4, length);
            }
        };
        while received.len() < header_end + length {
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            received.extend_from_slice(&chunk[..n]);
        }
        let request_body =
            serde_json::from_slice(&received[header_end..header_end + length]).unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
            reply.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        (head, request_body)
    });
    (format!("http://{address}/v1"), server)
}

#[tokio::test]
async fn provider_errors_keep_status_and_a_bounded_body() {
    let client = reqwest::Client::new();
    let mut req = request("nvidia", "z-ai/glm-5.3");
    // call(): an HTML 404 is reported with its status and text, not as a JSON
    // decoding failure.
    let page = format!(
        "<html><body>No route for this model {}</body></html>",
        "x".repeat(4000)
    );
    let (base, server) = one_shot_server("404 Not Found", "text/html", page).await;
    req.base_url = base;
    let err = call(&client, &req).await.unwrap_err().to_string();
    server.await.unwrap();
    assert!(
        err.contains("404") && err.contains("No route for this model"),
        "{err}"
    );
    assert!(err.len() < 1000, "unbounded: {} bytes", err.len());

    // stream(): the body of a refused request survives too.
    let refusal = json!({"error":{"message":"Rate limit reached for this key"}}).to_string();
    let (base, server) =
        one_shot_server("429 Too Many Requests", "application/json", refusal).await;
    req.base_url = base;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let err = stream(&client, &req, &Sink::default(), "refused", &cancel)
        .await
        .unwrap_err()
        .to_string();
    server.await.unwrap();
    assert!(
        err.contains("429") && err.contains("Rate limit reached for this key"),
        "{err}"
    );

    // An SSE error event keeps the provider's message, on both event shapes.
    let err = StreamState::default()
        .ingest(
            &json!({"error":{"message":"Upstream model overloaded"}}),
            false,
        )
        .unwrap_err();
    assert!(err.contains("Upstream model overloaded"), "{err}");
    let err = StreamState::default()
        .ingest(
            &json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}),
            true,
        )
        .unwrap_err();
    assert!(err.contains("Overloaded"), "{err}");
}

#[tokio::test]
async fn text_stream_that_ends_after_its_finish_reason_completes_without_done() {
    let text = json!({"choices":[{"delta":{"role":"assistant","content":"All done."},"finish_reason":null}]});
    let last = json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3}});
    // No [DONE], and the last event has no trailing newline either.
    let (base, server) = one_shot_server(
        "200 OK",
        "text/event-stream",
        format!("data: {text}\n\ndata: {last}"),
    )
    .await;
    let mut req = request("nvidia", "z-ai/glm-5.3");
    req.base_url = base;
    let sink = Sink::default();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    stream(&reqwest::Client::new(), &req, &sink, "no-done", &cancel)
        .await
        .unwrap();
    server.await.unwrap();
    let chunks = sink.0.lock().unwrap();
    let done = chunks.last().unwrap();
    assert!(done.done && done.tool_calls.is_none());
    assert!(done.native_turn.is_some());
    assert_eq!(done.output_tokens, Some(3));
    assert_eq!(
        chunks
            .iter()
            .map(|c| c.content.as_str())
            .collect::<String>(),
        "All done."
    );
}

#[tokio::test]
async fn openrouter_native_requests_carry_attribution_headers() {
    let client = reqwest::Client::new();
    let mut req = request("openrouter", "qwen/qwen3.8-27b:free");
    let last =
        json!({"choices":[{"delta":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]});
    let (base, server) = one_shot_server(
        "200 OK",
        "text/event-stream",
        format!("data: {last}\n\ndata: [DONE]\n\n"),
    )
    .await;
    req.base_url = base;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    stream(&client, &req, &Sink::default(), "openrouter", &cancel)
        .await
        .unwrap();
    let (head, _) = server.await.unwrap();
    assert!(head.contains("http-referer: https://aeroftp.app"), "{head}");
    assert!(head.contains("x-title: aeroftp"), "{head}");

    let reply =
        json!({"choices":[{"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]})
            .to_string();
    let (base, server) = one_shot_server("200 OK", "application/json", reply).await;
    req.base_url = base;
    assert_eq!(call(&client, &req).await.unwrap().content, "hi");
    let (head, _) = server.await.unwrap();
    assert!(head.contains("http-referer: https://aeroftp.app"), "{head}");
    assert!(head.contains("x-title: aeroftp"), "{head}");
}

#[test]
fn provider_error_text_is_scrubbed_before_it_is_cut() {
    // A key that straddles the 500-byte cut must not leave its prefix behind.
    for key in [
        "Bearer sk-proj-abcdefghijklmnopqrstuvwxyz0123456789",
        "sk-proj-abcdefghijklmnopqrstuvwxyz0123456789",
        "nvapi-abcdefghijklmnopqrstuvwxyz0123456789",
    ] {
        for offset in 470..500 {
            let body = format!("{} {key} rejected", "x".repeat(offset));
            let detail = error_detail(&body);
            assert!(detail.len() <= 500, "{offset}: {} bytes", detail.len());
            assert!(
                !detail.contains("proj-ab") && !detail.contains("nvapi-ab"),
                "{offset}: {}",
                &detail[detail.len().saturating_sub(40)..]
            );
        }
    }
}

#[test]
fn responses_continuation_asks_for_reasoning_only_when_it_replays_some() {
    // A model without a reviewed reasoning contract.
    let mut req = request("openai", "gpt-4.1");
    req.use_responses_api = Some(true);
    let plain = json!([
        {"type":"message","role":"assistant","content":[{"type":"output_text","text":"checking"}]},
        {"type":"function_call","call_id":"c1","name":"inspect","arguments":"{}"}
    ]);
    let mut continuation = req.clone();
    continuation.messages.push(assistant(&continuation, plain));
    continuation.messages.push(result("c1", "ok"));
    let body = crate::openai_responses::build_request_body(&continuation, false).unwrap();
    assert!(body.get("include").is_none(), "{body}");

    // Reasoning in the replay proves the model reasons: ask for it from now on,
    // even though this item is dropped for lacking its encrypted content.
    let reasoned = json!([
        {"type":"reasoning","id":"rs_plain","summary":[]},
        {"type":"function_call","call_id":"c1","name":"inspect","arguments":"{}"}
    ]);
    let mut continuation = req.clone();
    continuation
        .messages
        .push(assistant(&continuation, reasoned));
    continuation.messages.push(result("c1", "ok"));
    let body = crate::openai_responses::build_request_body(&continuation, false).unwrap();
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
}

#[test]
fn empty_tool_calls_in_a_text_stream_do_not_require_done() {
    let req = request("nvidia", "z-ai/glm-5.3");
    let mut state = StreamState::default();
    for (delta, reason) in [
        (
            json!({"role":"assistant","content":"All ","tool_calls":[]}),
            None,
        ),
        (json!({"content":"done.","tool_calls":[]}), Some("stop")),
    ] {
        state
            .ingest(
                &json!({"choices":[{"delta":delta,"finish_reason":reason}]}),
                false,
            )
            .unwrap();
    }
    state.end_of_stream(false);
    let parsed = state.finish(&req).unwrap();
    assert_eq!(parsed.content, "All done.");
    assert!(parsed.tool_calls.is_none());
}

#[test]
fn a_tool_calls_finish_without_done_is_not_taken_for_text() {
    let req = request("nvidia", "z-ai/glm-5.3");
    let mut state = StreamState::default();
    state
        .ingest(
            &json!({"choices":[{"delta":{"role":"assistant","content":"Calling"},"finish_reason":"tool_calls"}]}),
            false,
        )
        .unwrap();
    state.end_of_stream(false);
    // The tool call deltas never arrived: this is not a finished answer.
    assert!(state.finish(&req).is_err());
}

#[tokio::test]
async fn anthropic_requests_reach_v1_messages_from_either_base_url_form() {
    let reply = json!({"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}).to_string();
    // claude-opus-5-5 takes the native transport, claude-sonnet-4-5 the legacy one.
    for model in ["claude-opus-5-5", "claude-sonnet-4-5"] {
        for (suffix, path) in [
            ("", "/v1/messages"),
            ("/", "/v1/messages"),
            ("/v1", "/v1/messages"),
            ("/v1/", "/v1/messages"),
            // A gateway prefix is used as configured.
            ("/gateway", "/gateway/messages"),
        ] {
            for stream in [false, true] {
                let (base, server) = if stream {
                    one_shot_server("200 OK", "text/event-stream", String::new()).await
                } else {
                    one_shot_server("200 OK", "application/json", reply.clone()).await
                };
                let mut req = request("anthropic", model);
                req.api_key = Some("fixture".into());
                req.base_url = format!("{}{suffix}", base.trim_end_matches("/v1"));
                if stream {
                    let sink = Sink::default();
                    let _ = crate::ai_stream::ai_chat_stream_with_sink(&sink, req, "url").await;
                } else {
                    let _ = crate::ai::call_ai(req).await;
                }
                let (head, _) = server.await.unwrap();
                let line = head.lines().next().unwrap_or_default().to_owned();
                assert!(
                    line.starts_with(&format!("post {path} ")),
                    "{model} {suffix:?} stream={stream}: {line}"
                );
            }
        }
    }
}
