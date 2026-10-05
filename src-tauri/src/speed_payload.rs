// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The name of the files a speed test or a benchmark writes on a remote, one
//! rule for every path that writes them: the GUI speed test, the CLI
//! `benchmark`, `speed` and `speed-compare`, and AeroAgent's speed tool.
//!
//! The extension is `.dat`, never `.bin`. Koofr refuses to serve any `.bin`
//! file ("403 FileBlocked: File download restricted due to possible dangerous
//! content"), whatever it holds, a file of zeros included, while the same
//! bytes named `.dat` come back intact (measured 2026-10-05). With `.bin`
//! every download measurement on Koofr failed (#368).

/// Extension of every speed-test and benchmark payload.
pub const EXTENSION: &str = ".dat";

/// `stem` with the payload extension.
pub fn name(stem: &str) -> String {
    format!("{stem}{EXTENSION}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_is_never_named_bin() {
        let name = name(".aeroftp-speedtest-x");
        assert_eq!(name, ".aeroftp-speedtest-x.dat");
        assert!(!name.ends_with(".bin"));
    }
}
