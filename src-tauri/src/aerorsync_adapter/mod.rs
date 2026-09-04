//! Application-side adapter of the `aerorsync` module.
//!
//! Every AeroFTP type the module used to name lives here: the
//! `DeltaTransport` and `DeltaBatch` implementations, and the maps from
//! the module's crate-owned carriers onto `RsyncError`, `RsyncStats` and
//! `RsyncCapability`. The direction of the dependency is one way. This
//! adapter reads the module; the module never imports from here, and
//! that is pinned by
//! `aerorsync::tests::aerorsync_module_imports_nothing_from_the_app`,
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

//! Files: `config.rs` turns an application profile into a module
//! transport, `delta_transport.rs` and `local.rs` carry the two trait
//! implementations, `errors.rs` holds the maps onto the application
//! error, statistics and capability types.

pub mod config;
pub mod delta_transport;
pub mod errors;
pub mod local;

#[cfg(test)]
mod tests {
    /// The adapter declares what it reaches, in both directions.
    ///
    /// Toward the application: the module may not name it at all, so
    /// every application import in the whole boundary is in this file,
    /// and the list says which file names which module. Toward the
    /// module: the set of `crate::aerorsync::` submodules the adapter
    /// touches is the minimum public surface the crate will have to
    /// expose once it is extracted, so it is written out now, while it
    /// is small, and it is the starting inventory of that work.
    ///
    /// Both lists may only change in the commit that changes the
    /// adapter, and the failure message says which side moved. The
    /// adapter naming itself is neither: it is one file of this
    /// directory reaching another, and it is skipped explicitly. The
    /// scan is the module's own counter, copied rather than shared:
    /// the adapter must not depend on the module's test code. Keep the
    /// two in step.
    #[test]
    fn aerorsync_adapter_declares_its_imports() {
        /// Application modules the adapter names, file by file.
        const TOWARD_THE_APPLICATION: &[(&str, &str)] = &[
            ("config.rs", "rsync_over_ssh"),
            ("delta_transport.rs", "delta_transport"),
            ("delta_transport.rs", "rsync_over_ssh"),
            ("errors.rs", "rsync_over_ssh"),
            ("local.rs", "delta_transport"),
            ("local.rs", "rsync_over_ssh"),
        ];
        /// Module submodules the adapter reaches. This is the crate's
        /// minimum public surface after extraction.
        const TOWARD_THE_MODULE: &[&str] = &[
            "delta_transport_impl",
            "fallback_policy",
            "local_transport",
            "progress",
            "ssh_transport",
            "streaming_writer",
            "transport",
            "types",
        ];

        // Assembled so this file's own source does not trip the scan.
        let needle = ["crate", "::"].concat();
        let module_prefix = ["crate", "::aerorsync::"].concat();
        // The same refusals the module's own guard carries. They are here
        // and not shared because the adapter must not depend on the
        // module's test code; keep the two lists in step. A second
        // reviewer got a green out of this test with two of these, an
        // alias of the crate root and a `super` chain walking out of the
        // directory, which is why they are all spelled out.
        let root_alias_chain = ["super::", "super::"].concat();
        let conditional_attribute = ["cfg_", "attr"].concat();
        let escape_hatches = [
            ["#[", "path"].concat(),
            ["include", "!("].concat(),
            ["include_str", "!("].concat(),
            ["macro_rules", "!"].concat(),
        ];
        let root_aliases = [
            ["crate", " as "].concat(),
            ["extern ", "crate self"].concat(),
        ];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/aerorsync_adapter");
        let mut toward_app: Vec<(String, String)> = Vec::new();
        let mut toward_module: Vec<String> = Vec::new();
        let mut scanned = 0usize;

        for entry in std::fs::read_dir(&dir).expect("adapter directory must be readable") {
            let path = entry.expect("readable dir entry").path();
            assert!(
                !path.is_dir(),
                "{} is a subdirectory the flat scan cannot see; keep adapter sources in \
                 src/aerorsync_adapter/ itself",
                path.display()
            );
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
            for hatch in &escape_hatches {
                assert!(
                    !src.contains(hatch.as_str()),
                    "{name} uses `{hatch}`: the scan reads one flat directory and cannot see \
                     through it"
                );
            }
            // Line-based and comment-skipping, unlike the hatches above:
            // a comment that explains the refusal must not trip it.
            for (index, line) in src.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") || trimmed.starts_with('*') {
                    continue;
                }
                for alias in &root_aliases {
                    assert!(
                        !line.contains(alias.as_str()),
                        "{name}:{}: renames the crate root with `{alias}`: an alias reaches \
                         the application without ever spelling the prefix this scan reads",
                        index + 1
                    );
                }
            }
            for (index, line) in src.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") || trimmed.starts_with('*') {
                    continue;
                }
                let lineno = index + 1;
                assert!(
                    !line.contains(root_alias_chain.as_str()),
                    "{name}:{lineno}: `{root_alias_chain}` walks out of this directory to the \
                     crate root; name what you reach"
                );
                assert!(
                    !(line.contains(conditional_attribute.as_str()) && line.contains("path")),
                    "{name}:{lineno}: `{conditional_attribute}` with a `path` key redirects a \
                     module to a file the flat scan cannot see"
                );
                let mut rest = line;
                while let Some(at) = rest.find(needle.as_str()) {
                    let after = &rest[at + needle.len()..];
                    assert!(
                        !after.starts_with('{'),
                        "{name}:{lineno}: split the grouped import; `use {needle}{{..}}` hides \
                         which side of the boundary a path is on"
                    );
                    let ident: String = after
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    if ident == "aerorsync" {
                        let tail = &rest[at..];
                        assert!(
                            tail.starts_with(module_prefix.as_str()),
                            "{name}:{lineno}: name a submodule, not the module root"
                        );
                        let sub: String = tail[module_prefix.len()..]
                            .chars()
                            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                            .collect();
                        if !toward_module.contains(&sub) {
                            toward_module.push(sub);
                        }
                    } else if ident == "aerorsync_adapter" {
                        // The adapter naming itself: one file reaching
                        // another inside this directory is not a step
                        // across the boundary, and the scan says so out
                        // loud rather than folding it into either list.
                    } else if !ident.is_empty() {
                        let hit = (name.clone(), ident.clone());
                        if !toward_app.contains(&hit) {
                            toward_app.push(hit);
                        }
                    }
                    rest = &after[ident.len()..];
                }
            }
        }

        assert!(
            scanned >= 4,
            "the scan saw only {scanned} .rs files: is this still the adapter directory?"
        );

        toward_app.sort();
        let expected_app: Vec<(String, String)> = TOWARD_THE_APPLICATION
            .iter()
            .map(|(f, m)| ((*f).to_string(), (*m).to_string()))
            .collect();
        assert_eq!(
            toward_app, expected_app,
            "the adapter's application imports moved; declare them in the same commit"
        );

        toward_module.sort();
        let expected_module: Vec<String> =
            TOWARD_THE_MODULE.iter().map(|m| (*m).to_string()).collect();
        assert_eq!(
            toward_module, expected_module,
            "the adapter reaches a different set of module submodules; this list is the \
             crate's minimum public surface, so declare the change in the same commit"
        );
    }
}
