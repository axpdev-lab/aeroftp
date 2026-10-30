//! How long an HTTP provider waits for a server that does not answer.
//!
//! Every HTTP provider builds its own `reqwest::Client`, each with the limits
//! that suit its API. They all pass them through [`limit`], so the CLI's
//! global `--timeout` replaces every one of them at once, and the GUI, which
//! never sets it, keeps the providers' own values.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::sync::OnceLock;
use std::time::Duration;

/// The longest silence while a request is under way. 1800 s accommodates
/// large uploads on slow links and server-side post-processing (md5,
/// replication): 300 s killed 1 GiB WebDAV uploads below about 27 Mbps.
pub const READ_TIMEOUT: Duration = Duration::from_secs(1800);

/// One sign-in step (an OAuth request or access token), connect and reply.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(30);

static OVERRIDE: OnceLock<Duration> = OnceLock::new();

/// Replace every HTTP limit for the rest of the process. The CLI calls it
/// once, from `--timeout`, before it builds a provider. Returns `false` when
/// a limit was already set.
pub fn set_override(limit: Duration) -> bool {
    OVERRIDE.set(limit).is_ok()
}

/// The limit to use where a provider's own is `default`.
pub fn limit(default: Duration) -> Duration {
    resolve(OVERRIDE.get().copied(), default)
}

/// [`limit`] without the process-wide state, so it can be tested.
fn resolve(override_limit: Option<Duration>, default: Duration) -> Duration {
    override_limit.unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_keeps_its_own_limit_until_one_is_set() {
        assert_eq!(resolve(None, READ_TIMEOUT), READ_TIMEOUT);
        assert_eq!(
            resolve(None, Duration::from_secs(30)),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn a_set_limit_replaces_shorter_and_longer_ones_alike() {
        let set = Some(Duration::from_secs(90));
        assert_eq!(resolve(set, READ_TIMEOUT), Duration::from_secs(90));
        assert_eq!(
            resolve(set, Duration::from_secs(30)),
            Duration::from_secs(90)
        );
    }
}
