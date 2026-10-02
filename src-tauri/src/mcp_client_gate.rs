//! Request validation and error codes for outbound MCP calls.
//! Approval, live-state rechecks and audit belong to `mcp_client_bridge`.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::io::{self, Write};

use serde_json::Value;

const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_TOOL_NAME_BYTES: usize = 128;
const MAX_SESSION_ID_BYTES: usize = 128;

#[derive(Default)]
struct BoundedArguments(Vec<u8>);

impl Write for BoundedArguments {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_ARGUMENT_BYTES {
            return Err(io::Error::other("MCP arguments too large"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateError {
    InvalidRequest,
    UserUnavailable,
    ConfigUnavailable,
    ConfigDisabled,
    SecretUnavailable,
    StaleRevision,
    ApprovalRequired,
}

impl GateError {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "MCP_CALL_INVALID_REQUEST",
            Self::UserUnavailable => "MCP_USER_UNAVAILABLE",
            Self::ConfigUnavailable => "MCP_CONFIG_UNAVAILABLE",
            Self::ConfigDisabled => "MCP_CONFIG_DISABLED",
            Self::SecretUnavailable => "MCP_SECRET_UNAVAILABLE",
            Self::StaleRevision => "MCP_CONFIG_STALE_REVISION",
            Self::ApprovalRequired => "MCP_APPROVAL_REQUIRED",
        }
    }
}

/// Owned request fields are kept out of Debug and audit output.
pub(crate) struct GateRequest {
    pub server_id: String,
    pub tool_name: String,
    pub arguments: Value,
    pub expected_revision: String,
    pub session_id: String,
    pub approval_grant_id: Option<String>,
}

fn unsafe_display_char(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}')
}

impl GateRequest {
    pub(crate) fn validate(&self) -> Result<Vec<u8>, GateError> {
        let server = &self.server_id;
        let valid_server = !server.is_empty()
            && server.len() <= 64
            && server.as_bytes()[0].is_ascii_alphanumeric()
            && server
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !valid_server
            || self.tool_name.is_empty()
            || self.tool_name.len() > MAX_TOOL_NAME_BYTES
            || self.tool_name.chars().any(unsafe_display_char)
            || self.session_id.is_empty()
            || self.session_id.len() > MAX_SESSION_ID_BYTES
            || self.session_id.chars().any(char::is_control)
            || self.expected_revision.len() != 64
            || !self
                .expected_revision
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
            || !self.arguments.is_object()
        {
            return Err(GateError::InvalidRequest);
        }
        let mut args = BoundedArguments::default();
        serde_json::to_writer(&mut args, &self.arguments).map_err(|_| GateError::InvalidRequest)?;
        Ok(args.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request() -> GateRequest {
        GateRequest {
            server_id: "example".into(),
            tool_name: "echo".into(),
            arguments: json!({"text":"private-argument"}),
            expected_revision: "a".repeat(64),
            session_id: "chat-a".into(),
            approval_grant_id: None,
        }
    }

    #[test]
    fn valid_request_serializes_its_arguments() {
        assert_eq!(
            request().validate().unwrap(),
            br#"{"text":"private-argument"}"#
        );
    }

    #[test]
    fn malformed_identifiers_revision_and_arguments_are_rejected() {
        let cases: [fn(&mut GateRequest); 13] = [
            |r| r.server_id = String::new(),
            |r| r.server_id = "-leading".into(),
            |r| r.server_id = "private\nserver".into(),
            |r| r.server_id = "s".repeat(65),
            |r| r.tool_name = String::new(),
            |r| r.tool_name = "private\ntool".into(),
            |r| r.tool_name = "t".repeat(MAX_TOOL_NAME_BYTES + 1),
            |r| r.session_id = String::new(),
            |r| r.session_id = "chat\u{7}".into(),
            |r| r.session_id = "c".repeat(MAX_SESSION_ID_BYTES + 1),
            |r| r.expected_revision = "a".repeat(63),
            |r| r.expected_revision = "g".repeat(64),
            |r| r.arguments = json!(["not", "an", "object"]),
        ];
        for (index, mutate) in cases.iter().enumerate() {
            let mut changed = request();
            mutate(&mut changed);
            assert_eq!(
                changed.validate(),
                Err(GateError::InvalidRequest),
                "case {index}"
            );
        }
    }

    #[test]
    fn invisible_tool_names_are_rejected() {
        for character in [
            '\u{061C}', '\u{200B}', '\u{200F}', '\u{202E}', '\u{2066}', '\u{2069}', '\u{FEFF}',
        ] {
            let mut changed = request();
            changed.tool_name = format!("safe{character}tool");
            assert_eq!(changed.validate(), Err(GateError::InvalidRequest));
        }
    }

    #[test]
    fn arguments_over_the_byte_limit_are_rejected() {
        let mut changed = request();
        changed.arguments = json!({"data": "x".repeat(MAX_ARGUMENT_BYTES - 16)});
        assert!(changed.validate().is_ok());
        changed.arguments = json!({"data": "x".repeat(MAX_ARGUMENT_BYTES)});
        assert_eq!(changed.validate(), Err(GateError::InvalidRequest));
    }
}
