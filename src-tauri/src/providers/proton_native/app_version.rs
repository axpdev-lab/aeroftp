//! The `x-pm-appversion` value AeroFTP sends to Proton.
//!
//! The SDK README asks third-party clients for
//! `external-drive-{name}@{semver}-{channel}+{suffix}`: `{name}` in lowercase
//! letters and underscores, `{semver}` as `major.minor.patch`, an optional
//! `stable` / `beta` / `alpha` channel and optional build metadata. Proton
//! support reads this value to troubleshoot and may block one version known to
//! ship a serious bug, so it must say exactly which build is talking.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/// Project identifier in the header. Lowercase letters only, as the SDK asks.
pub const APP_NAME: &str = "aeroftp";

/// Release channel of this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Beta,
    Alpha,
}

impl Channel {
    fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
            Channel::Alpha => "alpha",
        }
    }

    /// Debug builds are development builds: they report `alpha` so Proton
    /// never mistakes a local build for a released one.
    pub fn of_this_build() -> Self {
        if cfg!(debug_assertions) {
            Channel::Alpha
        } else {
            Channel::Stable
        }
    }
}

/// Builds the header value from a version string such as `4.2.3` or
/// `4.2.3-rc.1`. Pre-release and build parts of the crate version are dropped
/// from `{semver}`: the SDK pattern only admits `major.minor.patch`, and the
/// channel already says the build is not a stable one.
pub fn app_version_for(version: &str, channel: Channel) -> String {
    let core = version.split(['-', '+']).next().unwrap_or(version).trim();
    format!("external-drive-{APP_NAME}@{core}-{}", channel.as_str())
}

/// The header value for the running build.
pub fn app_version() -> String {
    app_version_for(env!("CARGO_PKG_VERSION"), Channel::of_this_build())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the shape the SDK README documents, so a change of format is
    /// caught here rather than by Proton's servers.
    fn matches_sdk_pattern(value: &str) -> bool {
        let Some(rest) = value.strip_prefix("external-drive-") else {
            return false;
        };
        let Some((name, version)) = rest.split_once('@') else {
            return false;
        };
        let name_ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_');
        let (version, _build) = version.split_once('+').unwrap_or((version, ""));
        let (semver, channel) = version.split_once('-').unwrap_or((version, ""));
        let parts: Vec<&str> = semver.split('.').collect();
        let semver_ok = parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
        let channel_ok = matches!(channel, "" | "stable" | "beta" | "alpha");
        name_ok && semver_ok && channel_ok
    }

    #[test]
    fn running_build_value_matches_the_sdk_pattern() {
        let value = app_version();
        assert!(
            matches_sdk_pattern(&value),
            "x-pm-appversion `{value}` does not match external-drive-{{name}}@{{semver}}-{{channel}}"
        );
    }

    #[test]
    fn prerelease_versions_keep_only_the_core_semver() {
        assert_eq!(
            app_version_for("4.2.3-rc.1+abc", Channel::Beta),
            "external-drive-aeroftp@4.2.3-beta"
        );
        assert_eq!(
            app_version_for("4.2.3", Channel::Stable),
            "external-drive-aeroftp@4.2.3-stable"
        );
    }

    #[test]
    fn never_claims_an_official_proton_client() {
        let value = app_version();
        for official in ["cli-drive", "macos-drive", "windows-drive", "web-drive"] {
            assert!(
                !value.starts_with(official),
                "`{value}` would masquerade as the official `{official}` client"
            );
        }
    }
}
