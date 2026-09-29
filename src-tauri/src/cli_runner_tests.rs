// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::*;
use ftp_client_gui_lib::ai_core::EventSink;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn config(base_url: String) -> AgentConfig {
    AgentConfig {
        provider_name: "nvidia".into(),
        provider_type: ftp_client_gui_lib::ai::AIProviderType::Nvidia,
        model: "nvidia/nemotron-3-ultra-550b-a55b".into(),
        api_key: "local-fixture".into(),
        base_url,
        system: "Scripted local fixture".into(),
        approve_level: 0,
        max_steps: 3,
        plan_only: false,
        cost_limit: None,
        usage: Arc::new(Mutex::new(AgentUsage::default())),
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Value {
    read_request_with_head(socket).await.1
}

/// The request's head (request line and headers, as sent) and its body.
async fn read_request_with_head(socket: &mut tokio::net::TcpStream) -> (String, Value) {
    let mut bytes = vec![];
    let end = loop {
        let mut buf = [0; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        assert_ne!(n, 0);
        bytes.extend_from_slice(&buf[..n]);
        if let Some(index) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break index + 4;
        }
        assert!(bytes.len() < 1_048_576);
    };
    let headers = std::str::from_utf8(&bytes[..end]).unwrap().to_string();
    let length: usize = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
    while bytes.len() < end + length {
        let mut buf = [0; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        assert_ne!(n, 0);
        bytes.extend_from_slice(&buf[..n]);
    }
    (
        headers,
        serde_json::from_slice(&bytes[end..end + length]).unwrap(),
    )
}

async fn respond(socket: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    socket.write_all(response.as_bytes()).await.unwrap();
}

#[tokio::test]
async fn cli_runner_replays_denied_tool_and_accounts_real_stream_usage() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cfg = config(format!("http://{}", listener.local_addr().unwrap()));
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            assert_eq!(request["stream"], true);
            assert!(request["tools"].as_array().unwrap().len() > 4);
            let call = json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{
                "index":0,"id":"fixture-call-A","type":"function",
                "function":{"name":"local_read","arguments":"{\"path\":\"/never-open\"}"}
            }]}}]});
            let done = json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":2}});
            respond(&mut socket, "200 OK", &format!("data: {call}\n\ndata: {done}\n\ndata: [DONE]\n\n")).await;
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            let messages = request["messages"].as_array().unwrap();
            assert_eq!(messages[2]["tool_calls"][0]["id"], "fixture-call-A");
            assert_eq!(messages[3]["tool_call_id"], "fixture-call-A");
            assert_eq!(messages[3]["content"], "Tool call denied by user.");
            let done = json!({"choices":[{"delta":{"role":"assistant","content":"Denied safely."},"finish_reason":"stop"}],"usage":{"prompt_tokens":20,"completion_tokens":3}});
            respond(&mut socket, "200 OK", &format!("data: {done}\n\ndata: [DONE]\n\n")).await;
        };
        let mut messages = vec![serde_json::from_value(json!({"role":"user","content":"fixture"})).unwrap()];
        let (result, ()) = tokio::join!(agent_tool_loop(&cfg, &mut messages, false), server);
        assert_eq!(result.unwrap(), "Denied safely.");
        assert_eq!(messages.len(), 3);
        let usage = cfg.usage.lock().unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens, usage.total_tokens), (30, 5, 35));
    }).await.unwrap();
}

/// L16 (4.2.1 review): the GUI trims an API key before a request (#912), the
/// CLI agent sent the key as the environment or the vault held it. A key
/// saved with a trailing newline worked in the app and failed in
/// `aeroftp-cli agent`: a newline cannot go in a header at all.
#[tokio::test]
async fn cli_agent_sends_the_api_key_trimmed() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = config(format!("http://{}", listener.local_addr().unwrap()));
    cfg.api_key = "  local-fixture \n".into();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (head, _) = read_request_with_head(&mut socket).await;
        let done = json!({"choices":[{"delta":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]});
        respond(
            &mut socket,
            "200 OK",
            &format!("data: {done}\n\ndata: [DONE]\n\n"),
        )
        .await;
        head
    });
    let mut messages =
        vec![serde_json::from_value(json!({"role":"user","content":"fixture"})).unwrap()];
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        agent_tool_loop(&cfg, &mut messages, false),
    )
    .await
    .expect("the agent run did not end");
    match result {
        Ok(text) => assert_eq!(text, "ok"),
        Err(e) => panic!("the request was not sent: {e}"),
    }
    let head = tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("the fixture got no request")
        .unwrap();
    assert!(
        head.to_ascii_lowercase()
            .contains("authorization: bearer local-fixture\r\n"),
        "{head}"
    );
}

#[tokio::test]
async fn cli_runner_surfaces_http_failure_without_publishing_history() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cfg = config(format!("http://{}", listener.local_addr().unwrap()));
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            respond(&mut socket, "429 Too Many Requests", "fixture rate limit").await;
        };
        let mut messages = vec![];
        let (result, ()) = tokio::join!(agent_tool_loop(&cfg, &mut messages, false), server);
        assert!(result.unwrap_err().to_string().contains("429"));
        assert!(messages.is_empty());
    })
    .await
    .unwrap();
}

#[test]
fn cli_sink_discards_post_cancel_chunks() {
    let cancel = tokio_util::sync::CancellationToken::new();
    let streamed = Arc::new(Mutex::new(String::new()));
    let sink = CollectingCliSink::new(false, cancel.clone(), streamed.clone());
    let mut chunk = ftp_client_gui_lib::ai_stream::StreamChunk {
        native_turn: None,
        content: "before".into(),
        done: false,
        tool_calls: None,
        input_tokens: None,
        output_tokens: None,
        thinking: None,
        thinking_done: None,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    };
    sink.emit_stream_chunk("fixture", &chunk);
    cancel.cancel();
    chunk.content = "late".into();
    chunk.done = true;
    sink.emit_stream_chunk("fixture", &chunk);
    assert_eq!(sink.into_response("fixture").content, "before");
    assert_eq!(*streamed.lock().unwrap(), "before");
}

#[tokio::test]
async fn cli_failed_continuation_keeps_pairing_for_next_prompt() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cfg = config(format!("http://{}", listener.local_addr().unwrap()));
        let server = async {
            for step in 0..3 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = read_request(&mut socket).await;
                if step == 0 {
                    let call = json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{
                        "index":0,"id":"effect-A","type":"function",
                        "function":{"name":"local_read","arguments":"{}"}
                    }]},"finish_reason":"tool_calls"}]});
                    respond(&mut socket, "200 OK", &format!("data: {call}\n\ndata: [DONE]\n\n")).await;
                } else if step == 1 {
                    respond(&mut socket, "503 Service Unavailable", "fixture continuation failure").await;
                } else {
                    let messages = request["messages"].as_array().unwrap();
                    assert_eq!(messages[2]["tool_calls"][0]["id"], "effect-A");
                    assert_eq!(messages[3]["tool_call_id"], "effect-A");
                    assert_eq!(messages[4]["role"], "user");
                    let done = json!({"choices":[{"delta":{"role":"assistant","content":"Recovered."},"finish_reason":"stop"}]});
                    respond(&mut socket, "200 OK", &format!("data: {done}\n\ndata: [DONE]\n\n")).await;
                }
            }
        };
        let client = async {
            let mut messages = vec![serde_json::from_value(json!({"role":"user","content":"first"})).unwrap()];
            let error = agent_tool_loop(&cfg, &mut messages, false).await.unwrap_err().to_string();
            assert!(error.contains("503"), "{error}");
            agent_failed_turn(&mut messages, 0);
            assert_eq!(messages.len(), 3);
            messages.push(serde_json::from_value(json!({"role":"user","content":"next"})).unwrap());
            assert_eq!(agent_tool_loop(&cfg, &mut messages, false).await.unwrap(), "Recovered.");
        };
        tokio::join!(client, server);
    }).await.unwrap();
}

#[test]
fn cli_history_cleanup_keeps_whole_turns() {
    let msg =
        |role: &str| serde_json::from_value(json!({"role": role, "content":"fixture"})).unwrap();
    let mut history = vec![msg("user")];
    agent_failed_turn(&mut history, 0);
    assert!(history.is_empty());
    let mut history = vec![
        msg("user"),
        msg("assistant"),
        msg("tool"),
        msg("user"),
        msg("assistant"),
    ];
    agent_failed_turn(&mut history, 3);
    assert_eq!(history.len(), 5);
    agent_trim_history(&mut history, 3);
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].role, "user");
}

#[tokio::test]
async fn cli_interrupted_run_prints_partial_text_and_exits_130() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cfg = config(format!("http://{}", listener.local_addr().unwrap()));
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            let partial = json!({"choices":[{"delta":{"role":"assistant","content":"Partial answer"},"finish_reason":null}]});
            // A close-delimited event stream that never finishes on its own.
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
            socket.write_all(format!("{head}data: {partial}\n\n").as_bytes()).await.unwrap();
            // Hold the connection until the cancelled client drops it.
            let _ = socket.read(&mut [0u8; 1]).await;
        };
        let client = async {
            let cancel = tokio_util::sync::CancellationToken::new();
            let mut messages = vec![serde_json::from_value(json!({"role":"user","content":"fixture"})).unwrap()];
            let adapter = CliRunnerAdapter { cfg: &cfg, is_tty: false, streamed: Arc::default() };
            // Interrupt once the partial text has reached the sink, not after a guess.
            let interrupt = async {
                while adapter.streamed.lock().unwrap().as_str() != "Partial answer" {
                    tokio::task::yield_now().await;
                }
                cancel.cancel();
            };
            let (outcome, ()) = tokio::join!(agent_run(&adapter, &mut messages, &cancel), interrupt);
            let mut out = Vec::new();
            let code = report_agent_oneshot(&mut out, outcome, OutputFormat::Text, true);
            assert_eq!(code, 130, "Ctrl-C is exit code 130 (Interrupted)");
            assert_eq!(String::from_utf8(out).unwrap(), "Partial answer\n");
        };
        tokio::join!(client, server);
    })
    .await
    .unwrap();
}

#[test]
fn cli_tool_results_are_capped_at_a_char_boundary() {
    // A two-byte character straddles byte 8192.
    let result = format!("{}é{}", "a".repeat(8191), "b".repeat(20_000));
    let capped = cap_tool_result(result.clone());
    assert!(capped.len() < 8300, "not capped: {} bytes", capped.len());
    assert!(capped.starts_with(&"a".repeat(8191)));
    assert!(capped.ends_with(&format!("... [truncated, {} bytes total]", result.len())));
    assert_eq!(cap_tool_result("short é".into()), "short é");
}

#[test]
fn cli_history_trim_falls_back_to_the_latest_user_turn() {
    let msg =
        |role: &str| serde_json::from_value(json!({"role": role, "content":"fixture"})).unwrap();
    // One turn with many tool steps: no user message among the last three.
    let mut history = vec![msg("user"), msg("assistant"), msg("user")];
    for _ in 0..4 {
        history.push(msg("assistant"));
        history.push(msg("tool"));
    }
    agent_trim_history(&mut history, 3);
    assert_eq!(history.len(), 9);
    assert_eq!(history[0].role, "user");
    // Already starting at the latest user message: nothing more to drop.
    agent_trim_history(&mut history, 3);
    assert_eq!(history.len(), 9);
}
