//! The two rules the stand-in and its probe MUST agree on, in one place.
//!
//! Both sides compute the Request object path independently: the portal builds
//! the path it exports the handle at, and the client predicts that same path so
//! it can subscribe BEFORE the method returns. If the two formulas ever drift,
//! the reply is emitted on a path nobody listens on, the client waits for a
//! signal that never comes, and the symptom is a hang that looks like a defect
//! in the application under test rather than in the harness.
//!
//! They were previously two independent copies, which is precisely the
//! arrangement that lets such a drift happen silently.

/// A D-Bus object path element accepts only `[A-Za-z0-9_]`. The portal spec says
/// `handle_token` must already be a valid element, and GTK obeys that, but a
/// token with a hyphen in it makes the portal build a path the bus rejects and
/// the caller then sees `InvalidObjectPath` from somewhere deep in zvariant,
/// with nothing pointing at the token. Found exactly that way while testing this
/// harness. Both sides sanitize identically so a sloppy token degrades into a
/// working request instead of an unreadable error.
///
/// `fallback` is used when the token maps to the empty string, so each side can
/// still say which one produced the path.
pub fn sanitize_path_element(token: &str, fallback: &str) -> String {
    let mapped: String = token
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if mapped.is_empty() {
        fallback.to_string()
    } else {
        mapped
    }
}

/// Build the Request object path the way xdg-desktop-portal does, because the
/// caller predicts it and subscribes BEFORE the method returns.
///
/// The rule is `/org/freedesktop/portal/desktop/request/<SENDER>/<TOKEN>` where
/// SENDER is the caller's unique name with the leading ':' dropped and every
/// '.' replaced by '_'.
pub fn request_path(sender: &str, token: &str) -> String {
    let escaped = sender.trim_start_matches(':').replace('.', "_");
    format!("/org/freedesktop/portal/desktop/request/{escaped}/{token}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyphens_and_dots_become_underscores() {
        assert_eq!(sanitize_path_element("gtk-123.4", "fb"), "gtk_123_4");
    }

    #[test]
    fn an_empty_result_falls_back() {
        assert_eq!(sanitize_path_element("---", "fb"), "___");
        assert_eq!(sanitize_path_element("", "fb"), "fb");
    }

    #[test]
    fn the_sender_is_escaped_the_way_the_portal_does_it() {
        assert_eq!(
            request_path(":1.42", "gtk1"),
            "/org/freedesktop/portal/desktop/request/1_42/gtk1"
        );
    }

    /// Pin: a stand-in that owns the portal name without the full v3
    /// NetworkMonitor surface breaks the app under GTK_USE_PORTAL=1 instead of
    /// degrading to "no portal". GetStatus and CanReach must stay alongside the
    /// three getters; selftest-portal.sh also probes them live over D-Bus.
    #[test]
    fn network_monitor_implements_the_v3_surface_it_advertises() {
        let src = include_str!("main.rs");
        assert!(
            src.contains("fn get_status("),
            "NetworkMonitor must implement GetStatus when advertising version 3"
        );
        assert!(
            src.contains("fn can_reach("),
            "NetworkMonitor must implement CanReach when advertising version 3"
        );
        assert!(
            src.contains("fn get_available(")
                && src.contains("fn get_metered(")
                && src.contains("fn get_connectivity("),
            "NetworkMonitor must still answer the three v2 getters"
        );
        // version() on NetworkMonitor returns 3 (the interface attribute, not
        // the doc comment that mentions the same name earlier).
        let nm = src
            .split("#[interface(name = \"org.freedesktop.portal.NetworkMonitor\")]")
            .nth(1)
            .expect("NetworkMonitor interface attribute present");
        let version_body = nm
            .split("fn version")
            .nth(1)
            .expect("NetworkMonitor version property present");
        let first_return_line = version_body
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("3") || *l == "3")
            .expect("NetworkMonitor version body should return 3");
        assert!(
            first_return_line.starts_with('3'),
            "NetworkMonitor must advertise version 3, got {first_return_line:?}"
        );
    }
}
