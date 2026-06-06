//! Minimal AWS STS `AssumeRole` client: the S3 *credential plane*.
//!
//! Hand-rolled on purpose. The full `aws-sdk-sts` would add ~26 transitive
//! crates (the `aws-smithy-*` family + `aws-sigv4`/`aws-runtime`/`aws-types`)
//! for what is a single signed POST plus a small XML parse, and its headline
//! advantage (battle-tested credential refresh) does not apply here: the
//! refresh/caching layer has to be written by hand regardless (see issue #301,
//! Fase 3). This module reuses the same SigV4 primitives as the S3 data-plane
//! signer (`hmac`/`sha2`/`hex`) but targets the `sts` service with an
//! `application/x-www-form-urlencoded` body.
//!
//! [`assume_role`] exchanges long-term base credentials (access key + secret)
//! for temporary credentials (access key + secret + session token + expiry)
//! that then feed the data-plane signer through `S3Config.session_token`.

use crate::providers::types::ProviderError;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use quick_xml::events::Event;
use quick_xml::Reader;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

type HmacSha256 = Hmac<Sha256>;

/// AWS STS Query API version pinned by the `AssumeRole` action.
const STS_API_VERSION: &str = "2011-06-15";

/// Temporary credentials returned by `AssumeRole`.
///
/// `secret_access_key` and `session_token` are wrapped in [`SecretString`] so
/// they are zeroized on drop, matching the rest of the credential plumbing.
#[derive(Clone)]
pub struct TempCredentials {
    pub access_key_id: String,
    pub secret_access_key: SecretString,
    pub session_token: SecretString,
    /// Hard expiry returned by STS (UTC). `None` only if STS omitted it, which
    /// it never does in practice; treated as "refresh eagerly" by Fase 3.
    pub expiration: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for TempCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never leak the secret or session token through Debug.
        f.debug_struct("TempCredentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .field("session_token", &"<redacted>")
            .field("expiration", &self.expiration)
            .finish()
    }
}

/// Inputs for a single `AssumeRole` call.
pub struct AssumeRoleRequest<'a> {
    /// AWS region; selects both the regional STS endpoint and the SigV4
    /// signing region.
    pub region: &'a str,
    /// Long-term base access key id used to sign the STS request.
    pub access_key_id: &'a str,
    /// Long-term base secret used to sign the STS request.
    pub secret_access_key: &'a SecretString,
    /// ARN of the role to assume (required).
    pub role_arn: &'a str,
    /// Caller-chosen session name. Must match `[\w+=,.@-]{2,64}`.
    pub role_session_name: &'a str,
    /// Requested lifetime in seconds (900..=43200). `None` lets AWS apply the
    /// role default (typically 3600).
    pub duration_seconds: Option<u32>,
    /// `ExternalId` for cross-account confused-deputy protection.
    pub external_id: Option<&'a str>,
    /// MFA device serial/ARN; paired with `mfa_token_code`.
    pub mfa_serial: Option<&'a str>,
    /// One-time MFA token code; only meaningful with `mfa_serial`.
    pub mfa_token_code: Option<&'a str>,
    /// Session token of the *base* credentials, on the rare path where they
    /// are themselves temporary (chained AssumeRole). Usually `None`.
    pub base_session_token: Option<&'a SecretString>,
}

/// Reject regions that are not plain AWS region tokens before they are spliced
/// into the `sts.<region>.amazonaws.com` endpoint. The region comes from the
/// user's profile, but a value containing `/`, `@`, `.` or whitespace would
/// reshape the host (e.g. `foo/../evil.com` splits the host off into a path),
/// so we constrain it to the AWS region alphabet (letters, digits, hyphen).
fn validate_sts_region(region: &str) -> Result<(), ProviderError> {
    let r = region.trim();
    if r.is_empty() || r.eq_ignore_ascii_case("auto") {
        return Err(ProviderError::InvalidConfig(
            "STS AssumeRole needs a concrete AWS region (e.g. us-east-1); \
             'auto'/empty has no STS endpoint."
                .to_string(),
        ));
    }
    if !r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(ProviderError::InvalidConfig(format!(
            "STS AssumeRole: invalid AWS region '{region}' \
             (only letters, digits and hyphens are allowed)."
        )));
    }
    Ok(())
}

/// Validate `RoleSessionName` against the STS-documented pattern
/// `[\w+=,.@-]{2,64}` before the request, so an obviously malformed value fails
/// locally with a clear message instead of an opaque STS `ValidationError`.
fn validate_role_session_name(name: &str) -> Result<(), ProviderError> {
    let len_ok = (2..=64).contains(&name.len());
    let chars_ok = name.bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'=' | b',' | b'.' | b'@' | b'-')
    });
    if !len_ok || !chars_ok {
        return Err(ProviderError::InvalidConfig(format!(
            "STS AssumeRole: invalid RoleSessionName '{name}' \
             (allowed characters: letters, digits and _+=,.@- ; length 2..=64)."
        )));
    }
    Ok(())
}

/// Resolve the regional STS endpoint host for `region`.
///
/// Standard and GovCloud partitions use `sts.<region>.amazonaws.com`; the
/// China partition uses the `.amazonaws.com.cn` suffix. Regional endpoints are
/// preferred over the legacy global `sts.amazonaws.com` (which is us-east-1
/// only and reduces availability). Callers must validate `region` with
/// [`validate_sts_region`] first.
fn sts_endpoint_host(region: &str) -> String {
    if region.starts_with("cn-") {
        format!("sts.{region}.amazonaws.com.cn")
    } else {
        format!("sts.{region}.amazonaws.com")
    }
}

/// Build the `application/x-www-form-urlencoded` request body for `AssumeRole`.
fn build_assume_role_body(req: &AssumeRoleRequest<'_>) -> String {
    let mut params: Vec<(&str, String)> = vec![
        ("Action", "AssumeRole".to_string()),
        ("Version", STS_API_VERSION.to_string()),
        ("RoleArn", req.role_arn.to_string()),
        ("RoleSessionName", req.role_session_name.to_string()),
    ];
    if let Some(d) = req.duration_seconds {
        params.push(("DurationSeconds", d.to_string()));
    }
    if let Some(ext) = req.external_id.filter(|s| !s.is_empty()) {
        params.push(("ExternalId", ext.to_string()));
    }
    // MFA is all-or-nothing: AWS rejects a SerialNumber without a TokenCode, so
    // emit the pair only when both are present (a lone serial is ignored).
    if let (Some(serial), Some(code)) = (
        req.mfa_serial.filter(|s| !s.is_empty()),
        req.mfa_token_code.filter(|s| !s.is_empty()),
    ) {
        params.push(("SerialNumber", serial.to_string()));
        params.push(("TokenCode", code.to_string()));
    }
    params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// A fully signed STS request ready to be sent.
struct SignedStsRequest {
    url: String,
    body: String,
    amz_date: String,
    authorization: String,
    content_sha256: String,
    security_token: Option<String>,
}

/// Produce the SigV4-signed POST for an `AssumeRole` call at instant `now`.
///
/// Split out from [`assume_role`] so the signing is a pure function of its
/// inputs (including the timestamp) and can be unit-tested without a network.
fn sign_assume_role(req: &AssumeRoleRequest<'_>, now: DateTime<Utc>) -> SignedStsRequest {
    let host = sts_endpoint_host(req.region);
    let url = format!("https://{host}/");
    let body = build_assume_role_body(req);

    let date_stamp = now.format("%Y%m%d").to_string();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

    let content_sha256 = {
        let mut hasher = Sha256::new();
        hasher.update(body.as_bytes());
        hex::encode(hasher.finalize())
    };

    // Canonical headers (sorted, lowercase, trimmed). Content-Type, Host,
    // x-amz-content-sha256 and x-amz-date are always signed. If the base
    // credentials are themselves temporary, their session token is signed too.
    let content_type = "application/x-www-form-urlencoded; charset=utf-8";
    let security_token = req
        .base_session_token
        .map(|t| t.expose_secret().to_string());

    let mut headers: Vec<(String, String)> = vec![
        ("content-type".to_string(), content_type.to_string()),
        ("host".to_string(), host.clone()),
        ("x-amz-content-sha256".to_string(), content_sha256.clone()),
        ("x-amz-date".to_string(), amz_date.clone()),
    ];
    if let Some(ref token) = security_token {
        headers.push(("x-amz-security-token".to_string(), token.clone()));
    }
    headers.sort_by(|a, b| a.0.cmp(&b.0));

    let signed_headers_str = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_headers = headers
        .iter()
        .map(|(k, v)| format!("{}:{}\n", k, v.trim()))
        .collect::<String>();

    // POST to "/" with no query string.
    let canonical_request =
        format!("POST\n/\n\n{canonical_headers}\n{signed_headers_str}\n{content_sha256}");
    let canonical_request_hash = {
        let mut hasher = Sha256::new();
        hasher.update(canonical_request.as_bytes());
        hex::encode(hasher.finalize())
    };

    let credential_scope = format!("{date_stamp}/{}/sts/aws4_request", req.region);
    let string_to_sign =
        format!("AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{canonical_request_hash}");

    fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    let k_date = hmac_sha256(
        format!("AWS4{}", req.secret_access_key.expose_secret()).as_bytes(),
        date_stamp.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, req.region.as_bytes());
    let k_service = hmac_sha256(&k_region, b"sts");
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        req.access_key_id, credential_scope, signed_headers_str, signature
    );

    SignedStsRequest {
        url,
        body,
        amz_date,
        authorization,
        content_sha256,
        security_token,
    }
}

/// Raw result of one signed STS round-trip, kept unparsed so the caller can
/// decide whether to retry (clock skew) before mapping to a `ProviderError`.
struct StsResponse {
    status: reqwest::StatusCode,
    /// `Date` response header (RFC 2822), used to correct a local clock skew.
    server_date: Option<DateTime<Utc>>,
    body: String,
}

/// Sign and send one `AssumeRole` POST at the given clock instant.
async fn send_assume_role(
    client: &reqwest::Client,
    req: &AssumeRoleRequest<'_>,
    now: DateTime<Utc>,
) -> Result<StsResponse, ProviderError> {
    let signed = sign_assume_role(req, now);

    let mut request = client
        .post(&signed.url)
        .header(
            "Content-Type",
            "application/x-www-form-urlencoded; charset=utf-8",
        )
        .header("X-Amz-Date", &signed.amz_date)
        .header("X-Amz-Content-Sha256", &signed.content_sha256)
        .header("Authorization", &signed.authorization)
        .header("Accept", "application/xml");
    if let Some(ref token) = signed.security_token {
        request = request.header("X-Amz-Security-Token", token);
    }

    let response =
        request.body(signed.body).send().await.map_err(|e| {
            ProviderError::NetworkError(format!("STS AssumeRole request failed: {e}"))
        })?;

    let status = response.status();
    let server_date = response
        .headers()
        .get("date")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| DateTime::parse_from_rfc2822(s).ok())
        .map(|dt| dt.with_timezone(&Utc));
    let body = response
        .text()
        .await
        .map_err(|e| ProviderError::NetworkError(format!("STS response read failed: {e}")))?;

    Ok(StsResponse {
        status,
        server_date,
        body,
    })
}

/// True when an STS failure looks like a clock-skew / stale-signature problem
/// that a single retry with the server's own clock can fix.
fn is_clock_skew_error(body: &str) -> bool {
    let (code, message) = parse_sts_error(body);
    let hay = format!(
        "{} {}",
        code.unwrap_or_default(),
        message.unwrap_or_default()
    )
    .to_ascii_lowercase();
    hay.contains("requesttimetooskewed")
        || hay.contains("signaturedoesnotmatch")
        || hay.contains("skew")
}

/// Map a non-success STS response onto the right `ProviderError`. Access / role
/// / MFA problems are authentication failures; anything else (throttling,
/// service faults) is a server error.
fn map_sts_error(status: reqwest::StatusCode, body: &str) -> ProviderError {
    let (code, message) = parse_sts_error(body);
    let detail = match (code.as_deref(), message.as_deref()) {
        (Some(c), Some(m)) => format!("STS AssumeRole denied: {c}: {m}"),
        (Some(c), None) => format!("STS AssumeRole denied: {c}"),
        (None, Some(m)) => format!("STS AssumeRole denied: {m}"),
        (None, None) => format!("STS AssumeRole failed with HTTP {}", status.as_u16()),
    };
    let is_auth = code
        .as_deref()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            c.contains("accessdenied")
                || c.contains("invalidclienttoken")
                || c.contains("signature")
                || c.contains("expiredtoken")
                || c.contains("mfa")
                || c.contains("validationerror")
                || c.contains("malformedpolicy")
        })
        .unwrap_or(status.as_u16() == 403);
    if is_auth {
        ProviderError::AuthenticationFailed(detail)
    } else {
        ProviderError::ServerError(detail)
    }
}

/// Exchange long-term base credentials for temporary credentials via STS
/// `AssumeRole`, signing at clock instant `now`.
///
/// `now` is supplied by the caller (the S3 provider passes its skew-adjusted
/// clock) so the credential plane uses the same time source as the data plane.
/// If the first attempt fails with a clock-skew / stale-signature error and STS
/// returned its own `Date`, the request is retried once with the server clock,
/// mirroring the data-plane self-correction (issue #301, M1).
///
/// `client` is reused from the S3 provider so TLS/proxy/timeout settings are
/// shared. Errors are mapped onto [`ProviderError`]: STS deny/auth failures to
/// [`ProviderError::AuthenticationFailed`], malformed responses to
/// [`ProviderError::ParseError`], transport faults to
/// [`ProviderError::NetworkError`].
pub async fn assume_role(
    client: &reqwest::Client,
    req: &AssumeRoleRequest<'_>,
    now: DateTime<Utc>,
) -> Result<TempCredentials, ProviderError> {
    validate_sts_region(req.region)?;
    if req.role_arn.trim().is_empty() {
        return Err(ProviderError::InvalidConfig(
            "STS AssumeRole requires a Role ARN.".to_string(),
        ));
    }
    validate_role_session_name(req.role_session_name)?;

    let mut attempt = send_assume_role(client, req, now).await?;

    // Single retry with STS's own clock when the failure smells of clock skew.
    if !attempt.status.is_success() {
        if let (true, Some(server_time)) = (is_clock_skew_error(&attempt.body), attempt.server_date)
        {
            attempt = send_assume_role(client, req, server_time).await?;
        }
    }

    if !attempt.status.is_success() {
        return Err(map_sts_error(attempt.status, &attempt.body));
    }

    // Parse, then scrub the raw response: it carries the secret access key and
    // session token in cleartext and `String` is not zeroized on drop (L1).
    let creds = parse_assume_role_response(&attempt.body);
    attempt.body.zeroize();
    creds
}

/// Extract `Code`/`Message` from an STS `ErrorResponse` body, if present.
fn parse_sts_error(xml: &str) -> (Option<String>, Option<String>) {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut code = None;
    let mut message = None;
    let mut in_error = false;
    let mut cur: Option<&'static str> = None;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"Error" => in_error = true,
                b"Code" if in_error => cur = Some("code"),
                b"Message" if in_error => cur = Some("message"),
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if let Some(field) = cur {
                    // AWS credential values are base64 (no XML-special chars),
                    // so the raw bytes are byte-exact; matches the rest of the
                    // S3 XML parsing in this crate.
                    let text = String::from_utf8_lossy(t.as_ref()).into_owned();
                    match field {
                        "code" => code = Some(text),
                        "message" => message = Some(text),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"Error" => in_error = false,
                b"Code" | b"Message" => cur = None,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    (code, message)
}

/// Parse the `<Credentials>` block of a successful `AssumeRole` response.
fn parse_assume_role_response(xml: &str) -> Result<TempCredentials, ProviderError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut in_credentials = false;
    let mut cur: Option<&'static str> = None;
    let mut access_key_id: Option<String> = None;
    let mut secret_access_key: Option<String> = None;
    let mut session_token: Option<String> = None;
    let mut expiration_raw: Option<String> = None;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"Credentials" => in_credentials = true,
                b"AccessKeyId" if in_credentials => cur = Some("akid"),
                b"SecretAccessKey" if in_credentials => cur = Some("secret"),
                b"SessionToken" if in_credentials => cur = Some("token"),
                b"Expiration" if in_credentials => cur = Some("expiry"),
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if let Some(field) = cur {
                    // AWS credential values are base64 (no XML-special chars),
                    // so the raw bytes are byte-exact; matches the rest of the
                    // S3 XML parsing in this crate.
                    let text = String::from_utf8_lossy(t.as_ref()).into_owned();
                    match field {
                        "akid" => access_key_id = Some(text),
                        "secret" => secret_access_key = Some(text),
                        "token" => session_token = Some(text),
                        "expiry" => expiration_raw = Some(text),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"Credentials" => in_credentials = false,
                b"AccessKeyId" | b"SecretAccessKey" | b"SessionToken" | b"Expiration" => cur = None,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(ProviderError::ParseError(format!(
                    "STS response XML parse error: {e}"
                )))
            }
            _ => {}
        }
        buf.clear();
    }

    let access_key_id = access_key_id
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ProviderError::ParseError("STS response missing AccessKeyId".to_string()))?;
    let secret_access_key = secret_access_key.filter(|s| !s.is_empty()).ok_or_else(|| {
        ProviderError::ParseError("STS response missing SecretAccessKey".to_string())
    })?;
    let session_token = session_token.filter(|s| !s.is_empty()).ok_or_else(|| {
        ProviderError::ParseError("STS response missing SessionToken".to_string())
    })?;
    let expiration = expiration_raw.and_then(|s| {
        DateTime::parse_from_rfc3339(&s)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    });

    Ok(TempCredentials {
        access_key_id,
        secret_access_key: SecretString::from(secret_access_key),
        session_token: SecretString::from(session_token),
        expiration,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn base_req<'a>(
        region: &'a str,
        akid: &'a str,
        secret: &'a SecretString,
        role: &'a str,
        session: &'a str,
    ) -> AssumeRoleRequest<'a> {
        AssumeRoleRequest {
            region,
            access_key_id: akid,
            secret_access_key: secret,
            role_arn: role,
            role_session_name: session,
            duration_seconds: None,
            external_id: None,
            mfa_serial: None,
            mfa_token_code: None,
            base_session_token: None,
        }
    }

    #[test]
    fn region_validation_blocks_endpoint_injection() {
        for ok in ["us-east-1", "eu-central-1", "cn-north-1", "us-gov-west-1"] {
            assert!(validate_sts_region(ok).is_ok(), "region {ok:?} should pass");
        }
        for bad in [
            "",
            "auto",
            "us-east-1/../evil.com",
            "us@evil",
            "us east 1",
            "us.east.1",
            "us#1",
            "sts.evil.com:443",
        ] {
            assert!(
                validate_sts_region(bad).is_err(),
                "region {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn endpoint_host_resolves_per_partition() {
        assert_eq!(
            sts_endpoint_host("us-east-1"),
            "sts.us-east-1.amazonaws.com"
        );
        assert_eq!(
            sts_endpoint_host("eu-central-1"),
            "sts.eu-central-1.amazonaws.com"
        );
        assert_eq!(
            sts_endpoint_host("us-gov-west-1"),
            "sts.us-gov-west-1.amazonaws.com"
        );
        assert_eq!(
            sts_endpoint_host("cn-north-1"),
            "sts.cn-north-1.amazonaws.com.cn"
        );
    }

    #[test]
    fn body_includes_required_and_optional_params() {
        let secret = SecretString::from("secret".to_string());
        let mut req = base_req(
            "us-east-1",
            "AKIA",
            &secret,
            "arn:aws:iam::123456789012:role/Demo",
            "aeroftp-session",
        );
        req.duration_seconds = Some(7200);
        req.external_id = Some("ext-id-42");
        let body = build_assume_role_body(&req);
        assert!(body.contains("Action=AssumeRole"));
        assert!(body.contains("Version=2011-06-15"));
        // ARN colons/slashes are percent-encoded.
        assert!(body.contains("RoleArn=arn%3Aaws%3Aiam%3A%3A123456789012%3Arole%2FDemo"));
        assert!(body.contains("RoleSessionName=aeroftp-session"));
        assert!(body.contains("DurationSeconds=7200"));
        assert!(body.contains("ExternalId=ext-id-42"));
    }

    #[test]
    fn mfa_token_only_emitted_with_serial() {
        let secret = SecretString::from("secret".to_string());
        let mut req = base_req("us-east-1", "AKIA", &secret, "arn:role", "sess");
        // Token code without serial must not leak into the body.
        req.mfa_token_code = Some("123456");
        let body = build_assume_role_body(&req);
        assert!(!body.contains("TokenCode"));
        assert!(!body.contains("SerialNumber"));

        req.mfa_serial = Some("arn:aws:iam::123:mfa/user");
        let body = build_assume_role_body(&req);
        assert!(body.contains("SerialNumber=arn%3Aaws%3Aiam%3A%3A123%3Amfa%2Fuser"));
        assert!(body.contains("TokenCode=123456"));
    }

    #[test]
    fn signature_is_deterministic_and_well_formed() {
        let secret = SecretString::from("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string());
        let req = base_req(
            "us-east-1",
            "AKIDEXAMPLE",
            &secret,
            "arn:aws:iam::123456789012:role/Demo",
            "aeroftp",
        );
        let now = Utc.with_ymd_and_hms(2026, 6, 3, 12, 0, 0).unwrap();
        let s1 = sign_assume_role(&req, now);
        let s2 = sign_assume_role(&req, now);
        // Deterministic for fixed inputs (regression guard).
        assert_eq!(s1.authorization, s2.authorization);
        assert_eq!(s1.url, "https://sts.us-east-1.amazonaws.com/");
        assert!(s1
            .authorization
            .starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"));
        assert!(s1.authorization.contains("/us-east-1/sts/aws4_request"));
        // content-type, host, x-amz-content-sha256, x-amz-date are signed.
        assert!(s1
            .authorization
            .contains("SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date,"));
    }

    #[test]
    fn signature_changes_with_secret_and_body() {
        let secret_a = SecretString::from("AAAA".to_string());
        let secret_b = SecretString::from("BBBB".to_string());
        let now = Utc.with_ymd_and_hms(2026, 6, 3, 12, 0, 0).unwrap();

        let req_a = base_req("us-east-1", "AKID", &secret_a, "arn:role", "sess");
        let req_b = base_req("us-east-1", "AKID", &secret_b, "arn:role", "sess");
        assert_ne!(
            sign_assume_role(&req_a, now).authorization,
            sign_assume_role(&req_b, now).authorization,
            "different secret must yield different signature"
        );

        let mut req_c = base_req("us-east-1", "AKID", &secret_a, "arn:role", "sess");
        req_c.duration_seconds = Some(3600);
        assert_ne!(
            sign_assume_role(&req_a, now).authorization,
            sign_assume_role(&req_c, now).authorization,
            "different body must yield different signature"
        );
    }

    #[test]
    fn base_session_token_is_signed_when_present() {
        let secret = SecretString::from("secret".to_string());
        let token = SecretString::from("FwoGZXIvYXdzEXAMPLE".to_string());
        let mut req = base_req("us-east-1", "AKID", &secret, "arn:role", "sess");
        req.base_session_token = Some(&token);
        let now = Utc.with_ymd_and_hms(2026, 6, 3, 12, 0, 0).unwrap();
        let signed = sign_assume_role(&req, now);
        assert_eq!(
            signed.security_token.as_deref(),
            Some("FwoGZXIvYXdzEXAMPLE")
        );
        assert!(signed
            .authorization
            .contains("x-amz-content-sha256;x-amz-date;x-amz-security-token,"));
    }

    #[test]
    fn parses_successful_assume_role_response() {
        let xml = r#"<?xml version="1.0"?>
<AssumeRoleResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
  <AssumeRoleResult>
    <Credentials>
      <AccessKeyId>ASIAEXAMPLE</AccessKeyId>
      <SecretAccessKey>secretkey/EXAMPLE</SecretAccessKey>
      <SessionToken>FQoGZXIvYXdzEXAMPLETOKEN</SessionToken>
      <Expiration>2026-06-03T18:30:00Z</Expiration>
    </Credentials>
    <AssumedRoleUser>
      <Arn>arn:aws:sts::123456789012:assumed-role/Demo/aeroftp</Arn>
      <AssumedRoleId>AROAEXAMPLE:aeroftp</AssumedRoleId>
    </AssumedRoleUser>
  </AssumeRoleResult>
</AssumeRoleResponse>"#;
        let creds = parse_assume_role_response(xml).expect("parse ok");
        assert_eq!(creds.access_key_id, "ASIAEXAMPLE");
        assert_eq!(creds.secret_access_key.expose_secret(), "secretkey/EXAMPLE");
        assert_eq!(
            creds.session_token.expose_secret(),
            "FQoGZXIvYXdzEXAMPLETOKEN"
        );
        assert_eq!(
            creds.expiration,
            Some(Utc.with_ymd_and_hms(2026, 6, 3, 18, 30, 0).unwrap())
        );
    }

    #[test]
    fn missing_fields_are_parse_errors() {
        let xml = r#"<AssumeRoleResponse><AssumeRoleResult><Credentials>
            <AccessKeyId>ASIA</AccessKeyId>
        </Credentials></AssumeRoleResult></AssumeRoleResponse>"#;
        let err = parse_assume_role_response(xml).unwrap_err();
        assert!(matches!(err, ProviderError::ParseError(_)));
    }

    #[test]
    fn parses_sts_error_body() {
        let xml = r#"<ErrorResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
  <Error>
    <Type>Sender</Type>
    <Code>AccessDenied</Code>
    <Message>User is not authorized to perform sts:AssumeRole</Message>
  </Error>
  <RequestId>abc-123</RequestId>
</ErrorResponse>"#;
        let (code, message) = parse_sts_error(xml);
        assert_eq!(code.as_deref(), Some("AccessDenied"));
        assert_eq!(
            message.as_deref(),
            Some("User is not authorized to perform sts:AssumeRole")
        );
    }

    #[test]
    fn role_session_name_validation() {
        // Default and well-formed names pass.
        assert!(validate_role_session_name("aeroftp-session").is_ok());
        assert!(validate_role_session_name("Team_Sync+1=a,b.c@d").is_ok());
        assert!(validate_role_session_name("ab").is_ok());
        // Too short / empty / too long / illegal chars are rejected locally.
        assert!(validate_role_session_name("a").is_err());
        assert!(validate_role_session_name("").is_err());
        assert!(validate_role_session_name(&"x".repeat(65)).is_err());
        assert!(validate_role_session_name("has space").is_err());
        assert!(validate_role_session_name("has/slash").is_err());
    }

    #[test]
    fn clock_skew_errors_are_detected_for_retry() {
        let skewed = r#"<ErrorResponse><Error><Code>RequestTimeTooSkewed</Code>
            <Message>The difference between the request time and the current time is too large.</Message>
            </Error></ErrorResponse>"#;
        assert!(is_clock_skew_error(skewed));
        let sig = r#"<ErrorResponse><Error><Code>SignatureDoesNotMatch</Code>
            <Message>Signature expired</Message></Error></ErrorResponse>"#;
        assert!(is_clock_skew_error(sig));
        // A genuine authorization failure must NOT trigger a clock retry.
        let denied = r#"<ErrorResponse><Error><Code>AccessDenied</Code>
            <Message>User is not authorized to perform sts:AssumeRole</Message>
            </Error></ErrorResponse>"#;
        assert!(!is_clock_skew_error(denied));
    }

    #[test]
    fn mfa_pair_is_signed_into_canonical_request() {
        let secret = SecretString::from("secret".to_string());
        let mut req = base_req("us-east-1", "AKID", &secret, "arn:role", "sess");
        req.mfa_serial = Some("arn:aws:iam::123456789012:mfa/user");
        req.mfa_token_code = Some("654321");
        let body = build_assume_role_body(&req);
        assert!(body.contains("SerialNumber=arn%3Aaws%3Aiam%3A%3A123456789012%3Amfa%2Fuser"));
        assert!(body.contains("TokenCode=654321"));
    }
}
