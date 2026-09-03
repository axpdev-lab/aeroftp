//! Application-side adapter of the `aerorsync` module.
//!
//! Every AeroFTP type the module used to name lives here: the
//! `DeltaTransport` and `DeltaBatch` implementations, and the maps from
//! the module's crate-owned carriers onto `RsyncError`, `RsyncStats` and
//! `RsyncCapability`. The direction of the dependency is one way. This
//! adapter reads the module; the module never imports from here, and
//! that is pinned by
//! `aerorsync::tests::app_import_budget_matches_the_documented_inventory`,
//! whose counter compares `aerorsync` as a whole identifier so
//! `crate::aerorsync_adapter` inside the module would be a violation.
//!
//! The envelope strings the maps build (`native fallback ({Kind:?}):
//! {detail}` and `native hard rejection ({Kind:?}): {detail}`) are a
//! product contract, not an implementation detail: `delta_sync_rsync`
//! re-reads them to choose between a transparent classic fallback and a
//! surfaced hard error. They are built in exactly one place, here.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

pub mod config;
pub mod delta_transport;
pub mod errors;

#[cfg(test)]
mod tests {
    /// The adapter may only reach the module through the surface the
    /// module publishes on purpose. A new entry in this list is a
    /// deliberate widening and belongs in the same commit that needs it.
    #[test]
    fn aerorsync_adapter_reaches_only_the_module_public_surface() {
        const ALLOWED: &[&str] = &[
            "delta_transport_impl",
            "events",
            "fallback_policy",
            "progress",
            // `config.rs` builds an `SshTransportConfig` out of an
            // application profile, so it names the module that owns it.
            "ssh_transport",
            "streaming_writer",
            "transport",
            "types",
        ];
        // Assembled so this file's own source does not trip the scan.
        let needle = ["crate", "::aerorsync::"].concat();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/aerorsync_adapter");
        let mut scanned = 0usize;
        let mut checked = 0usize;
        for entry in std::fs::read_dir(&dir).expect("adapter directory must be readable") {
            let path = entry.expect("readable dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            scanned += 1;
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .expect("utf-8 file name")
                .to_string();
            let src = std::fs::read_to_string(&path).expect("readable source file");
            for (index, line) in src.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") || trimmed.starts_with('*') {
                    continue;
                }
                let mut rest = line;
                while let Some(at) = rest.find(needle.as_str()) {
                    let after = &rest[at + needle.len()..];
                    let module: String = after
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    assert!(
                        ALLOWED.contains(&module.as_str()),
                        "{name}:{}: the adapter reaches `{module}`, which is not part of the \
                         module surface it is allowed to see",
                        index + 1
                    );
                    checked += 1;
                    rest = &after[module.len()..];
                }
            }
        }
        assert!(
            scanned >= 3,
            "the scan saw only {scanned} .rs files: is this still the adapter directory?"
        );
        assert!(
            checked > 0,
            "the scan matched no module path at all: the needle no longer matches the code"
        );
    }
}
