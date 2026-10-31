// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The folder a session stands in when its profile's initial folder is
//! relative.
//!
//! Connect enters a relative initial folder (`docs`) from where the session
//! starts: the root of a cloud drive, the login home on FTP and SFTP. Those
//! providers resolve a relative path against the folder they stand in, so a
//! path built on the relative folder applied it twice: `a.txt` reached
//! `docs/docs/a.txt`, and the benchmark's scratch folder `docs/docs/
//! aeroftp-bench`. A provider that keeps the folder as a prefix of its own
//! (S3's `bucket/backups/`) stays at its root, and the folder stays as given.

use crate::providers::StorageProvider;

/// Whether a session in `cwd` stands in the relative folder `base`: `cwd` is
/// `/docs`, or ends with `/docs` (the login home's `docs`). Compared folder
/// by folder, so `./docs` and `docs//sub` match the `/docs` and `/docs/sub` a
/// provider reports. A base with `..` cannot be told from here and is kept.
pub fn stands_in(cwd: &str, base: &str) -> bool {
    let folders = |path: &str| -> Vec<String> {
        path.split('/')
            .filter(|folder| !folder.is_empty() && *folder != ".")
            .map(str::to_string)
            .collect()
    };
    let (cwd, base) = (folders(cwd), folders(base));
    !base.is_empty() && !base.iter().any(|folder| folder == "..") && cwd.ends_with(&base)
}

/// The initial folder `path` as the connected session has entered it: the
/// folder the session stands in when `path` is relative and it stands there,
/// `path` as given otherwise.
pub async fn as_entered(path: &str, provider: &mut dyn StorageProvider) -> String {
    if !path.is_empty() && !path.starts_with('/') {
        if let Ok(cwd) = provider.pwd().await {
            if stands_in(&cwd, path) {
                return cwd;
            }
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::stands_in;

    /// The relative base is compared with the folder the session reports one
    /// folder at a time: `./docs` stood in `/docs` and was not seen, so it
    /// stayed relative and was applied again.
    #[test]
    fn stands_in_compares_folders_not_spellings() {
        for (cwd, base) in [
            ("/docs", "docs"),
            ("/home/u/docs/", "docs"),
            ("/docs", "./docs"),
            ("/./docs", "docs"),
            ("/home/u/docs/sub", "docs//sub/"),
            ("/docs/sub", "./docs/./sub"),
            ("/home/u/docs ", "docs "),
        ] {
            assert!(stands_in(cwd, base), "{cwd:?} {base:?}");
        }
        for (cwd, base) in [
            ("/mydocs", "docs"),
            ("/docs", "docs "),
            ("/docs", "../docs"),
            ("/docs", "."),
            ("/", ""),
        ] {
            assert!(!stands_in(cwd, base), "{cwd:?} {base:?}");
        }
    }
}
