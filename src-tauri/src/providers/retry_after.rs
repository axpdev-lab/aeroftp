//! Provider-side `Retry-After` parsing helpers shared by Google Drive,
//! Dropbox, OneDrive, and Box (T-DEBT-05, S1-T02b/c/d/e).
//!
//! Each provider parses its rate-limit response in a slightly different way:
//! - Google Drive: standard `Retry-After` header (seconds form in practice).
//! - Dropbox: `retry_after` field in the 429 JSON body (float seconds).
//! - OneDrive: `Retry-After` header (seconds) with `X-RateLimit-Reset`
//!   (epoch UTC seconds) as fallback.
//! - Box: standard `Retry-After` header (seconds).
//!
//! The pure functions here own the actual string/integer arithmetic so each
//! provider site stays a thin wrapper around a [`reqwest::Response`] (or
//! body string for Dropbox). That keeps the helpers covered by ordinary
//! unit tests instead of fighting a mocked `reqwest::Response`.
//!
//! Producers attach the parsed hint to the `ProviderError` message via
//! [`crate::transfer_dag::adaptive::embed_retry_after_marker`]; the
//! transfer-DAG executor extracts it via `parse_embedded_retry_after` and
//! forwards it to `AimdController::on_congestion_with_hint`.

use std::time::Duration;

/// Parse a `Retry-After` header value (or other seconds-as-string field).
///
/// Accepts a non-negative integer of seconds. Returns `None` if the input is
/// empty, has leading/trailing junk that does not trim cleanly, contains a
/// negative value, or is not a decimal integer.
///
/// HTTP also allows an HTTP-date form for `Retry-After`, but the four
/// providers wired into T-DEBT-05 (Google Drive, Dropbox, OneDrive, Box)
/// all return the seconds form in practice. Should a provider start
/// returning an HTTP-date, add a fallback here that prefers
/// `chrono::DateTime::parse_from_rfc2822` over introducing the `httpdate`
/// crate.
pub fn parse_retry_after_seconds(raw: &str) -> Option<Duration> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<u64>().ok().map(Duration::from_secs)
}

/// Parse a `retry_after` numeric field from a Dropbox 429 JSON body. The
/// Dropbox API spec puts the hint at the top level of the error JSON as a
/// floating-point value, so this helper accepts a `serde_json::Value` to
/// keep the JSON dependency local to the call site instead of parsing
/// inside the helper.
///
/// Returns `None` if the value is missing, negative, NaN, or infinite.
/// Fractional seconds round up to the nearest whole second to avoid
/// defeating the AIMD lower-clamp (which floors at 1 second anyway).
pub fn parse_retry_after_dropbox_value(body_json: &serde_json::Value) -> Option<Duration> {
    let secs = body_json.get("retry_after")?.as_f64()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    // Round up so a 0.4 s hint becomes 1 s (matches AIMD MIN_HINT_COOLDOWN).
    let rounded = secs.ceil() as u64;
    Some(Duration::from_secs(rounded))
}

/// OneDrive secondary signal: `X-RateLimit-Reset` carries an epoch UTC
/// seconds timestamp at which the throttle expires. Used as a fallback
/// only when the primary `Retry-After` header is absent.
///
/// Returns `None` when the header is unparseable or already in the past.
pub fn parse_x_ratelimit_reset(raw: &str, now_epoch_secs: u64) -> Option<Duration> {
    let reset_epoch: u64 = raw.trim().parse().ok()?;
    if reset_epoch <= now_epoch_secs {
        return None;
    }
    Some(Duration::from_secs(reset_epoch - now_epoch_secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_retry_after_seconds_accepts_plain_integer() {
        assert_eq!(
            parse_retry_after_seconds("30"),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn parse_retry_after_seconds_trims_whitespace() {
        assert_eq!(
            parse_retry_after_seconds("  45  "),
            Some(Duration::from_secs(45))
        );
    }

    #[test]
    fn parse_retry_after_seconds_rejects_empty() {
        assert_eq!(parse_retry_after_seconds(""), None);
        assert_eq!(parse_retry_after_seconds("   "), None);
    }

    #[test]
    fn parse_retry_after_seconds_rejects_negative_and_garbage() {
        assert_eq!(parse_retry_after_seconds("-5"), None);
        assert_eq!(parse_retry_after_seconds("abc"), None);
        assert_eq!(parse_retry_after_seconds("12.5"), None);
        assert_eq!(
            parse_retry_after_seconds("Wed, 21 Oct 2015 07:28:00 GMT"),
            None
        );
    }

    #[test]
    fn parse_retry_after_dropbox_extracts_top_level_field() {
        let body = json!({
            "error_summary": "too_many_requests/...",
            "error": {".tag": "too_many_requests"},
            "retry_after": 45.0
        });
        assert_eq!(
            parse_retry_after_dropbox_value(&body),
            Some(Duration::from_secs(45))
        );
    }

    #[test]
    fn parse_retry_after_dropbox_rounds_up_fractional() {
        let body = json!({"retry_after": 0.4});
        assert_eq!(
            parse_retry_after_dropbox_value(&body),
            Some(Duration::from_secs(1))
        );
        let body2 = json!({"retry_after": 12.1});
        assert_eq!(
            parse_retry_after_dropbox_value(&body2),
            Some(Duration::from_secs(13))
        );
    }

    #[test]
    fn parse_retry_after_dropbox_handles_missing_field() {
        let body = json!({"error_summary": "...", "error": {}});
        assert_eq!(parse_retry_after_dropbox_value(&body), None);
    }

    #[test]
    fn parse_retry_after_dropbox_rejects_negative_or_nan() {
        let neg = json!({"retry_after": -5.0});
        assert_eq!(parse_retry_after_dropbox_value(&neg), None);
        let nan = json!({"retry_after": "not a number"});
        assert_eq!(parse_retry_after_dropbox_value(&nan), None);
    }

    #[test]
    fn parse_x_ratelimit_reset_returns_delta_when_in_future() {
        // now=100, reset=160 → 60s delta.
        assert_eq!(
            parse_x_ratelimit_reset("160", 100),
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn parse_x_ratelimit_reset_returns_none_when_in_past_or_equal() {
        assert_eq!(parse_x_ratelimit_reset("90", 100), None);
        assert_eq!(parse_x_ratelimit_reset("100", 100), None);
    }

    #[test]
    fn parse_x_ratelimit_reset_rejects_malformed() {
        assert_eq!(parse_x_ratelimit_reset("abc", 100), None);
        assert_eq!(parse_x_ratelimit_reset("", 100), None);
        assert_eq!(parse_x_ratelimit_reset("-50", 100), None);
    }
}
