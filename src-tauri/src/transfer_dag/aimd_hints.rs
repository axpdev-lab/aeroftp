// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use crate::providers::ProviderType;

/// Per-provider tuning overlay for the AIMD adaptive controller.
///
/// All fields are optional so the controller can preserve its built-in legacy
/// behaviour when no CLI overlay is installed for the current provider.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AimdHint {
    /// Number of API dispatches allowed inside one pacing window before the
    /// next dispatch is delayed by `min_sleep`.
    pub burst: Option<usize>,
    /// Minimum spacing between API dispatch windows for the provider.
    pub min_sleep: Option<Duration>,
    /// Static Retry-After fallback used when the provider raised congestion but
    /// the failing request did not carry its own server-side cooldown hint.
    pub retry_after_secs: Option<u64>,
}

static AIMD_HINTS: OnceLock<HashMap<ProviderType, AimdHint>> = OnceLock::new();

/// Install the global per-provider hint table once at CLI boot.
///
/// Subsequent calls are ignored so every downstream transfer surface observes
/// a stable snapshot for the whole process lifetime.
pub fn init(hints: HashMap<ProviderType, AimdHint>) {
    let _ = AIMD_HINTS.set(hints);
}

/// Lookup the hint overlay for a specific provider.
///
/// Returns a default, all-`None` hint when the table is unset or the provider
/// has no override entry.
pub fn for_provider(provider: ProviderType) -> AimdHint {
    AIMD_HINTS
        .get()
        .and_then(|table| table.get(&provider).cloned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `AIMD_HINTS` is a process-global `OnceLock` so init-vs-lookup
    /// invariants cannot be tested in isolation: another test in the
    /// suite (e.g. `adaptive::tests::install_google_drive_test_hint`) may
    /// have populated the table before this test runs, or this test may
    /// run alone in `--test-name` filtering mode. We therefore restrict
    /// the assertions to a property that holds regardless of test
    /// ordering: any provider type that no other test installs must
    /// return the all-`None` default `AimdHint`. `Cloudinary` is unused
    /// in fixtures elsewhere in this crate, so it makes a stable probe.
    ///
    /// Idempotency of `init` is guaranteed structurally by the `OnceLock`
    /// API (`set` returns `Err` on a populated cell, which we discard
    /// with `let _`) and is implicitly re-verified by every other test
    /// that calls `init` and observes its fixture later in the run.
    #[test]
    fn for_provider_returns_default_when_provider_has_no_overlay() {
        assert_eq!(for_provider(ProviderType::Cloudinary), AimdHint::default());
    }
}