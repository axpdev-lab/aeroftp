//! S3 Storage Provider
//!
//! Implementation of the StorageProvider trait for Amazon S3 and S3-compatible storage.
//! Supports AWS S3, MinIO, Backblaze B2, DigitalOcean Spaces, Cloudflare R2, Wasabi, etc.
//!
//! This implementation uses reqwest with AWS Signature Version 4 for authentication,
//! avoiding the heavyweight aws-sdk-s3 dependency for better compile times and smaller binaries.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use quick_xml::events::Event;
use quick_xml::Reader;
use secrecy::ExposeSecret;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info};
use reqwest::{Client, Method, StatusCode};
use std::collections::HashMap;

use super::{
    StorageProvider, ProviderError, ProviderType, RemoteEntry, S3Config, FileVersion,
    sanitize_api_error,
};

/// S3 Storage Provider
pub struct S3Provider {
    config: S3Config,
    client: Client,
    current_prefix: String,
    connected: bool,
}

impl S3Provider {
    /// Create a new S3 provider with the given configuration
    pub fn new(config: S3Config) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("HTTP client init failed: {e}")))?;

        Ok(Self {
            config,
            client,
            current_prefix: String::new(),
            connected: false,
        })
    }
    
    /// Get the S3 endpoint URL
    fn endpoint(&self) -> String {
        if let Some(ref endpoint) = self.config.endpoint {
            endpoint.trim_end_matches('/').to_string()
        } else {
            format!("https://s3.{}.amazonaws.com", self.config.region)
        }
    }
    
    /// Build URL for S3 operations
    fn build_url(&self, key: &str) -> String {
        let endpoint = self.endpoint();
        let key = key.trim_start_matches('/');
        
        if self.config.path_style {
            // Path-style: https://endpoint/bucket/key
            if key.is_empty() {
                format!("{}/{}", endpoint, self.config.bucket)
            } else {
                format!("{}/{}/{}", endpoint, self.config.bucket, key)
            }
        } else {
            // Virtual-hosted style: https://bucket.endpoint/key
            let endpoint_without_scheme = endpoint
                .replace("https://", "")
                .replace("http://", "");
            let scheme = if endpoint.starts_with("http://") { "http" } else { "https" };
            
            if key.is_empty() {
                format!("{}://{}.{}", scheme, self.config.bucket, endpoint_without_scheme)
            } else {
                format!("{}://{}.{}/{}", scheme, self.config.bucket, endpoint_without_scheme, key)
            }
        }
    }

    fn is_filelu_s3_endpoint(&self) -> bool {
        self.config
            .endpoint
            .as_deref()
            .map(|ep| {
                let lower = ep.to_ascii_lowercase();
                lower.contains("s5lu.com") || lower.contains("filelu")
            })
            .unwrap_or(false)
    }

    async fn verify_copy_target_exists(&self, to: &str) -> Result<(), ProviderError> {
        let to_key = to.trim_start_matches('/');
        let mut last_status: Option<StatusCode> = None;

        for attempt in 0..5 {
            let response = self.s3_request(Method::HEAD, to_key, None, None).await?;
            let status = response.status();
            debug!(
                "S3 rename verify attempt {}: HEAD {} -> {}",
                attempt + 1,
                to_key,
                status
            );

            if status == StatusCode::OK {
                return Ok(());
            }

            // Some S3-compatible providers may return temporary/inconsistent HEAD results
            // immediately after CopyObject. Fall back to prefix listing and exact-key match.
            if matches!(status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN | StatusCode::METHOD_NOT_ALLOWED) {
                let listed_keys = self.list_keys_with_prefix(to_key).await?;
                debug!(
                    "S3 rename verify attempt {}: list prefix '{}' returned {} keys",
                    attempt + 1,
                    to_key,
                    listed_keys.len()
                );
                if listed_keys.iter().any(|k| k == to_key) {
                    debug!(
                        "S3 rename verify attempt {}: destination '{}' found via list",
                        attempt + 1,
                        to_key
                    );
                    return Ok(());
                }
            }

            last_status = Some(status);

            if !matches!(status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN | StatusCode::METHOD_NOT_ALLOWED) || attempt == 4 {
                break;
            }

            tokio::time::sleep(std::time::Duration::from_millis(75 * (1 << attempt))).await;
        }

        Err(ProviderError::ServerError(format!(
            "Copy verification failed: destination {} not readable after copy (status: {})",
            to,
            last_status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        )))
    }

    async fn rename_filelu_safe(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_key = from.trim_start_matches('/');
        let to_key = to.trim_start_matches('/');

        debug!("FileLu S3 safe rename start: {} -> {}", from_key, to_key);

        let source_response = self.s3_request(Method::GET, from_key, None, None).await?;
        let source_status = source_response.status();
        if source_status != StatusCode::OK {
            return Err(ProviderError::ServerError(format!(
                "FileLu safe rename read failed ({}): {}",
                source_status,
                from
            )));
        }

        let source_bytes = source_response
            .bytes()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let put_response = self
            .s3_request(Method::PUT, to_key, None, Some(source_bytes.to_vec()))
            .await?;
        let put_status = put_response.status();
        let put_body = put_response.text().await.unwrap_or_default();

        match put_status {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => {
                if put_body.to_ascii_lowercase().contains("<error>") {
                    let err_code = put_body
                        .split("<Code>").nth(1)
                        .and_then(|s| s.split("</Code>").next())
                        .unwrap_or("PutError");
                    let err_msg = put_body
                        .split("<Message>").nth(1)
                        .and_then(|s| s.split("</Message>").next())
                        .unwrap_or("S3 provider returned an error during put");
                    return Err(ProviderError::ServerError(format!(
                        "FileLu safe rename write failed ({}): {} - {}",
                        put_status,
                        sanitize_api_error(err_code),
                        sanitize_api_error(err_msg)
                    )));
                }
            }
            _ => {
                return Err(ProviderError::ServerError(format!(
                    "FileLu safe rename write failed ({}): {}",
                    put_status,
                    sanitize_api_error(&put_body)
                )));
            }
        }

        self.delete(from).await?;
        info!("Renamed file (FileLu safe path) {} to {}", from, to);
        Ok(())
    }
    
    /// Sign a request using AWS Signature Version 4
    /// This is a simplified implementation - for production, consider using aws-sigv4
    fn sign_request(
        &self,
        method: &str,
        url: &str,
        headers: &mut HashMap<String, String>,
        payload_hash: &str,
    ) -> Result<String, ProviderError> {
        use hmac::{Hmac, Mac};
        use sha2::{Sha256, Digest};
        
        type HmacSha256 = Hmac<Sha256>;
        
        let now: DateTime<Utc> = Utc::now();
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        
        headers.insert("x-amz-date".to_string(), amz_date.clone());
        headers.insert("x-amz-content-sha256".to_string(), payload_hash.to_string());
        
        // Parse URL to get host and path
        let parsed = url::Url::parse(url)
            .map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;
        
        let host = parsed.host_str().unwrap_or("");
        let path = parsed.path();
        
        // Query parameters must be sorted alphabetically for canonical request
        let canonical_query = {
            let mut params: Vec<(String, String)> = parsed.query_pairs()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            params.sort_by(|a, b| {
                // Sort by key first, then by value
                match a.0.cmp(&b.0) {
                    std::cmp::Ordering::Equal => a.1.cmp(&b.1),
                    other => other,
                }
            });
            params.iter()
                .map(|(k, v)| format!("{}={}", 
                    urlencoding::encode(k), 
                    urlencoding::encode(v)))
                .collect::<Vec<_>>()
                .join("&")
        };
        
        headers.insert("host".to_string(), host.to_string());
        
        // Create canonical request
        let mut signed_headers: Vec<&str> = headers.keys().map(|s| s.as_str()).collect();
        signed_headers.sort();
        let signed_headers_str = signed_headers.join(";");
        
        let mut canonical_headers = String::new();
        for header in &signed_headers {
            if let Some(value) = headers.get(*header) {
                canonical_headers.push_str(&format!("{}:{}\n", header.to_lowercase(), value.trim()));
            }
        }
        
        // URI-encode each path segment individually (H-10: SigV4 requires encoded segments)
        let canonical_path = if path.is_empty() || path == "/" {
            "/".to_string()
        } else {
            let encoded_segments: Vec<String> = path
                .split('/')
                .map(|segment| {
                    if segment.is_empty() {
                        String::new()
                    } else {
                        urlencoding::encode(segment).into_owned()
                    }
                })
                .collect();
            encoded_segments.join("/")
        };

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method,
            canonical_path,
            canonical_query,
            canonical_headers,
            signed_headers_str,
            payload_hash
        );
        
        let canonical_request_hash = {
            let mut hasher = Sha256::new();
            hasher.update(canonical_request.as_bytes());
            hex::encode(hasher.finalize())
        };
        
        // Create string to sign
        let credential_scope = format!("{}/{}/s3/aws4_request", date_stamp, self.config.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            amz_date,
            credential_scope,
            canonical_request_hash
        );
        
        // Calculate signature
        fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
            let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
            mac.update(data);
            mac.finalize().into_bytes().to_vec()
        }
        
        let k_date = hmac_sha256(
            format!("AWS4{}", self.config.secret_access_key.expose_secret()).as_bytes(),
            date_stamp.as_bytes()
        );
        let k_region = hmac_sha256(&k_date, self.config.region.as_bytes());
        let k_service = hmac_sha256(&k_region, b"s3");
        let k_signing = hmac_sha256(&k_service, b"aws4_request");
        let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

        // Create authorization header
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            self.config.access_key_id,
            credential_scope,
            signed_headers_str,
            signature
        );

        Ok(authorization)
    }
    
    /// Make a signed request to S3
    async fn s3_request(
        &self,
        method: Method,
        key: &str,
        query_params: Option<&[(&str, &str)]>,
        body: Option<Vec<u8>>,
    ) -> Result<reqwest::Response, ProviderError> {
        use sha2::{Sha256, Digest};
        use tracing::{debug, warn};
        
        let mut url = self.build_url(key);
        if let Some(params) = query_params {
            let query: String = params.iter()
                .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
                .collect::<Vec<_>>()
                .join("&");
            if !query.is_empty() {
                url = format!("{}?{}", url, query);
            }
        }
        
        debug!("S3 Request: {} {}", method, url);
        debug!("S3 Bucket: {}, Region: {}, Path-style: {}", 
               self.config.bucket, self.config.region, self.config.path_style);
        
        let payload = body.as_deref().unwrap_or(&[]);
        let payload_hash = {
            let mut hasher = Sha256::new();
            hasher.update(payload);
            hex::encode(hasher.finalize())
        };
        
        let mut headers = HashMap::new();
        let authorization = self.sign_request(method.as_str(), &url, &mut headers, &payload_hash)?;
        
        let mut request = self.client.request(method.clone(), &url);
        
        for (key, value) in headers.iter() {
            request = request.header(key, value);
        }
        request = request.header("Authorization", &authorization);
        
        // SEC-06: Redact sensitive headers before logging
        {
            let redacted: HashMap<&String, String> = headers.iter().map(|(k, v)| {
                let lower = k.to_lowercase();
                if lower == "authorization" || lower == "x-amz-security-token" {
                    (k, "[REDACTED]".to_string())
                } else {
                    (k, v.clone())
                }
            }).collect();
            debug!("S3 Headers: {:?}", redacted);
        }

        if let Some(body_data) = body {
            // Explicitly set Content-Length for empty bodies (required by some S3-compatible services like Backblaze B2)
            request = request.header("Content-Length", body_data.len().to_string());
            request = request.body(body_data);
        }

        // ERR-03: Use retry wrapper for transient errors (429, 500, 502, 503, 504)
        let built_request = request.build()
            .map_err(|e| ProviderError::NetworkError(format!("Failed to build request: {e}")))?;
        let response = super::send_with_retry(
            &self.client,
            built_request,
            &super::HttpRetryConfig::default(),
        ).await.map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        let status = response.status();
        if !status.is_success() {
            warn!("S3 Response Status: {} for {} {}", status, method, url);
        }
        
        Ok(response)
    }
    
    /// Parse S3 ListObjectsV2 XML response using quick-xml (M-11/M-12)
    fn parse_list_response(&self, xml_str: &str) -> Result<(Vec<RemoteEntry>, Option<String>), ProviderError> {
        let mut entries = Vec::new();

        debug!("Parsing S3 ListObjectsV2 XML response, {} bytes", xml_str.len());

        let mut reader = Reader::from_str(xml_str);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();

        // State machine for tracking current element context
        enum Context {
            None,
            CommonPrefixes,
            Contents,
        }
        let mut context = Context::None;
        let mut current_tag = String::new();

        // Fields for CommonPrefixes
        let mut cp_prefix: Option<String> = None;

        // Fields for Contents
        let mut c_key: Option<String> = None;
        let mut c_size: Option<String> = None;
        let mut c_modified: Option<String> = None;
        let mut c_etag: Option<String> = None;
        let mut c_storage_class: Option<String> = None;

        // Top-level field
        let mut top_next_token: Option<String> = None;
        let mut in_next_token = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    match tag_name.as_str() {
                        "CommonPrefixes" => {
                            context = Context::CommonPrefixes;
                            cp_prefix = None;
                        }
                        "Contents" => {
                            context = Context::Contents;
                            c_key = None;
                            c_size = None;
                            c_modified = None;
                            c_etag = None;
                            c_storage_class = None;
                        }
                        "NextContinuationToken" => {
                            in_next_token = true;
                        }
                        _ => {
                            current_tag = tag_name;
                        }
                    }
                }
                Ok(Event::Text(ref e)) => {
                    let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();

                    if text.is_empty() {
                        buf.clear();
                        continue;
                    }

                    if in_next_token {
                        top_next_token = Some(text.clone());
                    }

                    match context {
                        Context::CommonPrefixes => {
                            if current_tag == "Prefix" {
                                cp_prefix = Some(text);
                            }
                        }
                        Context::Contents => match current_tag.as_str() {
                            "Key" => c_key = Some(text),
                            "Size" => c_size = Some(text),
                            "LastModified" => c_modified = Some(text),
                            "ETag" => c_etag = Some(text),
                            "StorageClass" => c_storage_class = Some(text),
                            _ => {}
                        },
                        Context::None => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    match tag_name.as_str() {
                        "CommonPrefixes" => {
                            if let Some(ref full_prefix) = cp_prefix {
                                let name = full_prefix
                                    .trim_end_matches('/')
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or(full_prefix)
                                    .to_string();

                                if !name.is_empty() {
                                    entries.push(RemoteEntry::directory(
                                        name,
                                        format!("/{}", full_prefix.trim_end_matches('/')),
                                    ));
                                }
                            }
                            context = Context::None;
                        }
                        "Contents" => {
                            if let Some(ref key) = c_key {
                                // Skip directory markers
                                if !key.ends_with('/') {
                                    // Skip if key equals current prefix
                                    let dominated = key == &self.current_prefix
                                        || key.trim_start_matches('/') == self.current_prefix.trim_start_matches('/');
                                    if !dominated {
                                        let name = key.rsplit('/').next().unwrap_or(key).to_string();
                                        if !name.is_empty() {
                                            let size: u64 = c_size.as_ref()
                                                .and_then(|s| s.parse().ok())
                                                .unwrap_or(0);

                                            let etag = c_etag.as_ref()
                                                .map(|s| s.trim_matches('"').to_string());

                                            let mut metadata = HashMap::new();
                                            if let Some(etag) = etag {
                                                metadata.insert("etag".to_string(), etag);
                                            }
                                            if let Some(ref sc) = c_storage_class {
                                                metadata.insert("storage_class".to_string(), sc.clone());
                                            }

                                            entries.push(RemoteEntry {
                                                name,
                                                path: format!("/{}", key),
                                                is_dir: false,
                                                size,
                                                modified: c_modified.clone(),
                                                permissions: None,
                                                owner: None,
                                                group: None,
                                                is_symlink: false,
                                                link_target: None,
                                                mime_type: None,
                                                metadata,
                                            });
                                        }
                                    }
                                }
                            }
                            context = Context::None;
                        }
                        "NextContinuationToken" => {
                            in_next_token = false;
                        }
                        _ => {}
                    }
                    current_tag.clear();
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    return Err(ProviderError::ParseError(format!("XML parse error: {}", e)));
                }
                _ => {}
            }
            buf.clear();
        }

        Ok((entries, top_next_token))
    }

    /// Extract content from an XML tag using quick-xml (M-11/M-12)
    fn extract_xml_tag(&self, xml_str: &str, tag: &str) -> Option<String> {
        let mut reader = Reader::from_str(xml_str);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut inside_target = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let name = e.name();
                    let tag_name = String::from_utf8_lossy(name.as_ref());
                    if tag_name == tag {
                        inside_target = true;
                    }
                }
                Ok(Event::Text(ref e)) => {
                    if inside_target {
                        let trimmed = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                        if !trimmed.is_empty() {
                            return Some(trimmed);
                        }
                    }
                }
                Ok(Event::End(ref e)) => {
                    let name = e.name();
                    let tag_name = String::from_utf8_lossy(name.as_ref());
                    if tag_name == tag {
                        inside_target = false;
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        None
    }

    /// Minimum part size for multipart upload (5 MB)
    const MULTIPART_THRESHOLD: usize = 5 * 1024 * 1024;
    /// Part size for multipart upload chunks (5 MB)
    const MULTIPART_PART_SIZE: usize = 5 * 1024 * 1024;

    /// Initiate a multipart upload, returns the UploadId.
    /// Optionally sets Content-Type for the resulting object (UPLOAD-01).
    async fn create_multipart_upload(&self, key: &str, content_type: Option<&str>) -> Result<String, ProviderError> {
        // For multipart, Content-Type must be set on initiation, not on individual parts.
        // We build a custom request to include the header.
        let url = {
            let base = self.build_url(key);
            format!("{}?uploads=", base)
        };

        use sha2::{Sha256, Digest};
        let payload_hash = {
            let mut hasher = Sha256::new();
            hasher.update(b"");
            hex::encode(hasher.finalize())
        };

        let mut headers = HashMap::new();
        if let Some(ct) = content_type {
            headers.insert("content-type".to_string(), ct.to_string());
        }
        let authorization = self.sign_request("POST", &url, &mut headers, &payload_hash)?;

        let mut request = self.client.post(&url);
        for (k, v) in headers.iter() {
            request = request.header(k, v);
        }
        request = request.header("Authorization", &authorization);
        request = request.header("Content-Length", "0");

        let response = request.send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(ProviderError::TransferFailed(
                format!("CreateMultipartUpload failed ({}): {}", status, sanitize_api_error(&body)),
            ));
        }

        self.extract_xml_tag(&body, "UploadId")
            .ok_or_else(|| ProviderError::ParseError("Missing UploadId in response".to_string()))
    }

    /// Upload a single part, returns the ETag
    async fn upload_part(
        &self,
        key: &str,
        upload_id: &str,
        part_number: u32,
        data: Vec<u8>,
    ) -> Result<String, ProviderError> {
        let part_num_str = part_number.to_string();
        let params: &[(&str, &str)] = &[
            ("partNumber", &part_num_str),
            ("uploadId", upload_id),
        ];

        let response = self.s3_request(Method::PUT, key, Some(params), Some(data)).await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(
                format!("UploadPart {} failed ({}): {}", part_number, status, sanitize_api_error(&body)),
            ));
        }

        // ETag is in the response headers
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| ProviderError::ParseError("Missing ETag in UploadPart response".to_string()))?;

        Ok(etag)
    }

    /// Complete a multipart upload
    async fn complete_multipart_upload(
        &self,
        key: &str,
        upload_id: &str,
        parts: &[(u32, String)],
    ) -> Result<(), ProviderError> {
        // Build XML body
        let mut xml = String::from("<CompleteMultipartUpload>");
        for (part_number, etag) in parts {
            xml.push_str(&format!(
                "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>",
                part_number, etag,
            ));
        }
        xml.push_str("</CompleteMultipartUpload>");

        let response = self.s3_request(
            Method::POST,
            key,
            Some(&[("uploadId", upload_id)]),
            Some(xml.into_bytes()),
        ).await?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(ProviderError::TransferFailed(
                format!("CompleteMultipartUpload failed ({}): {}", status, sanitize_api_error(&body)),
            ));
        }

        // UPLOAD-07: AWS S3 can return HTTP 200 but include an <Error> in the XML body
        if body.contains("<Error>") {
            let error_msg = self.extract_xml_tag(&body, "Message")
                .or_else(|| self.extract_xml_tag(&body, "Code"))
                .unwrap_or_else(|| "Unknown error in CompleteMultipartUpload response".to_string());
            return Err(ProviderError::TransferFailed(
                format!("CompleteMultipartUpload 200-with-error: {}", sanitize_api_error(&error_msg)),
            ));
        }

        Ok(())
    }

    /// Upload a file using S3 multipart upload with streaming (no full-file buffering).
    /// UPLOAD-02: Reads chunks from disk instead of loading entire file into RAM.
    async fn upload_multipart_streaming(
        &self,
        key: &str,
        local_path: &str,
        total_size: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use tokio::io::AsyncReadExt;

        // UPLOAD-01: Detect MIME type from filename for multipart uploads
        let content_type = mime_guess::from_path(local_path)
            .first_or_octet_stream()
            .to_string();
        let upload_id = self.create_multipart_upload(key, Some(&content_type)).await?;
        let mut parts: Vec<(u32, String)> = Vec::new();
        let mut file = tokio::fs::File::open(local_path).await
            .map_err(ProviderError::IoError)?;
        let mut part_number = 1u32;
        let mut uploaded: u64 = 0;

        loop {
            let mut buf = vec![0u8; Self::MULTIPART_PART_SIZE];
            let mut filled = 0;
            // Read exactly MULTIPART_PART_SIZE bytes (or less at EOF)
            while filled < Self::MULTIPART_PART_SIZE {
                let n = file.read(&mut buf[filled..]).await
                    .map_err(|e| ProviderError::TransferFailed(format!("Read error: {e}")))?;
                if n == 0 {
                    break; // EOF
                }
                filled += n;
            }
            if filled == 0 {
                break; // No more data
            }
            buf.truncate(filled);

            match self.upload_part(key, &upload_id, part_number, buf).await {
                Ok(etag) => {
                    parts.push((part_number, etag));
                    uploaded += filled as u64;
                    if let Some(ref progress) = on_progress {
                        progress(uploaded, total_size);
                    }
                    part_number += 1;
                }
                Err(e) => {
                    let _ = self.abort_multipart_upload(key, &upload_id).await;
                    return Err(e);
                }
            }
        }

        self.complete_multipart_upload(key, &upload_id, &parts).await
    }

    /// Abort a multipart upload
    async fn abort_multipart_upload(
        &self,
        key: &str,
        upload_id: &str,
    ) -> Result<(), ProviderError> {
        let _ = self.s3_request(
            Method::DELETE,
            key,
            Some(&[("uploadId", upload_id)]),
            None,
        ).await;
        Ok(())
    }

    /// List all object keys under a given prefix (non-recursive, no delimiter).
    /// Used by rename (folder) and rmdir_recursive.
    /// Includes pagination via continuation-token (H-05).
    async fn list_keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>, ProviderError> {
        let mut all_keys = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut params: Vec<(&str, &str)> = vec![
                ("list-type", "2"),
                ("prefix", prefix),
                ("max-keys", "1000"),
            ];

            let token_str: String;
            if let Some(ref token) = continuation_token {
                token_str = token.clone();
                params.push(("continuation-token", &token_str));
            }

            let response = self.s3_request(
                Method::GET,
                "",
                Some(&params),
                None,
            ).await?;

            if response.status() != StatusCode::OK {
                return Err(ProviderError::ServerError("Failed to list objects by prefix".to_string()));
            }

            let xml_str = response.text().await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            // Parse keys and next token using quick-xml
            let mut reader = Reader::from_str(&xml_str);
            reader.config_mut().trim_text(true);
            let mut buf = Vec::new();
            let mut inside_key = false;
            let mut inside_next_token = false;
            let mut next_token: Option<String> = None;

            loop {
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(ref e)) => {
                        let name = e.name();
                        let tag = String::from_utf8_lossy(name.as_ref());
                        match tag.as_ref() {
                            "Key" => inside_key = true,
                            "NextContinuationToken" => inside_next_token = true,
                            _ => {}
                        }
                    }
                    Ok(Event::Text(ref e)) => {
                        let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                        if !text.is_empty() {
                            if inside_key {
                                all_keys.push(text);
                            } else if inside_next_token {
                                next_token = Some(text);
                            }
                        }
                    }
                    Ok(Event::End(ref e)) => {
                        let name = e.name();
                        let tag = String::from_utf8_lossy(name.as_ref());
                        match tag.as_ref() {
                            "Key" => inside_key = false,
                            "NextContinuationToken" => inside_next_token = false,
                            _ => {}
                        }
                    }
                    Ok(Event::Eof) => break,
                    Err(e) => {
                        return Err(ProviderError::ParseError(format!("XML parse error: {}", e)));
                    }
                    _ => {}
                }
                buf.clear();
            }

            if let Some(token) = next_token {
                continuation_token = Some(token);
            } else {
                break;
            }
        }

        Ok(all_keys)
    }
}

#[async_trait]
impl StorageProvider for S3Provider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }

    fn provider_type(&self) -> ProviderType {
        ProviderType::S3
    }
    
    fn display_name(&self) -> String {
        if self.config.endpoint.is_some() {
            format!("s3://{} (custom)", self.config.bucket)
        } else {
            format!("s3://{} ({})", self.config.bucket, self.config.region)
        }
    }
    
    async fn connect(&mut self) -> Result<(), ProviderError> {
        // Test connection by listing bucket with max-keys=0
        let response = self.s3_request(
            Method::GET,
            "",
            Some(&[("list-type", "2"), ("max-keys", "1")]),
            None,
        ).await?;
        
        match response.status() {
            StatusCode::OK => {
                self.connected = true;
                
                // Set initial prefix if configured
                if let Some(ref prefix) = self.config.prefix {
                    self.current_prefix = prefix.trim_matches('/').to_string();
                }
                
                Ok(())
            }
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => {
                let body = response.text().await.unwrap_or_default();
                // Extract error message from XML if present
                let error_msg = if body.contains("<Message>") {
                    body.split("<Message>").nth(1)
                        .and_then(|s| s.split("</Message>").next())
                        .unwrap_or("Access denied")
                        .to_string()
                } else {
                    body.clone()
                };
                Err(ProviderError::AuthenticationFailed(format!("S3 auth error: {}", sanitize_api_error(&error_msg))))
            }
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(format!("Bucket '{}' not found", self.config.bucket)))
            }
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::ConnectionFailed(format!("S3 error ({}): {}", status, sanitize_api_error(&body))))
            }
        }
    }
    
    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        Ok(())
    }
    
    fn is_connected(&self) -> bool {
        self.connected
    }
    
    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let prefix = if path.is_empty() || path == "/" || path == "." {
            self.current_prefix.clone()
        } else {
            path.trim_matches('/').to_string()
        };
        
        let prefix_with_slash = if prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", prefix)
        };
        
        let mut all_entries = Vec::new();
        let mut continuation_token: Option<String> = None;

        // LIST-01: Pagination loop handles >1000 items via NextContinuationToken
        loop {
            let mut params: Vec<(&str, &str)> = vec![
                ("list-type", "2"),
                ("delimiter", "/"),
                ("max-keys", "1000"),
            ];
            
            if !prefix_with_slash.is_empty() {
                params.push(("prefix", &prefix_with_slash));
            }
            
            let token_str: String;
            if let Some(ref token) = continuation_token {
                token_str = token.clone();
                params.push(("continuation-token", &token_str));
            }
            
            let response = self.s3_request(Method::GET, "", Some(&params), None).await?;
            
            match response.status() {
                StatusCode::OK => {
                    let xml = response.text().await
                        .map_err(|e| ProviderError::ParseError(e.to_string()))?;
                    
                    // Debug: Log raw XML response (truncated for readability)
                    let xml_preview = if xml.len() > 2000 {
                        format!("{}... [truncated, total {} bytes]", &xml[..2000], xml.len())
                    } else {
                        xml.clone()
                    };
                    debug!("S3 LIST response XML:\n{}", xml_preview);
                    
                    let (entries, next_token) = self.parse_list_response(&xml)?;
                    info!("S3 LIST parsed {} entries from response", entries.len());
                    all_entries.extend(entries);
                    
                    if let Some(token) = next_token {
                        continuation_token = Some(token);
                    } else {
                        break;
                    }
                }
                status => {
                    let body = response.text().await.unwrap_or_default();
                    // Extract error message from XML if present
                    let error_msg = if body.contains("<Message>") {
                        body.split("<Message>").nth(1)
                            .and_then(|s| s.split("</Message>").next())
                            .unwrap_or(&body)
                            .to_string()
                    } else if body.contains("<Code>") {
                        // Try to get the error code
                        body.split("<Code>").nth(1)
                            .and_then(|s| s.split("</Code>").next())
                            .unwrap_or(&body)
                            .to_string()
                    } else {
                        body
                    };
                    return Err(ProviderError::ServerError(format!("List failed ({}): {}", status, sanitize_api_error(&error_msg))));
                }
            }
        }
        
        Ok(all_entries)
    }
    
    async fn pwd(&mut self) -> Result<String, ProviderError> {
        if self.current_prefix.is_empty() {
            Ok("/".to_string())
        } else {
            Ok(format!("/{}", self.current_prefix))
        }
    }
    
    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let new_prefix = if path == "/" || path.is_empty() {
            String::new()
        } else if path == ".." {
            // Go up one level
            let parts: Vec<&str> = self.current_prefix.split('/').collect();
            if parts.len() > 1 {
                parts[..parts.len()-1].join("/")
            } else {
                String::new()
            }
        } else if path.starts_with('/') || self.current_prefix.is_empty() {
            path.trim_matches('/').to_string()
        } else {
            format!("{}/{}", self.current_prefix, path.trim_matches('/'))
        };
        
        // Verify the prefix exists by listing it
        let prefix_check = if new_prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", new_prefix)
        };
        
        let response = self.s3_request(
            Method::GET,
            "",
            Some(&[
                ("list-type", "2"),
                ("prefix", &prefix_check),
                ("max-keys", "1"),
            ]),
            None,
        ).await?;
        
        if response.status() == StatusCode::OK {
            self.current_prefix = new_prefix;
            Ok(())
        } else {
            Err(ProviderError::NotFound(path.to_string()))
        }
    }
    
    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }
    
    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let key = remote_path.trim_start_matches('/');
        // DL-01: Retry handled by s3_request → send_with_retry (429, 5xx)
        let response = self.s3_request(Method::GET, key, None, None).await?;

        match response.status() {
            StatusCode::OK => {
                let total_size = response.content_length().unwrap_or(0);

                // H-01: Streaming download — write chunks as they arrive
                let mut stream = response.bytes_stream();
                let mut file = tokio::fs::File::create(local_path).await
                    .map_err(ProviderError::IoError)?;
                let mut downloaded: u64 = 0;

                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                    file.write_all(&chunk).await
                        .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                    downloaded += chunk.len() as u64;
                    if let Some(ref progress) = on_progress {
                        progress(downloaded, total_size);
                    }
                }
                file.flush().await
                    .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

                Ok(())
            }
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(remote_path.to_string()))
            }
            status => {
                Err(ProviderError::TransferFailed(format!("Download failed with status: {}", status)))
            }
        }
    }
    
    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let key = remote_path.trim_start_matches('/');
        let response = self.s3_request(Method::GET, key, None, None).await?;

        match response.status() {
            StatusCode::OK => {
                // H2: Size-limited download to prevent OOM on large files
                super::response_bytes_with_limit(response, super::MAX_DOWNLOAD_TO_BYTES).await
            }
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(remote_path.to_string()))
            }
            status => {
                Err(ProviderError::TransferFailed(format!("Download failed with status: {}", status)))
            }
        }
    }
    
    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let file_meta = tokio::fs::metadata(local_path).await
            .map_err(ProviderError::IoError)?;
        let total_size = file_meta.len();
        let key = remote_path.trim_start_matches('/');

        // UPLOAD-02: Use streaming multipart upload for files larger than 5MB
        // Reads chunks from disk instead of buffering entire file in RAM
        if total_size > Self::MULTIPART_THRESHOLD as u64 {
            return self.upload_multipart_streaming(key, local_path, total_size, on_progress).await;
        }

        // Streaming upload for small files (< 5MB)
        use tokio_util::io::ReaderStream;
        let file = tokio::fs::File::open(local_path).await
            .map_err(ProviderError::IoError)?;
        let stream = ReaderStream::new(file);
        let body = reqwest::Body::wrap_stream(stream);

        // Build the request manually with streaming body (cannot use s3_request helper for streaming)
        let url = self.build_url(key);
        // For streaming, we use UNSIGNED-PAYLOAD since we cannot hash the stream upfront
        let payload_hash = "UNSIGNED-PAYLOAD";
        let mut headers = HashMap::new();
        let authorization = self.sign_request("PUT", &url, &mut headers, payload_hash)?;

        // UPLOAD-01: Detect MIME type from filename extension
        let content_type = mime_guess::from_path(local_path)
            .first_or_octet_stream()
            .to_string();

        let mut request = self.client.put(&url);
        for (k, v) in headers.iter() {
            request = request.header(k, v);
        }
        request = request.header("Authorization", &authorization);
        request = request.header("Content-Length", total_size.to_string());
        request = request.header("Content-Type", &content_type);
        request = request.body(body);

        let response = request.send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => {
                if let Some(progress) = on_progress {
                    progress(total_size, total_size);
                }
                Ok(())
            }
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::TransferFailed(format!("Upload failed ({}): {}", status, sanitize_api_error(&body))))
            }
        }
    }
    
    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        // S3 doesn't have real directories, but we can create a zero-byte object with trailing /
        let key = format!("{}/", path.trim_matches('/'));
        
        let response = self.s3_request(Method::PUT, &key, None, Some(Vec::new())).await?;
        
        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
            status => {
                Err(ProviderError::ServerError(format!("mkdir failed with status: {}", status)))
            }
        }
    }
    
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let key = path.trim_start_matches('/');
        let response = self.s3_request(Method::DELETE, key, None, None).await?;
        
        match response.status() {
            StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::ACCEPTED => Ok(()),
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(path.to_string()))
            }
            status => {
                Err(ProviderError::ServerError(format!("Delete failed with status: {}", status)))
            }
        }
    }
    
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // For S3, delete the directory marker
        let key = format!("{}/", path.trim_matches('/'));
        self.delete(&key).await
    }
    
    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let prefix = format!("{}/", path.trim_matches('/'));
        let keys = self.list_keys_with_prefix(&prefix).await?;

        // DELETE-01: Use S3 batch delete (POST /?delete) for up to 1000 keys per request
        for chunk in keys.chunks(1000) {
            let mut xml = String::from("<Delete><Quiet>true</Quiet>");
            for key in chunk {
                xml.push_str(&format!(
                    "<Object><Key>{}</Key></Object>",
                    quick_xml::escape::escape(key)
                ));
            }
            xml.push_str("</Delete>");

            let xml_bytes = xml.into_bytes();

            // S3 batch delete requires Content-MD5
            let md5_digest = {
                use md5::{Md5, Digest};
                use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
                let mut hasher = Md5::new();
                hasher.update(&xml_bytes);
                BASE64.encode(hasher.finalize())
            };

            // Build signed request manually (need custom Content-MD5 header)
            let url = format!("{}?delete", self.build_url(""));
            let payload_hash = {
                use sha2::{Sha256, Digest};
                let mut hasher = Sha256::new();
                hasher.update(&xml_bytes);
                hex::encode(hasher.finalize())
            };

            let mut headers = HashMap::new();
            headers.insert("content-md5".to_string(), md5_digest);
            let authorization = self.sign_request("POST", &url, &mut headers, &payload_hash)?;

            let mut request = self.client.post(&url);
            for (k, v) in headers.iter() {
                request = request.header(k, v);
            }
            request = request.header("Authorization", &authorization);
            request = request.header("Content-Length", xml_bytes.len().to_string());
            request = request.body(xml_bytes);

            let response = request.send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !response.status().is_success() {
                // Fall back to sequential delete if batch fails
                tracing::warn!("S3 batch delete failed ({}), falling back to sequential", response.status());
                for key in chunk {
                    let _ = self.s3_request(Method::DELETE, key, None, None).await;
                }
            }
        }

        Ok(())
    }
    
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let from_trimmed = from.trim_matches('/');
        let to_trimmed = to.trim_matches('/');
        let prefix = format!("{}/", from_trimmed);

        // Check if this is a directory by listing objects under the prefix
        let keys = self.list_keys_with_prefix(&prefix).await?;

        if keys.is_empty() {
            if self.is_filelu_s3_endpoint() {
                return self.rename_filelu_safe(from, to).await;
            }

            // Single file rename: copy + delete
            self.server_copy(from, to).await?;
            self.verify_copy_target_exists(to).await?;
            self.delete(from).await?;
            info!("Renamed file (copy+delete) {} to {}", from, to);
        } else {
            // Directory rename: copy all objects to new prefix, then delete originals
            let to_prefix = format!("{}/", to_trimmed);

            for old_key in &keys {
                let new_key = old_key.replacen(&prefix, &to_prefix, 1);
                self.server_copy(
                    &format!("/{}", old_key),
                    &format!("/{}", new_key),
                ).await?;
            }

            // Delete all original objects
            for old_key in &keys {
                let _ = self.s3_request(Method::DELETE, old_key, None, None).await;
            }

            // Also try to delete the old directory marker (if exists)
            let _ = self.s3_request(Method::DELETE, &prefix, None, None).await;

            info!("Renamed directory (copy+delete {} objects) {} to {}", keys.len(), from, to);
        }

        Ok(())
    }
    
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let key = path.trim_start_matches('/');
        
        // Use HEAD request to get object metadata
        let response = self.s3_request(Method::HEAD, key, None, None).await?;
        
        match response.status() {
            StatusCode::OK => {
                let size = response
                    .headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                
                let modified = response
                    .headers()
                    .get("last-modified")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                
                let content_type = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                
                let etag = response
                    .headers()
                    .get("etag")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.trim_matches('"').to_string());
                
                let name = key.rsplit('/').next().unwrap_or(key).to_string();
                let is_dir = key.ends_with('/') && size == 0;
                
                let mut metadata = HashMap::new();
                if let Some(etag) = etag {
                    metadata.insert("etag".to_string(), etag);
                }
                
                Ok(RemoteEntry {
                    name,
                    path: format!("/{}", key),
                    is_dir,
                    size,
                    modified,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: content_type,
                    metadata,
                })
            }
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(path.to_string()))
            }
            status => {
                Err(ProviderError::ServerError(format!("HEAD failed with status: {}", status)))
            }
        }
    }
    
    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let entry = self.stat(path).await?;
        Ok(entry.size)
    }
    
    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }
    
    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        // S3 is stateless, just verify credentials still work
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let response = self.s3_request(
            Method::GET,
            "",
            Some(&[("list-type", "2"), ("max-keys", "0")]),
            None,
        ).await?;
        
        if response.status() == StatusCode::FORBIDDEN {
            self.connected = false;
            return Err(ProviderError::AuthenticationFailed("Credentials expired".to_string()));
        }
        
        Ok(())
    }
    
    async fn server_info(&mut self) -> Result<String, ProviderError> {
        let endpoint = if let Some(ref ep) = self.config.endpoint {
            ep.clone()
        } else {
            format!("AWS S3 ({})", self.config.region)
        };
        
        Ok(format!("S3 Storage: {} - Bucket: {}", endpoint, self.config.bucket))
    }
    
    // QUOTA-01: S3 buckets have no inherent storage quota. AWS S3 provides unlimited storage
    // with pay-per-use pricing. There is no API to query "used/total" space for a bucket.
    // CloudWatch metrics (BucketSizeBytes) are delayed by ~24h and require separate permissions.
    // Returning NotSupported is the correct behavior for S3.

    fn supports_share_links(&self) -> bool {
        true
    }
    
    async fn create_share_link(
        &mut self,
        path: &str,
        expires_in_secs: Option<u64>,
    ) -> Result<String, ProviderError> {
        // Generate a presigned URL
        use sha2::{Sha256, Digest};
        use hmac::{Hmac, Mac};

        type HmacSha256 = Hmac<Sha256>;

        let key = path.trim_start_matches('/');
        let expires = expires_in_secs.unwrap_or(3600); // Default 1 hour

        let now: DateTime<Utc> = Utc::now();
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

        let credential_scope = format!("{}/{}/s3/aws4_request", date_stamp, self.config.region);
        let credential = format!("{}/{}", self.config.access_key_id, credential_scope);

        let url = self.build_url(key);
        let parsed = url::Url::parse(&url)
            .map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;

        let host = parsed.host_str().unwrap_or("");
        let raw_path = parsed.path();

        // M-13: URI-encode each path segment for the canonical URI
        let canonical_path = if raw_path.is_empty() || raw_path == "/" {
            "/".to_string()
        } else {
            let encoded_segments: Vec<String> = raw_path
                .split('/')
                .map(|segment| {
                    if segment.is_empty() {
                        String::new()
                    } else {
                        urlencoding::encode(segment).into_owned()
                    }
                })
                .collect();
            encoded_segments.join("/")
        };

        // Build canonical query string
        let signed_headers = "host";
        let query_params = format!(
            "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential={}&X-Amz-Date={}&X-Amz-Expires={}&X-Amz-SignedHeaders={}",
            urlencoding::encode(&credential),
            amz_date,
            expires,
            signed_headers
        );

        // Canonical request
        let canonical_request = format!(
            "GET\n{}\n{}\nhost:{}\n\n{}\nUNSIGNED-PAYLOAD",
            canonical_path,
            query_params,
            host,
            signed_headers
        );

        let canonical_hash = {
            let mut hasher = Sha256::new();
            hasher.update(canonical_request.as_bytes());
            hex::encode(hasher.finalize())
        };

        // String to sign
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            amz_date,
            credential_scope,
            canonical_hash
        );

        // Calculate signature
        fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
            let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
            mac.update(data);
            mac.finalize().into_bytes().to_vec()
        }

        let k_date = hmac_sha256(
            format!("AWS4{}", self.config.secret_access_key.expose_secret()).as_bytes(),
            date_stamp.as_bytes()
        );
        let k_region = hmac_sha256(&k_date, self.config.region.as_bytes());
        let k_service = hmac_sha256(&k_region, b"s3");
        let k_signing = hmac_sha256(&k_service, b"aws4_request");
        let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

        Ok(format!("{}?{}&X-Amz-Signature={}", url, query_params, signature))
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let prefix = path.trim_matches('/');
        let prefix_with_slash = if prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", prefix)
        };

        // M1: Cap search results to prevent unbounded memory growth on large buckets.
        // S3 buckets can contain millions of objects; without a cap, a broad pattern
        // could return all of them, causing OOM.
        const MAX_SEARCH_RESULTS: usize = 10_000;

        // Use ListObjectsV2 with prefix (no delimiter to get all recursive objects)
        let mut all_entries = Vec::new();
        let mut continuation_token: Option<String> = None;
        let pattern_lower = pattern.to_lowercase();

        loop {
            let mut params: Vec<(&str, &str)> = vec![
                ("list-type", "2"),
                ("max-keys", "1000"),
            ];

            if !prefix_with_slash.is_empty() {
                params.push(("prefix", &prefix_with_slash));
            }

            let token_str: String;
            if let Some(ref token) = continuation_token {
                token_str = token.clone();
                params.push(("continuation-token", &token_str));
            }

            let response = self.s3_request(Method::GET, "", Some(&params), None).await?;

            if response.status() != StatusCode::OK {
                let body = response.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!("Search failed: {}", sanitize_api_error(&body))));
            }

            let xml_str = response.text().await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            // Parse keys, sizes, and filter by pattern using quick-xml
            let mut find_reader = Reader::from_str(&xml_str);
            find_reader.config_mut().trim_text(true);
            let mut find_buf = Vec::new();
            let mut in_contents = false;
            let mut in_next_tok = false;
            let mut find_tag = String::new();
            let mut find_key: Option<String> = None;
            let mut find_size: Option<String> = None;
            let mut find_modified: Option<String> = None;
            let mut next_tok_val: Option<String> = None;

            loop {
                match find_reader.read_event_into(&mut find_buf) {
                    Ok(Event::Start(ref e)) => {
                        let tn = String::from_utf8_lossy(e.name().as_ref()).to_string();
                        match tn.as_str() {
                            "Contents" => {
                                in_contents = true;
                                find_key = None;
                                find_size = None;
                                find_modified = None;
                            }
                            "NextContinuationToken" => in_next_tok = true,
                            _ => find_tag = tn,
                        }
                    }
                    Ok(Event::Text(ref e)) => {
                        let t = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                        if !t.is_empty() {
                            if in_next_tok {
                                next_tok_val = Some(t.clone());
                            }
                            if in_contents {
                                match find_tag.as_str() {
                                    "Key" => find_key = Some(t),
                                    "Size" => find_size = Some(t),
                                    "LastModified" => find_modified = Some(t),
                                    _ => {}
                                }
                            }
                        }
                    }
                    Ok(Event::End(ref e)) => {
                        let tn = String::from_utf8_lossy(e.name().as_ref()).to_string();
                        match tn.as_str() {
                            "Contents" => {
                                if let Some(ref key) = find_key {
                                    if !key.ends_with('/') {
                                        let name = key.rsplit('/').next().unwrap_or(key);
                                        if name.to_lowercase().contains(&pattern_lower) {
                                            let size: u64 = find_size.as_ref()
                                                .and_then(|s| s.parse().ok())
                                                .unwrap_or(0);
                                            all_entries.push(RemoteEntry {
                                                name: name.to_string(),
                                                path: format!("/{}", key),
                                                is_dir: false,
                                                size,
                                                modified: find_modified.clone(),
                                                permissions: None,
                                                owner: None,
                                                group: None,
                                                is_symlink: false,
                                                link_target: None,
                                                mime_type: None,
                                                metadata: HashMap::new(),
                                            });
                                        }
                                    }
                                }
                                in_contents = false;
                            }
                            "NextContinuationToken" => in_next_tok = false,
                            _ => {}
                        }
                        find_tag.clear();
                    }
                    Ok(Event::Eof) => break,
                    Err(e) => {
                        return Err(ProviderError::ParseError(format!("XML parse error: {}", e)));
                    }
                    _ => {}
                }
                find_buf.clear();
            }

            // M1: Stop paginating once we've collected enough results
            if all_entries.len() >= MAX_SEARCH_RESULTS {
                info!("S3 find: reached {} result cap, stopping pagination", MAX_SEARCH_RESULTS);
                break;
            }

            match next_tok_val {
                Some(token) => continuation_token = Some(token),
                None => break,
            }
        }

        Ok(all_entries)
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let from_key = from.trim_start_matches('/');
        let to_key = to.trim_start_matches('/');
        let copy_source = format!("/{}/{}", self.config.bucket, from_key);

        let url = self.build_url(to_key);

        use sha2::{Sha256, Digest};
        let payload_hash = {
            let mut hasher = Sha256::new();
            hasher.update(b"");
            hex::encode(hasher.finalize())
        };

        let mut headers = HashMap::new();
        headers.insert("x-amz-copy-source".to_string(), copy_source);
        // COPY-01: Preserve original object metadata during copy
        headers.insert("x-amz-metadata-directive".to_string(), "COPY".to_string());
        let authorization = self.sign_request("PUT", &url, &mut headers, &payload_hash)?;

        let mut request = self.client.put(&url);
        for (key, value) in headers.iter() {
            request = request.header(key, value);
        }
        request = request.header("Authorization", &authorization);
        request = request.header("Content-Length", "0");

        let response = request.send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        match status {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => {
                // S3-compatible providers may return HTTP 200 with an XML <Error> payload.
                // Treat this as a failed copy to avoid deleting the source during rename.
                if body.to_ascii_lowercase().contains("<error>") {
                    let err_code = body
                        .split("<Code>").nth(1)
                        .and_then(|s| s.split("</Code>").next())
                        .unwrap_or("CopyError");
                    let err_msg = body
                        .split("<Message>").nth(1)
                        .and_then(|s| s.split("</Message>").next())
                        .unwrap_or("S3 provider returned an error during copy");
                    return Err(ProviderError::ServerError(format!(
                        "Copy failed ({}): {} - {}",
                        status,
                        sanitize_api_error(err_code),
                        sanitize_api_error(err_msg)
                    )));
                }

                info!("Copied {} to {}", from, to);
                Ok(())
            }
            _ => {
                Err(ProviderError::ServerError(format!("Copy failed ({}): {}", status, sanitize_api_error(&body))))
            }
        }
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_multipart: true,
            multipart_threshold: Self::MULTIPART_THRESHOLD as u64,
            multipart_part_size: Self::MULTIPART_PART_SIZE as u64,
            multipart_max_parallel: 4,
            supports_server_checksum: true,
            preferred_checksum_algo: Some("ETag".to_string()),
            ..Default::default()
        }
    }

    fn supports_versions(&self) -> bool {
        true
    }

    async fn list_versions(&mut self, path: &str) -> Result<Vec<FileVersion>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let key = path.trim_start_matches('/');
        let mut all_versions = Vec::new();
        let mut key_marker: Option<String> = None;
        let mut version_id_marker: Option<String> = None;

        loop {
            let mut params: Vec<(&str, &str)> = vec![
                ("versions", ""),
                ("prefix", key),
            ];

            let km_str: String;
            let vm_str: String;
            if let Some(ref km) = key_marker {
                km_str = km.clone();
                params.push(("key-marker", &km_str));
            }
            if let Some(ref vm) = version_id_marker {
                vm_str = vm.clone();
                params.push(("version-id-marker", &vm_str));
            }

            let response = self.s3_request(Method::GET, "", Some(&params), None).await?;

            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(
                    format!("ListObjectVersions failed ({}): {}", status, sanitize_api_error(&body)),
                ));
            }

            let xml_str = response.text().await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            debug!("S3 ListObjectVersions response, {} bytes", xml_str.len());

            // Parse ListVersionsResult XML using quick-xml
            let mut reader = Reader::from_str(&xml_str);
            reader.config_mut().trim_text(true);
            let mut buf = Vec::new();

            let mut in_version = false;
            let mut _in_delete_marker = false;
            let mut current_tag = String::new();

            // Fields for <Version> elements
            let mut v_key: Option<String> = None;
            let mut v_version_id: Option<String> = None;
            let mut v_is_latest: Option<String> = None;
            let mut v_last_modified: Option<String> = None;
            let mut v_size: Option<String> = None;

            // Pagination fields
            let mut is_truncated = false;
            let mut next_key_marker: Option<String> = None;
            let mut next_version_id_marker: Option<String> = None;
            let mut in_is_truncated = false;
            let mut in_next_key_marker = false;
            let mut in_next_version_id_marker = false;

            loop {
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(ref e)) => {
                        let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                        match tag_name.as_str() {
                            "Version" => {
                                in_version = true;
                                v_key = None;
                                v_version_id = None;
                                v_is_latest = None;
                                v_last_modified = None;
                                v_size = None;
                            }
                            "DeleteMarker" => {
                                _in_delete_marker = true;
                            }
                            "IsTruncated" => in_is_truncated = true,
                            "NextKeyMarker" => in_next_key_marker = true,
                            "NextVersionIdMarker" => in_next_version_id_marker = true,
                            _ => {
                                current_tag = tag_name;
                            }
                        }
                    }
                    Ok(Event::Text(ref e)) => {
                        let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                        if text.is_empty() {
                            buf.clear();
                            continue;
                        }

                        if in_is_truncated {
                            is_truncated = text == "true";
                        }
                        if in_next_key_marker {
                            next_key_marker = Some(text.clone());
                        }
                        if in_next_version_id_marker {
                            next_version_id_marker = Some(text.clone());
                        }

                        if in_version {
                            match current_tag.as_str() {
                                "Key" => v_key = Some(text),
                                "VersionId" => v_version_id = Some(text),
                                "IsLatest" => v_is_latest = Some(text),
                                "LastModified" => v_last_modified = Some(text),
                                "Size" => v_size = Some(text),
                                _ => {}
                            }
                        }
                    }
                    Ok(Event::End(ref e)) => {
                        let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                        match tag_name.as_str() {
                            "Version" => {
                                // Only include versions whose key exactly matches
                                if let Some(ref vk) = v_key {
                                    if vk == key {
                                        let version_id = v_version_id.clone().unwrap_or_default();
                                        let is_latest = v_is_latest.as_deref() == Some("true");
                                        let size: u64 = v_size.as_ref()
                                            .and_then(|s| s.parse().ok())
                                            .unwrap_or(0);

                                        let mut modified_by_str = None;
                                        if is_latest {
                                            modified_by_str = Some("(latest)".to_string());
                                        }

                                        all_versions.push(FileVersion {
                                            id: version_id,
                                            modified: v_last_modified.clone(),
                                            size,
                                            modified_by: modified_by_str,
                                        });
                                    }
                                }
                                in_version = false;
                            }
                            "DeleteMarker" => {
                                _in_delete_marker = false;
                            }
                            "IsTruncated" => in_is_truncated = false,
                            "NextKeyMarker" => in_next_key_marker = false,
                            "NextVersionIdMarker" => in_next_version_id_marker = false,
                            _ => {}
                        }
                        current_tag.clear();
                    }
                    Ok(Event::Eof) => break,
                    Err(e) => {
                        return Err(ProviderError::ParseError(format!("XML parse error: {}", e)));
                    }
                    _ => {}
                }
                buf.clear();
            }

            if is_truncated {
                key_marker = next_key_marker;
                version_id_marker = next_version_id_marker;
            } else {
                break;
            }
        }

        info!("S3 ListObjectVersions: found {} versions for '{}'", all_versions.len(), key);
        Ok(all_versions)
    }

    async fn download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let key = path.trim_start_matches('/');
        let response = self.s3_request(
            Method::GET,
            key,
            Some(&[("versionId", version_id)]),
            None,
        ).await?;

        match response.status() {
            StatusCode::OK => {
                let mut stream = response.bytes_stream();
                let mut file = tokio::fs::File::create(local_path).await
                    .map_err(ProviderError::IoError)?;

                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                    file.write_all(&chunk).await
                        .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                }
                file.flush().await
                    .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

                info!("Downloaded version '{}' of '{}' to '{}'", version_id, key, local_path);
                Ok(())
            }
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(
                    format!("Version '{}' of '{}' not found", version_id, path),
                ))
            }
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::TransferFailed(
                    format!("Download version failed ({}): {}", status, sanitize_api_error(&body)),
                ))
            }
        }
    }

    async fn restore_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let key = path.trim_start_matches('/');
        // Restore by copying the old version to itself
        let copy_source = format!(
            "/{}/{}?versionId={}",
            self.config.bucket,
            urlencoding::encode(key),
            urlencoding::encode(version_id)
        );

        let url = self.build_url(key);

        use sha2::{Sha256, Digest};
        let payload_hash = {
            let mut hasher = Sha256::new();
            hasher.update(b"");
            hex::encode(hasher.finalize())
        };

        let mut headers = HashMap::new();
        headers.insert("x-amz-copy-source".to_string(), copy_source);
        let authorization = self.sign_request("PUT", &url, &mut headers, &payload_hash)?;

        let mut request = self.client.put(&url);
        for (k, v) in headers.iter() {
            request = request.header(k, v);
        }
        request = request.header("Authorization", &authorization);
        request = request.header("Content-Length", "0");

        let response = request.send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => {
                info!("Restored '{}' to version '{}'", key, version_id);
                Ok(())
            }
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::ServerError(
                    format!("Restore version failed ({}): {}", status, sanitize_api_error(&body)),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_build_url_path_style() {
        let provider = S3Provider::new(S3Config {
            endpoint: Some("http://localhost:9000".to_string()),
            region: "us-east-1".to_string(),
            access_key_id: "minioadmin".to_string(),
            secret_access_key: secrecy::SecretString::from("minioadmin".to_string()),
            bucket: "test-bucket".to_string(),
            prefix: None,
            path_style: true,
        }).expect("Failed to create S3Provider");

        assert_eq!(
            provider.build_url("path/to/file.txt"),
            "http://localhost:9000/test-bucket/path/to/file.txt"
        );
    }

    #[test]
    fn test_build_url_virtual_hosted() {
        let provider = S3Provider::new(S3Config {
            endpoint: None,
            region: "us-west-2".to_string(),
            access_key_id: "key".to_string(),
            secret_access_key: secrecy::SecretString::from("secret".to_string()),
            bucket: "my-bucket".to_string(),
            prefix: None,
            path_style: false,
        }).expect("Failed to create S3Provider");

        assert_eq!(
            provider.build_url("path/to/file.txt"),
            "https://my-bucket.s3.us-west-2.amazonaws.com/path/to/file.txt"
        );
    }
}
