//! AI Core — Trait abstractions for decoupling AI backend from Tauri
//!
//! These traits allow the same AI tool execution and streaming logic to run
//! in both the Tauri GUI and the standalone CLI binary.

pub mod event_sink;
pub mod credential_provider;
pub mod remote_backend;
pub mod tauri_impl;
pub mod cli_impl;

pub use event_sink::{EventSink, ToolProgress};
pub use credential_provider::{CredentialProvider, ServerProfile, ServerCredentials};
pub use remote_backend::RemoteBackend;
