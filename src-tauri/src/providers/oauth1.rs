//! OAuth 1.0a Authentication Module (RFC 5849)
//!
//! Provides HMAC-SHA1 request signing for 4shared and other OAuth 1.0 APIs.
//! Zero new dependencies: uses hmac, sha1, base64, rand already in Cargo.toml.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use base64::Engine;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// OAuth 1.0 credentials (consumer + token pair)
/// A3-06: Implements Drop to zeroize secrets when the struct goes out of scope
#[derive(Debug)]
pub struct OAuth1Credentials {
    pub consumer_key: String,
    pub consumer_secret: String,
    pub token: String,
    pub token_secret: String,
}

impl Drop for OAuth1Credentials {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.consumer_secret.zeroize();
        self.token.zeroize();
        self.token_secret.zeroize();
    }
}

/// RFC 5849 percent-encoding (uppercase, unreserved chars only)
pub fn percent_encode(input: &str) -> String {
    let mut encoded = String::with_capacity(input.len() * 2);
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    encoded
}

/// Generate a random nonce (32 alphanumeric chars).
///
/// FS-001: Uses `rand::thread_rng()` which is backed by a CSPRNG (ChaCha20 on most
/// platforms). 32 chars from a 36-char alphabet yields ~1.65 × 10^49 combinations,
/// making nonce collisions astronomically unlikely. Combined with the per-request
/// Unix timestamp, this satisfies RFC 5849 Section 3.3 uniqueness requirements.
pub fn generate_nonce() -> String {
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| {
            let idx: u8 = rng.gen_range(0..36);
            if idx < 10 {
                (b'0' + idx) as char
            } else {
                (b'a' + idx - 10) as char
            }
        })
        .collect()
}

/// Current Unix timestamp as string
pub fn generate_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

/// Build HMAC-SHA1 signature base string and sign it
fn sign(
    method: &str,
    url: &str,
    params: &[(String, String)],
    consumer_secret: &str,
    token_secret: &str,
) -> String {
    // 1. Parameter string. RFC 5849 3.4.1.3.2: every pair is encoded first,
    // then sorted by encoded name and, for a repeated name, by encoded value
    // (`%5B` sorts before `A`, and `x=1&x=2` keeps both pairs).
    let mut encoded: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (percent_encode(k), percent_encode(v)))
        .collect();
    encoded.sort();
    let param_string: String = encoded
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");

    // 2. Signature base string: METHOD&url&params
    let base_string = format!(
        "{}&{}&{}",
        method.to_uppercase(),
        percent_encode(url),
        percent_encode(&param_string)
    );

    // 3. Signing key: consumer_secret&token_secret
    let signing_key = format!(
        "{}&{}",
        percent_encode(consumer_secret),
        percent_encode(token_secret)
    );

    // 4. HMAC-SHA1
    let mut mac =
        HmacSha1::new_from_slice(signing_key.as_bytes()).expect("HMAC accepts any key length");
    mac.update(base_string.as_bytes());
    let result = mac.finalize().into_bytes();

    base64::engine::general_purpose::STANDARD.encode(result)
}

/// Split a URL into base URL (without query string) and query parameters.
/// RFC 5849 Section 3.4.1.2: query params must be extracted and included
/// in the signature base string separately from the URL.
fn split_url_query(url: &str) -> (String, Vec<(String, String)>) {
    if let Some(qmark) = url.find('?') {
        let base = url[..qmark].to_string();
        let query = &url[qmark + 1..];
        // RFC 5849 3.4.1.3.1: the query is parsed as
        // application/x-www-form-urlencoded, so names and values are DECODED
        // here (`%20` and `+` become a space). `sign` encodes every parameter
        // once; keeping the wire form here signed `a%2520b` for `a%20b`.
        let params: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        (base, params)
    } else {
        (url.to_string(), Vec::new())
    }
}

/// Build an OAuth 1.0 `Authorization` header for a signed request.
///
/// `extra_params` are additional query/body params that must be included
/// in the signature base string (but NOT in the Authorization header).
///
/// RFC 5849: The URL passed to this function may contain query parameters.
/// They are automatically extracted, included in the signature base string,
/// and the base URL (without query string) is used for signing.
pub fn authorization_header(
    method: &str,
    url: &str,
    creds: &OAuth1Credentials,
    extra_params: &[(&str, &str)],
) -> String {
    let nonce = generate_nonce();
    let timestamp = generate_timestamp();

    // RFC 5849 Section 3.4.1.2: Extract query parameters from URL and include
    // them in the signature base string. The base URL excludes the query string.
    let (base_url, query_params) = split_url_query(url);

    // Collect all params for signature (OAuth + query + extra)
    let mut params: Vec<(String, String)> = vec![
        ("oauth_consumer_key".to_string(), creds.consumer_key.clone()),
        ("oauth_nonce".to_string(), nonce.clone()),
        (
            "oauth_signature_method".to_string(),
            "HMAC-SHA1".to_string(),
        ),
        ("oauth_timestamp".to_string(), timestamp.clone()),
        ("oauth_token".to_string(), creds.token.clone()),
        ("oauth_version".to_string(), "1.0".to_string()),
    ];

    // Include query parameters from URL in signature
    for (k, v) in &query_params {
        params.push((k.clone(), v.clone()));
    }

    for (k, v) in extra_params {
        params.push((k.to_string(), v.to_string()));
    }

    let signature = sign(
        method,
        &base_url,
        &params,
        &creds.consumer_secret,
        &creds.token_secret,
    );

    // Build header (only oauth_* params + signature)
    format!(
        "OAuth oauth_consumer_key=\"{}\", oauth_nonce=\"{}\", oauth_signature=\"{}\", oauth_signature_method=\"HMAC-SHA1\", oauth_timestamp=\"{}\", oauth_token=\"{}\", oauth_version=\"1.0\"",
        percent_encode(&creds.consumer_key),
        percent_encode(&nonce),
        percent_encode(&signature),
        percent_encode(&timestamp),
        percent_encode(&creds.token),
    )
}

/// Build Authorization header for requests without an access token (request_token phase).
/// Only uses consumer credentials.
pub fn authorization_header_consumer_only(
    method: &str,
    url: &str,
    consumer_key: &str,
    consumer_secret: &str,
    extra_params: &[(&str, &str)],
) -> String {
    let nonce = generate_nonce();
    let timestamp = generate_timestamp();

    let mut params: Vec<(String, String)> = vec![
        ("oauth_consumer_key".to_string(), consumer_key.to_string()),
        ("oauth_nonce".to_string(), nonce.clone()),
        (
            "oauth_signature_method".to_string(),
            "HMAC-SHA1".to_string(),
        ),
        ("oauth_timestamp".to_string(), timestamp.clone()),
        ("oauth_version".to_string(), "1.0".to_string()),
    ];

    for (k, v) in extra_params {
        params.push((k.to_string(), v.to_string()));
    }

    // Extract query params from URL for signing
    let (base_url, query_params) = split_url_query(url);
    for (k, v) in &query_params {
        params.push((k.clone(), v.clone()));
    }

    // token_secret is empty for request_token
    let signature = sign(method, &base_url, &params, consumer_secret, "");

    format!(
        "OAuth oauth_consumer_key=\"{}\", oauth_nonce=\"{}\", oauth_signature=\"{}\", oauth_signature_method=\"HMAC-SHA1\", oauth_timestamp=\"{}\", oauth_version=\"1.0\"",
        percent_encode(consumer_key),
        percent_encode(&nonce),
        percent_encode(&signature),
        percent_encode(&timestamp),
    )
}

/// Step 1: Obtain a request token from the provider
///
/// `oauth_callback` is included in both the signature base string AND
/// sent as a POST body parameter so the server can find it.
pub async fn request_token(
    consumer_key: &str,
    consumer_secret: &str,
    request_token_url: &str,
    callback_url: &str,
) -> Result<(String, String), String> {
    // Include oauth_callback in signature AND send as POST body
    let extra = [("oauth_callback", callback_url)];
    let auth = authorization_header_consumer_only(
        "POST",
        request_token_url,
        consumer_key,
        consumer_secret,
        &extra,
    );

    let body = format!("oauth_callback={}", percent_encode(callback_url));

    let client = sign_in_client()?;
    let resp = client
        .post(request_token_url)
        .header("Authorization", &auth)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| sign_in_error("Request token", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| sign_in_error("Read body", e));
        return Err(format!("Request token failed ({}): {}", status, body));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| sign_in_error("Read body", e))?;
    parse_token_response(&body)
}

/// A sign-in step waits for the server as long as the GitHub sign-in: without
/// a limit, a server that accepts the connection and never answers holds the
/// sign-in dialog, or a CLI `profile-auth`, for ever.
fn sign_in_limit() -> std::time::Duration {
    super::http_limits::limit(super::http_limits::SIGN_IN_TIMEOUT)
}

/// The client of one sign-in step, bounded by [`sign_in_limit`].
fn sign_in_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(crate::providers::AEROFTP_USER_AGENT)
        .connect_timeout(sign_in_limit())
        .timeout(sign_in_limit())
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))
}

/// reqwest's `Display` for a timeout is the generic "error sending request",
/// so a step that ran out of time says so.
fn sign_in_error(step: &str, e: reqwest::Error) -> String {
    if e.is_timeout() {
        format!(
            "{step}: the server did not answer within {} s",
            sign_in_limit().as_secs()
        )
    } else {
        format!("{step} HTTP error: {e}")
    }
}

/// Step 2: Build the authorization URL for the user to visit
pub fn authorize_url(base_url: &str, request_token: &str) -> String {
    format!("{}?oauth_token={}", base_url, percent_encode(request_token))
}

/// Step 3: Exchange the request token + verifier for an access token.
/// `verifier` can be empty for OAuth 1.0 providers that don't use oauth_verifier (e.g. 4shared).
pub async fn access_token(
    consumer_key: &str,
    consumer_secret: &str,
    access_token_url: &str,
    request_token: &str,
    request_token_secret: &str,
    verifier: &str,
) -> Result<(String, String), String> {
    let creds = OAuth1Credentials {
        consumer_key: consumer_key.to_string(),
        consumer_secret: consumer_secret.to_string(),
        token: request_token.to_string(),
        token_secret: request_token_secret.to_string(),
    };

    // Only include oauth_verifier if non-empty (OAuth 1.0a has it, OAuth 1.0 does not)
    let extra_with_verifier = [("oauth_verifier", verifier)];
    let extra_empty: [(&str, &str); 0] = [];
    let auth = if verifier.is_empty() {
        authorization_header("POST", access_token_url, &creds, &extra_empty)
    } else {
        authorization_header("POST", access_token_url, &creds, &extra_with_verifier)
    };

    // The verifier was signed above but also has to reach the server.
    let auth = if verifier.is_empty() {
        auth
    } else {
        format!("{auth}, oauth_verifier=\"{}\"", percent_encode(verifier))
    };
    let client = sign_in_client()?;
    let resp = client
        .post(access_token_url)
        .header("Authorization", &auth)
        .send()
        .await
        .map_err(|e| sign_in_error("Access token", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| sign_in_error("Read body", e));
        return Err(format!("Access token failed ({}): {}", status, body));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| sign_in_error("Read body", e))?;
    parse_token_response(&body)
}

/// Parse `oauth_token=xxx&oauth_token_secret=yyy` response
fn parse_token_response(body: &str) -> Result<(String, String), String> {
    let pairs: std::collections::HashMap<&str, &str> = body
        .split('&')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            Some((parts.next()?, parts.next()?))
        })
        .collect();

    let token = pairs
        .get("oauth_token")
        .ok_or("Missing oauth_token in response")?
        .to_string();
    let secret = pairs
        .get("oauth_token_secret")
        .ok_or("Missing oauth_token_secret in response")?
        .to_string();

    Ok((token, secret))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 5849 section 1.2: the documented photos request and its signature,
    /// reproduced byte for byte (that example carries no `oauth_version`).
    #[test]
    fn sign_matches_the_rfc_5849_example() {
        let params: Vec<(String, String)> = [
            ("file", "vacation.jpg"),
            ("size", "original"),
            ("oauth_consumer_key", "dpf43f3p2l4k3l03"),
            ("oauth_token", "nnch734d00sl2jdk"),
            ("oauth_signature_method", "HMAC-SHA1"),
            ("oauth_timestamp", "137131202"),
            ("oauth_nonce", "chapoH"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert_eq!(
            sign(
                "GET",
                "http://photos.example.net/photos",
                &params,
                "kd94hf93k423kf44",
                "pfkkdhi9sl3r4s00",
            ),
            "MdpQcU8iPSUjWoN/UDMsK2sui9I="
        );
    }

    /// #1506 Requested 167, OAuth 1.0 side: query values taken from a URL are
    /// on the wire already encoded, so they are decoded before `sign` encodes
    /// them once. A value from the URL signs exactly like the same raw value
    /// passed as an extra parameter.
    #[test]
    fn split_url_query_decodes_so_a_query_value_is_signed_once() {
        let (base, params) =
            split_url_query("https://h.example/p?name=vacation%20photo%2B1%25.jpg&q=a+b&empty=");
        assert_eq!(base, "https://h.example/p");
        assert_eq!(
            params,
            vec![
                ("name".to_string(), "vacation photo+1%.jpg".to_string()),
                ("q".to_string(), "a b".to_string()),
                ("empty".to_string(), String::new()),
            ]
        );
        let raw: Vec<(String, String)> = vec![
            ("name".to_string(), "vacation photo+1%.jpg".to_string()),
            ("q".to_string(), "a b".to_string()),
            ("empty".to_string(), String::new()),
        ];
        assert_eq!(
            sign("GET", &base, &params, "cs", "ts"),
            sign("GET", &base, &raw, "cs", "ts")
        );
    }

    /// CodeRabbit on #1515, RFC 5849 3.4.1.3.2: pairs are sorted by ENCODED
    /// name (`%5B` before `A`, while the decoded `[` sorts after `A`), then by
    /// encoded value, and a repeated name keeps every pair (`x=1&%78=2` is two
    /// `x` pairs, which a map collapsed into one).
    #[test]
    fn sign_sorts_encoded_pairs_and_keeps_repeated_names() {
        let base = "https://h.example/p";
        let reference = |pairs: &str| {
            let base_string = format!("GET&{}&{}", percent_encode(base), percent_encode(pairs));
            let mut mac = HmacSha1::new_from_slice(b"cs&ts").unwrap();
            mac.update(base_string.as_bytes());
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
        };
        let (_, bracket) = split_url_query("https://h.example/p?A=1&%5B=2");
        assert_eq!(
            sign("GET", base, &bracket, "cs", "ts"),
            reference("%5B=2&A=1")
        );
        let (_, repeated) = split_url_query("https://h.example/p?x=2&%78=1");
        assert_eq!(repeated.len(), 2);
        assert_eq!(
            sign("GET", base, &repeated, "cs", "ts"),
            reference("x=1&x=2")
        );
    }

    #[test]
    fn test_percent_encode() {
        assert_eq!(percent_encode("hello world"), "hello%20world");
        assert_eq!(percent_encode("test@example.com"), "test%40example.com");
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn test_nonce_length() {
        let nonce = generate_nonce();
        assert_eq!(nonce.len(), 32);
        assert!(nonce.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn test_timestamp_is_numeric() {
        let ts = generate_timestamp();
        assert!(ts.parse::<u64>().is_ok());
    }

    /// Each sign-in step against a server that accepts the connection and
    /// never answers. The clock is paused, so a limit elapses at once and the
    /// outer bound only fires when the step has none.
    async fn wait_for_a_mute_server<F>(step: F) -> Result<(String, String), String>
    where
        F: std::future::Future<Output = Result<(String, String), String>>,
    {
        tokio::time::timeout(std::time::Duration::from_secs(3600), step)
            .await
            .expect("the sign-in step is still waiting for the server after an hour")
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_token_the_server_never_answers_ends_the_sign_in() {
        let url = crate::providers::mute_http_endpoint();
        let outcome = wait_for_a_mute_server(request_token("key", "secret", &url, "oob")).await;
        let error = outcome.expect_err("no answer");
        assert!(error.contains("did not answer within 30 s"), "{error}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_access_token_the_server_never_answers_ends_the_sign_in() {
        let url = crate::providers::mute_http_endpoint();
        let outcome = wait_for_a_mute_server(access_token(
            "key",
            "secret",
            &url,
            "token",
            "token-secret",
            "",
        ))
        .await;
        let error = outcome.expect_err("no answer");
        assert!(error.contains("did not answer within 30 s"), "{error}");
    }
}
