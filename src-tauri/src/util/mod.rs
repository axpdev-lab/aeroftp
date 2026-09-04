// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Shared lifecycle primitives, and the small predicates that more than one
//! module has to agree on.
//!
//! The lifecycle helpers are the load-bearing ones for making resource
//! ownership deterministic across the codebase. Prefer them to raw
//! `tokio::spawn`, raw `tokio::signal::ctrl_c()`, or manual `?`-propagation
//! around async cleanup.

pub mod abort_on_drop;
pub mod endpoint;
pub mod provider_guard;
pub mod shutdown;

pub use abort_on_drop::AbortOnDrop;
pub use endpoint::endpoint_stays_on_this_machine;
pub use provider_guard::ProviderGuard;
pub use shutdown::{shutdown_signal, ShutdownSignal};
