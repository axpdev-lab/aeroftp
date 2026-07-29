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
}
