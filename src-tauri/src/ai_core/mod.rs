//! AI Core — Trait abstractions for decoupling AI backend from Tauri
//!
//! These traits allow the same AI tool execution and streaming logic to run
//! in both the Tauri GUI and the standalone CLI binary.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

pub mod cli_impl;
pub mod credential_provider;
pub mod event_sink;
pub mod remote_backend;
pub mod tauri_impl;

pub use credential_provider::{CredentialProvider, ServerCredentials, ServerProfile};
pub use event_sink::{EventSink, ToolProgress};
pub use remote_backend::RemoteBackend;
