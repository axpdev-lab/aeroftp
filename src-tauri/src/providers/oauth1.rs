//! OAuth 1.0a Authentication Module (RFC 5849)
//!
//! Provides HMAC-SHA1 request signing for 4shared and other OAuth 1.0 APIs.
//! Zero new dependencies — uses hmac, sha1, base64, rand already in Cargo.toml.

use hmac::{Hmac, Mac};
use sha1::Sha1;
use base64::Engine;
use rand::Rng;
use std::collections::BTreeMap;

type HmacSha1 = Hmac<Sha1>;

/// OAuth 1.0 credentials (consumer + token pair)
#[derive(Debug, Clone)]
pub struct OAuth1Credentials {
    pub consumer_key: String,
    pub consumer_secret: String,
    pub token: String,
    pub token_secret: String,
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

/// Generate a random nonce (32 alphanumeric chars)
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
    params: &BTreeMap<String, String>,
    consumer_secret: &str,
    token_secret: &str,
) -> String {
    // 1. Parameter string (sorted, percent-encoded key=value pairs)
    let param_string: String = params
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
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
    let signing_key = format!("{}&{}", percent_encode(consumer_secret), percent_encode(token_secret));

    // 4. HMAC-SHA1
    let mut mac = HmacSha1::new_from_slice(signing_key.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(base_string.as_bytes());
    let result = mac.finalize().into_bytes();

    base64::engine::general_purpose::STANDARD.encode(result)
}

/// Build an OAuth 1.0 `Authorization` header for a signed request.
///
/// `extra_params` are additional query/body params that must be included
/// in the signature base string (but NOT in the Authorization header).
pub fn authorization_header(
    method: &str,
    url: &str,
    creds: &OAuth1Credentials,
    extra_params: &[(&str, &str)],
) -> String {
    let nonce = generate_nonce();
    let timestamp = generate_timestamp();

    // Collect all params for signature (OAuth + extra)
    let mut params = BTreeMap::new();
    params.insert("oauth_consumer_key".to_string(), creds.consumer_key.clone());
    params.insert("oauth_nonce".to_string(), nonce.clone());
    params.insert("oauth_signature_method".to_string(), "HMAC-SHA1".to_string());
    params.insert("oauth_timestamp".to_string(), timestamp.clone());
    params.insert("oauth_token".to_string(), creds.token.clone());
    params.insert("oauth_version".to_string(), "1.0".to_string());

    for (k, v) in extra_params {
        params.insert(k.to_string(), v.to_string());
    }

    let signature = sign(method, url, &params, &creds.consumer_secret, &creds.token_secret);

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

    let mut params = BTreeMap::new();
    params.insert("oauth_consumer_key".to_string(), consumer_key.to_string());
    params.insert("oauth_nonce".to_string(), nonce.clone());
    params.insert("oauth_signature_method".to_string(), "HMAC-SHA1".to_string());
    params.insert("oauth_timestamp".to_string(), timestamp.clone());
    params.insert("oauth_version".to_string(), "1.0".to_string());

    for (k, v) in extra_params {
        params.insert(k.to_string(), v.to_string());
    }

    // token_secret is empty for request_token
    let signature = sign(method, url, &params, consumer_secret, "");

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
    let auth = authorization_header_consumer_only("POST", request_token_url, consumer_key, consumer_secret, &extra);

    let body = format!("oauth_callback={}", percent_encode(callback_url));

    let client = reqwest::Client::new();
    let resp = client
        .post(request_token_url)
        .header("Authorization", &auth)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("Request token HTTP error: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Request token failed ({}): {}", status, body));
    }

    let body = resp.text().await.map_err(|e| format!("Read body error: {}", e))?;
    parse_token_response(&body)
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

    let client = reqwest::Client::new();
    let resp = client
        .post(access_token_url)
        .header("Authorization", &auth)
        .send()
        .await
        .map_err(|e| format!("Access token HTTP error: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Access token failed ({}): {}", status, body));
    }

    let body = resp.text().await.map_err(|e| format!("Read body error: {}", e))?;
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
}
