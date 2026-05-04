//! Async stdio transport for MCP JSON-RPC 2.0
//!
//! - Reads line-delimited JSON from stdin (async, non-blocking)
//! - Writes JSON-RPC responses to stdout (serialized via Mutex)
//! - Diagnostics/logs to stderr only
//! - Max line size: 1 MB

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

/// Maximum bytes per JSON-RPC line (1 MB).
const MAX_LINE_BYTES: usize = 1_048_576;

/// Async stdin reader: yields one JSON-RPC line at a time.
pub struct StdinReader {
    reader: BufReader<io::Stdin>,
}

impl Default for StdinReader {
    fn default() -> Self {
        Self::new()
    }
}

impl StdinReader {
    pub fn new() -> Self {
        Self {
            reader: BufReader::new(io::stdin()),
        }
    }

    /// Read the next line from stdin.
    /// Returns `None` on EOF (stdin closed), `Err` on line-too-long or IO error.
    pub async fn next_line(&mut self) -> Option<Result<String, TransportError>> {
        let mut line = String::new();
        match self.reader.read_line(&mut line).await {
            Ok(0) => None, // EOF
            Ok(n) if n > MAX_LINE_BYTES => Some(Err(TransportError::LineTooLong(n))),
            Ok(_) => {
                let trimmed = line.trim().to_string();
                if trimmed.is_empty() {
                    // Skip blank lines, try next
                    Some(Ok(String::new()))
                } else {
                    Some(Ok(trimmed))
                }
            }
            Err(e) => Some(Err(TransportError::Io(e.to_string()))),
        }
    }
}

/// Async stdout writer: serialized JSON-RPC output.
pub struct StdoutWriter {
    writer: Mutex<io::Stdout>,
}

impl Default for StdoutWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl StdoutWriter {
    pub fn new() -> Self {
        Self {
            writer: Mutex::new(io::stdout()),
        }
    }

    /// Write a JSON-RPC message (adds trailing newline, flushes).
    pub async fn write_message(&self, msg: &serde_json::Value) -> Result<(), TransportError> {
        let serialized =
            serde_json::to_string(msg).map_err(|e| TransportError::Serialize(e.to_string()))?;
        let mut out = self.writer.lock().await;
        out.write_all(serialized.as_bytes())
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        out.write_all(b"\n")
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        out.flush()
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        Ok(())
    }
}

/// Transport-level errors.
#[derive(Debug)]
pub enum TransportError {
    LineTooLong(usize),
    Io(String),
    Serialize(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::LineTooLong(n) => write!(f, "Line exceeds 1 MB limit ({} bytes)", n),
            TransportError::Io(e) => write!(f, "IO error: {}", e),
            TransportError::Serialize(e) => write!(f, "Serialization error: {}", e),
        }
    }
}
