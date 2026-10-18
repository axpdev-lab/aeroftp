//! `aeroftp-cli mcp` answers every request but `initialize` on a tokio worker
//! thread. In a debug build the request's future, copied by value through each
//! layer that wraps it, and the frame that builds the tool table needed more
//! than a worker's 2 MiB stack: the server aborted on its first `tools/list`
//! ("thread 'tokio-rt-worker' has overflowed its stack") and the MCP client
//! only saw the connection close. Here the workers get half that stack
//! (`RUST_MIN_STACK`, which tokio's threads inherit), so the margin is pinned
//! too, and the server must answer and exit cleanly.

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Half the stack a tokio worker gets by default.
const WORKER_STACK: usize = 1024 * 1024;

#[test]
fn the_mcp_server_answers_on_half_a_worker_stack() {
    let config = isolated_cli::root();
    let mut child = isolated_cli::command(
        std::path::Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")),
        config.path(),
    )
    .arg("mcp")
    // Private portable state on every OS, with no keyring access.
    .env("AEROFTP_MASTER_PASSWORD", "mcp-worker-stack-fixture")
    .env("RUST_MIN_STACK", WORKER_STACK.to_string())
    .env_remove("RUST_LOG")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn aeroftp-cli mcp");
    // Drained from the start, so a chatty server never blocks on a full pipe.
    let stderr_pipe = child.stderr.take().expect("stderr");
    let stderr = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut BufReader::new(stderr_pipe), &mut text);
        text
    });

    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "worker-stack-test", "version": "1"}
        }}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "aeroftp_mcp_info", "arguments": {}
        }}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}),
        json!({"jsonrpc": "2.0", "id": 5, "method": "prompts/list"}),
    ];
    let mut stdin = child.stdin.take().expect("stdin");
    for request in &requests {
        writeln!(stdin, "{request}").expect("write a request");
    }
    stdin.flush().expect("flush requests");

    // Answers come back in any order: the server runs requests concurrently.
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(message) = serde_json::from_str::<Value>(&line) {
                if tx.send(message).is_err() {
                    break;
                }
            }
        }
    });
    let mut answers: HashMap<u64, Value> = HashMap::new();
    let deadline = Instant::now() + Duration::from_secs(120);
    while answers.len() < 5 {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(message) = rx.recv_timeout(left) else {
            break;
        };
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            answers.insert(id, message);
        }
    }

    // Closing stdin ends the server once it has drained its requests.
    drop(stdin);
    let exit_deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll aeroftp-cli mcp") {
            break Some(status);
        }
        if Instant::now() >= exit_deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stderr = stderr.join().unwrap_or_default();

    let mut ids: Vec<_> = answers.keys().copied().collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        [1, 2, 3, 4, 5],
        "every request answered; exit {status:?}, stderr:\n{stderr}"
    );
    assert!(
        status.is_some_and(|status| status.success()),
        "clean exit after stdin closed, got {status:?}; stderr:\n{stderr}"
    );
    for (id, answer) in &answers {
        assert!(answer.get("error").is_none(), "request {id}: {answer}");
    }
    let tools = answers[&2]["result"]["tools"]
        .as_array()
        .expect("tools/list result");
    assert!(
        tools.iter().any(|tool| tool["name"] == "aeroftp_mcp_info"),
        "tools/list names the tool called next"
    );
    assert_ne!(
        answers[&3]["result"]["isError"],
        json!(true),
        "{}",
        answers[&3]
    );
}
