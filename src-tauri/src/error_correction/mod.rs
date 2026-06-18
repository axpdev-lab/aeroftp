// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Error Correction: thin re-export of the converged engine in the `aerovault` crate.
//!
//! M7 convergence (2026-06-18): the app no longer keeps a forked `.aerocorrect` / AVEC /
//! Reed-Solomon implementation. The standalone `correct` command, the `aeroftp_correct_*`
//! MCP tools, and the AeroSync download error correction now all route into
//! [`aerovault::error_correction`] (the single, audited implementation; the produced sidecar
//! bytes are byte-identical to what the app emitted before, pinned by the crate's cross-impl
//! golden T5). This module carries **no error-correction logic of its own**: it only
//! re-exports the crate's public API so existing `crate::error_correction::...` call sites
//! (CLI `correct`, MCP `aeroftp_correct_*`, the AeroSync sync pipeline, the `sync_ec_*` Tauri
//! commands) resolve unchanged.
//!
//! Not in scope here: the AeroCrypt overlay (`crate::aerocrypt`) is a separate codec/product
//! and is untouched by this convergence.

pub use aerovault::error_correction::{
    ERROR_CORRECTION_DEFAULT_PCT, ERROR_CORRECTION_MAX_PCT, ERROR_CORRECTION_MIN_PCT,
};
pub use aerovault::{
    correct_generate, correct_repair, correct_repair_anchored, correct_verify,
    CorrectGenerateReport, CorrectRepairReport, CorrectVerifyReport,
};

/// AeroSync windowed-sidecar error-correction API, re-exported from the crate's
/// `error_correction::sync` module. The `aerosync` path is preserved so existing
/// `crate::error_correction::aerosync::*` call sites resolve without change after the
/// convergence (windowed `.aerocorrect` generation with the cap + minimum-benefit gate, and
/// verify/repair against an out-of-band expected SHA-256).
pub mod aerosync {
    pub use aerovault::error_correction::sync::*;
}
