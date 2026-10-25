// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Destination identity for GUI and CLI recursive downloads. Comparison keys
//! never change the exact remote identity or the path passed to a writer.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

pub struct LocalTargetClaim {
    pub remote_path: String,
    pub local_path: PathBuf,
}

/// Independent filesystem equivalences. Case-sensitive APFS still folds
/// canonical Unicode spellings; Linux filesystems need not fold either.
#[derive(Clone, Copy)]
pub struct DestinationRules {
    pub fold_case: bool,
    pub fold_normalization: bool,
}

#[derive(Debug)]
pub struct LocalTargetConflict {
    pub local_path: PathBuf,
    first_remote: String,
    second_remote: String,
}

impl std::fmt::Display for LocalTargetConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Ambiguous local target '{}': remote entries '{}' and '{}' both map to it",
            self.local_path.display(),
            self.first_remote,
            self.second_remote
        )
    }
}

pub struct LocalTargetGuard {
    rules: DestinationRules,
    claimed: HashMap<String, String>,
}

impl LocalTargetGuard {
    pub fn new(destination: &Path) -> Self {
        Self::with_rules(destination_rules(destination))
    }

    pub fn with_rules(rules: DestinationRules) -> Self {
        Self {
            rules,
            claimed: HashMap::new(),
        }
    }

    fn key(&self, path: &Path) -> String {
        let mut key = path.to_string_lossy().into_owned();
        if self.rules.fold_case {
            key = case_fold_key(&key);
        }
        if self.rules.fold_normalization {
            key = key.nfc().collect();
        }
        key
    }

    /// Validate a whole listing before publishing any claim. Repeated listings
    /// of the same exact source are harmless, but siblings claiming one target
    /// are refused even when the server repeated their remote spelling.
    pub fn claim_listing(
        &mut self,
        claims: &[LocalTargetClaim],
    ) -> Result<(), LocalTargetConflict> {
        let mut listing = HashMap::new();
        for claim in claims {
            let key = self.key(&claim.local_path);
            if let Some(first) = listing.insert(key.clone(), claim.remote_path.clone()) {
                return Err(ambiguous_local_target(
                    &first,
                    &claim.remote_path,
                    &claim.local_path,
                ));
            }
            if let Some(first) = self.claimed.get(&key) {
                if crate::sync_core::walk_path_key(first)
                    != crate::sync_core::walk_path_key(&claim.remote_path)
                {
                    return Err(ambiguous_local_target(
                        first,
                        &claim.remote_path,
                        &claim.local_path,
                    ));
                }
            }
        }
        self.claimed.extend(listing);
        Ok(())
    }
}

/// Case key for a case-insensitive volume, one character at a time: the
/// uppercase of each character, then the lowercase of each character of that
/// (full mappings, so `ß` becomes `ss`). NTFS compares through an uppercase
/// table and APFS through Unicode case folding, so `ς`, `σ` and `Σ` are one
/// name on both, and APFS also folds the Kelvin sign to `k` and `ß` to `ss`.
/// `str::to_lowercase` is not a stable key: it applies the final-sigma rule
/// and keeps `ς` apart from `σ`. Where the volumes differ (`ß` and `ss`, or
/// dotless `ı`, are distinct names on NTFS) the key merges: a needless
/// refusal is safer than two files landing on one name.
fn case_fold_key(name: &str) -> String {
    name.chars()
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .collect()
}

fn ambiguous_local_target(first: &str, second: &str, local: &Path) -> LocalTargetConflict {
    LocalTargetConflict {
        local_path: local.to_path_buf(),
        first_remote: first.into(),
        second_remote: second.into(),
    }
}

fn destination_rules(destination: &Path) -> DestinationRules {
    // A new nested destination inherits the closest existing parent's volume.
    let parent = destination
        .ancestors()
        .find(|path| path.is_dir())
        .unwrap_or(Path::new("."));
    // Each attempt owns a private, randomly named namespace. No pre-existing
    // file can be removed, including the uppercase PID/counter probe spelling
    // used by older versions. TempDir cleans up only this created namespace.
    let probed = tempfile::Builder::new()
        .prefix(".aeroftp-name-probe-")
        .tempdir_in(parent)
        .ok()
        .and_then(|dir| {
            let case = probe_alias(dir.path(), "case", "CASE").ok()?;
            let normalization = probe_alias(dir.path(), "café", "cafe\u{301}").ok()?;
            Some(DestinationRules {
                fold_case: case,
                fold_normalization: normalization,
            })
        });
    // When probing is unavailable, refuse possible collisions conservatively.
    #[allow(unused_mut)]
    let mut rules = probed.unwrap_or(DestinationRules {
        fold_case: true,
        fold_normalization: true,
    });
    #[cfg(test)]
    {
        if let Ok(value) = std::env::var("AEROFTP_TEST_LOCAL_CASE_FOLD") {
            if value == "1" || value == "0" {
                rules.fold_case = value == "1";
            }
        }
        if let Ok(value) = std::env::var("AEROFTP_TEST_LOCAL_NORMALIZATION_FOLD") {
            if value == "1" || value == "0" {
                rules.fold_normalization = value == "1";
            }
        }
    }
    rules
}

fn probe_alias(dir: &Path, first: &str, second: &str) -> std::io::Result<bool> {
    let original = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(first))?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(second))
    {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // The counterpart must be the file we opened, rather than an
            // unrelated occupant. A collision fails the probe, never deletes it.
            let same = same_file::Handle::from_file(original)?
                == same_file::Handle::from_path(dir.join(second))?;
            if same {
                Ok(true)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "probe collision",
                ))
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn claim(local: &str, remote: &str) -> LocalTargetClaim {
        LocalTargetClaim {
            local_path: PathBuf::from(local),
            remote_path: remote.into(),
        }
    }

    #[test]
    fn audit425_normalization_is_independent_of_case_and_io_keeps_spelling() {
        let claims = [
            claim("/tmp/café", "/root/café"),
            claim("/tmp/cafe\u{301}", "/root/cafe\u{301}"),
        ];
        let mut guard = LocalTargetGuard::with_rules(DestinationRules {
            fold_case: false,
            fold_normalization: true,
        });
        assert!(guard.claim_listing(&claims).is_err());
        assert_eq!(claims[1].remote_path, "/root/cafe\u{301}");
        assert_eq!(claims[1].local_path, PathBuf::from("/tmp/cafe\u{301}"));
        // A failed listing commits no claims.
        guard.claim_listing(&claims[..1]).unwrap();
    }

    #[test]
    fn audit425_case_key_merges_greek_sigmas_kelvin_and_sharp_s() {
        let insensitive = || {
            LocalTargetGuard::with_rules(DestinationRules {
                fold_case: true,
                fold_normalization: false,
            })
        };
        for (first, second) in [
            ("ς", "σ"),
            ("ΟΔΟΣ", "οδος"),
            ("οδοσ", "οδος"),
            ("\u{212A}", "k"),
            ("ß", "ss"),
            ("STRASSE", "straße"),
        ] {
            assert!(
                insensitive()
                    .claim_listing(&[
                        claim(first, &format!("/{first}")),
                        claim(second, &format!("/{second}")),
                    ])
                    .is_err(),
                "{first} and {second} are one name on a case-insensitive volume"
            );
        }
        insensitive()
            .claim_listing(&[claim("i", "/i"), claim("İ", "/İ")])
            .unwrap();
    }

    #[test]
    fn audit425_sensitive_filesystem_retains_distinct_unicode_and_case() {
        let mut guard = LocalTargetGuard::with_rules(DestinationRules {
            fold_case: false,
            fold_normalization: false,
        });
        guard
            .claim_listing(&[
                claim("café", "/café"),
                claim("cafe\u{301}", "/cafe\u{301}"),
                claim("A", "/A"),
                claim("a", "/a"),
            ])
            .unwrap();
    }

    #[test]
    fn audit425_case_and_unicode_aliases_are_refused_across_listings() {
        let mut guard = LocalTargetGuard::with_rules(DestinationRules {
            fold_case: true,
            fold_normalization: true,
        });
        guard
            .claim_listing(&[claim("CAFÉ", "/first/CAFÉ")])
            .unwrap();
        assert!(guard
            .claim_listing(&[claim("cafe\u{301}", "/second/cafe\u{301}")])
            .is_err());
    }

    #[test]
    fn audit425_case_probe_preserves_preexisting_uppercase_file() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!(".AEROFTP-CASE-PROBE-{}-0", std::process::id());
        let victim = dir.path().join(name);
        std::fs::write(&victim, b"keep me").unwrap();
        let _ = LocalTargetGuard::new(dir.path());
        assert_eq!(std::fs::read(victim).unwrap(), b"keep me");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn audit425_probe_collision_leaves_both_occupants_untouched() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CASE"), b"original").unwrap();
        // On a case-sensitive volume this creates "case" and detects the
        // unrelated counterpart. On an insensitive volume create_new refuses.
        let _ = probe_alias(dir.path(), "case", "CASE");
        assert_eq!(std::fs::read(dir.path().join("CASE")).unwrap(), b"original");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn audit425_macos_destination_folds_normalization() {
        let dir = tempfile::tempdir().unwrap();
        assert!(destination_rules(dir.path()).fold_normalization);
    }
}
