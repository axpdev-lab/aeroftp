// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Whether a remote path a user wrote or saved may be used as a location:
//! joined to a root, stripped of a prefix, or compared with a folder
//! boundary.
//!
//! Such a path is used as written. A `..` step names the folder above, which
//! only the server knows through a symlink, so `/docs/../private` starts
//! with `/docs` and names a folder outside it. A `.` step and an empty step
//! (a doubled slash) name the same folder as the plain path on a POSIX
//! server, but a comparison by text reads them as another one, and a prefix
//! stripped before an empty step left an absolute path (`~//docs` became
//! `/docs`, at the server root). Each of them is refused, never collapsed
//! into the folder it might have meant, and the message names the path as
//! written.
//!
//! The rule, the same in `src/utils/syncNavigationPath.ts`
//! (`locationProblem`), with the cases both sides pass in
//! `src/shared/path-location-cases.json`:
//! - `/` and `\` both end a step;
//! - a `.` or a `..` step is refused;
//! - an empty step is refused, except the one before a leading separator
//!   (an absolute path), the second one of a leading `\\` (a Windows UNC
//!   path, `\\nas\share`), and the one after a single trailing separator
//!   (`/docs/` is `/docs`); `/` is the root;
//! - a NUL character is refused;
//! - the empty path is not refused here: each site says what it means.

/// Why a path cannot be used as a location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationProblem {
    /// A `..` step.
    ParentStep,
    /// A `.` step.
    CurrentStep,
    /// An empty step: a doubled separator.
    EmptyStep,
    /// A NUL character.
    Nul,
}

impl LocationProblem {
    /// The case name in `path-location-cases.json`.
    pub fn name(self) -> &'static str {
        match self {
            Self::ParentStep => "parent",
            Self::CurrentStep => "current",
            Self::EmptyStep => "empty",
            Self::Nul => "nul",
        }
    }

    fn reason(self) -> &'static str {
        match self {
            Self::ParentStep => "a '..' step",
            Self::CurrentStep => "a '.' step",
            Self::EmptyStep => "an empty step (a doubled slash)",
            Self::Nul => "a NUL character",
        }
    }
}

/// The first reason `path` cannot be used as a location, or `None`.
pub fn location_problem(path: &str) -> Option<LocationProblem> {
    if path.contains('\0') {
        return Some(LocationProblem::Nul);
    }
    let steps: Vec<&str> = path.split(['/', '\\']).collect();
    let last = steps.len() - 1;
    for (index, step) in steps.iter().enumerate() {
        match *step {
            ".." => return Some(LocationProblem::ParentStep),
            "." => return Some(LocationProblem::CurrentStep),
            // The empty path, the step before a leading separator, and the
            // one after a single trailing separator name no folder of their
            // own; `/` is both.
            "" if index == 0 || (index == last && !steps[index - 1].is_empty()) => {}
            "" if index == last && index == 1 => {}
            "" if index == 1 && path.starts_with("\\\\") => {}
            "" => return Some(LocationProblem::EmptyStep),
            _ => {}
        }
    }
    None
}

/// `Err` naming `path` as written, with `what` it is (`"remote folder"`),
/// when it cannot be used as a location.
pub fn refuse_an_unusable_location(what: &str, path: &str) -> Result<(), String> {
    match location_problem(path) {
        None => Ok(()),
        Some(problem) => Err(format!(
            "The {what} '{}' contains {}: write it as a plain path, without '.' or '..' steps \
             or doubled slashes",
            path.replace('\0', "\\0"),
            problem.reason()
        )),
    }
}

/// What follows the login folder in `path` when `path` is `~` or `~/...`:
/// every separator after the `~` goes, so the rest is relative to the login
/// folder and never a folder at the server root. `None` for any other path.
pub fn login_relative_rest(path: &str) -> Option<&str> {
    let rest = path.strip_prefix('~')?;
    if !rest.is_empty() && !rest.starts_with('/') {
        return None;
    }
    Some(rest.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cases of `src/shared/path-location-cases.json`, which the
    /// frontend's `locationProblem` passes too: both sides refuse the same
    /// paths for the same reason.
    #[test]
    fn a_location_is_refused_as_the_frontend_refuses_it() {
        let table: serde_json::Value =
            serde_json::from_str(include_str!("../../../src/shared/path-location-cases.json"))
                .unwrap();
        let cases = table["cases"].as_array().unwrap();
        assert!(cases.len() >= 20);
        for case in cases {
            let path = case["path"].as_str().unwrap();
            let expected = case["problem"].as_str();
            assert_eq!(
                location_problem(path).map(LocationProblem::name),
                expected,
                "{case}"
            );
            let refused = refuse_an_unusable_location("remote folder", path);
            assert_eq!(refused.is_err(), expected.is_some(), "{case}");
            if let Err(message) = refused {
                assert!(message.contains(&path.replace('\0', "\\0")), "{message}");
            }
        }
    }

    /// The production source of `file`: a module's own tests are left out.
    fn production(file: &'static str) -> &'static str {
        file.find("\npub(crate) mod tests {")
            .or_else(|| file.find("\nmod tests {"))
            .map_or(file, |end| &file[..end])
    }

    /// Every function of the AeroCloud code that strips, compares or joins a
    /// saved remote folder or path, or strips the `~` of a login-relative
    /// one, checks it here first, or is listed with the reason it needs no
    /// check. A new `strip_prefix`, `starts_with` or `trim_*_matches` on such
    /// a path fails here until it does one or the other. The property is
    /// pinned by the case table and by each site's own test.
    #[test]
    fn every_location_site_checks_the_path_first() {
        const FILES: &[(&str, &str)] = &[
            ("lib.rs", include_str!("../lib.rs")),
            ("cloud_service.rs", include_str!("../cloud_service.rs")),
            ("cloud_config.rs", include_str!("../cloud_config.rs")),
            ("cloud_pairs.rs", include_str!("../cloud_pairs.rs")),
            (
                "cloud_provider_factory.rs",
                include_str!("../cloud_provider_factory.rs"),
            ),
            ("providers/ftp.rs", include_str!("../providers/ftp.rs")),
            (
                "crypt_overlay_provider.rs",
                include_str!("../crypt_overlay_provider.rs"),
            ),
        ];
        const OPS: &[&str] = &[
            "remote_path.strip_prefix(",
            "remote_path.starts_with(",
            "remote_path.trim_start_matches(",
            "remote_path.trim_end_matches(",
            "remote_folder.strip_prefix(",
            "remote_folder.starts_with(",
            "remote_folder.trim_start_matches(",
            "remote_folder.trim_end_matches(",
            "strip_prefix(\"~",
            "strip_prefix('~'",
        ];
        const CHECKS: &[&str] = &[
            "refuse_an_unusable_location(",
            "refuse_an_unusable_remote_folder(",
            "location_problem(",
            "login_relative_rest(",
        ];
        // (file, function, why it needs no check of its own)
        const EXEMPT: &[(&str, &str, &str)] = &[
            // The cycle composes its folder through
            // `place_login_relative_remote_folder`, which refuses one that is
            // not plain before any of these runs on it.
            (
                "cloud_service.rs",
                "recheck_update_transfer",
                "the composed folder",
            ),
            (
                "cloud_service.rs",
                "process_comparison_with_provider",
                "the composed folder",
            ),
            (
                "cloud_service.rs",
                "ensure_remote_folder_within_overlay",
                "the composed folder",
            ),
            // The absolute-path rule is a check, not a strip; the function
            // refuses the folder itself (asserted below).
            (
                "cloud_config.rs",
                "validate_config",
                "refuses the folder itself",
            ),
            // A path a file operation is given at run time, not a location.
            (
                "providers/ftp.rs",
                "absolute_remote_path",
                "a file operation's path",
            ),
        ];
        let mut checked = Vec::new();
        for (file, source) in FILES {
            for chunk in production(source).split("fn ").skip(1) {
                let name: String = chunk
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                let Some(first) = OPS.iter().filter_map(|op| chunk.find(op)).min() else {
                    continue;
                };
                if EXEMPT.iter().any(|(f, n, _)| f == file && *n == name) {
                    continue;
                }
                let check = CHECKS.iter().filter_map(|c| chunk.find(c)).min();
                assert!(
                    check.is_some_and(|at| at < first),
                    "{file} {name} strips or compares a saved path before it checks it"
                );
                checked.push(name);
            }
        }
        assert!(checked.len() >= 2, "{checked:?}");
        // The exempt functions still exist: a rename cannot hide a new site.
        for (file, name, _) in EXEMPT {
            let source = FILES.iter().find(|(f, _)| f == file).unwrap().1;
            assert!(
                source.contains(&format!("fn {name}(")) || source.contains(&format!("fn {name}<")),
                "{file} {name}"
            );
        }
        // The cycle refuses before it places; every save refuses as written.
        let body = |source: &'static str, name: &str| {
            let at = source
                .find(&format!("fn {name}("))
                .unwrap_or_else(|| panic!("{name}"));
            &source[at..at
                + source[at + 3..]
                    .find("\nfn ")
                    .map_or(source.len() - at, |e| e + 3)]
        };
        let service = FILES[1].1;
        let place = body(service, "place_login_relative_remote_folder");
        assert!(
            place.find("refuse_an_unusable_remote_folder(") < place.find("resolve_home_scope(")
        );
        let lib = FILES[0].1;
        for name in [
            "save_cloud_config_cmd_blocking",
            "save_cloud_pairs_config_cmd_blocking",
            "add_cloud_pair_blocking",
        ] {
            assert!(
                body(lib, name).contains("refuse_an_unusable_remote_folder("),
                "{name}"
            );
        }
        assert!(body(lib, "setup_aerocloud").contains("validate_config(&config)"));
        assert!(body(FILES[2].1, "validate_config").contains("refuse_an_unusable_remote_folder("));
        let cli = include_str!("../bin/aeroftp_cli.rs");
        for name in ["cmd_aerocloud", "cmd_aerocloud_pair"] {
            assert!(
                body(cli, name).contains("refuse_an_unusable_remote_folder("),
                "{name}"
            );
        }
    }

    #[test]
    fn the_login_relative_rest_drops_every_separator_after_the_tilde() {
        assert_eq!(login_relative_rest("~"), Some(""));
        assert_eq!(login_relative_rest("~/"), Some(""));
        assert_eq!(login_relative_rest("~/docs"), Some("docs"));
        assert_eq!(
            login_relative_rest("~//docs/../shared"),
            Some("docs/../shared")
        );
        assert_eq!(login_relative_rest("~alice/docs"), None);
        assert_eq!(login_relative_rest("docs"), None);
        assert_eq!(login_relative_rest("/~/docs"), None);
    }
}
