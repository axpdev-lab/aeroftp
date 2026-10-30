// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The override the CLI's `--timeout` sets is process-wide and set once, so it
//! is tested in a binary of its own, where no other test can see it.

use std::time::Duration;

use ftp_client_gui_lib::providers::http_limits::{limit, set_override, READ_TIMEOUT};

#[test]
fn set_override_replaces_every_provider_limit_once() {
    assert_eq!(limit(READ_TIMEOUT), READ_TIMEOUT);
    assert!(set_override(Duration::from_secs(90)));
    assert_eq!(limit(READ_TIMEOUT), Duration::from_secs(90));
    assert_eq!(limit(Duration::from_secs(30)), Duration::from_secs(90));
    assert!(!set_override(Duration::from_secs(5)));
    assert_eq!(limit(READ_TIMEOUT), Duration::from_secs(90));
}
