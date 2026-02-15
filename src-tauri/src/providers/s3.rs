//! S3 Storage Provider
//!
//! Implementation of the StorageProvider trait for Amazon S3 and S3-compatible storage.
//! Supports AWS S3, MinIO, Backblaze B2, DigitalOcean Spaces, Cloudflare R2, Wasabi, etc.
//!
//! This implementation uses reqwest with AWS Signature Version 4 for authentication,
//! avoiding the heavyweight aws-sdk-s3 dependency for better compile times and smaller binaries.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tracing::{debug, info};
use reqwest::{Client, Method, StatusCode};
use std::collections::HashMap;

use super::{
    StorageProvider, ProviderError, ProviderType, RemoteEntry, S3Config,
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
    pub fn new(config: S3Config) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("Failed to create HTTP client");
        
        Self {
            config,
            client,
            current_prefix: String::new(),
            connected: false,
        }
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
        
        // URI-encode the path (but keep / as is)
        let canonical_path = if path.is_empty() { "/" } else { path };
        
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
            format!("AWS4{}", self.config.secret_access_key).as_bytes(),
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
        
        debug!("S3 Headers: {:?}", headers);
        
        if let Some(body_data) = body {
            // Explicitly set Content-Length for empty bodies (required by some S3-compatible services like Backblaze B2)
            request = request.header("Content-Length", body_data.len().to_string());
            request = request.body(body_data);
        }
        
        let response = request.send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        let status = response.status();
        if !status.is_success() {
            warn!("S3 Response Status: {} for {} {}", status, method, url);
        }
        
        Ok(response)
    }
    
    /// Parse S3 ListObjectsV2 XML response
    fn parse_list_response(&self, xml: &str) -> Result<(Vec<RemoteEntry>, Option<String>), ProviderError> {
        let mut entries = Vec::new();
        
        debug!("Parsing S3 ListObjectsV2 XML response, {} bytes", xml.len());
        
        // Parse common prefixes (directories)
        // Note: (?s) enables DOTALL mode so . matches newlines in multi-line XML
        let prefix_pattern = regex::Regex::new(r"(?s)<CommonPrefixes>\s*<Prefix>([^<]+)</Prefix>\s*</CommonPrefixes>")
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        
        for cap in prefix_pattern.captures_iter(xml) {
            if let Some(prefix_match) = cap.get(1) {
                let full_prefix = prefix_match.as_str();
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
        }
        
        // Parse objects (files)
        let contents_pattern = regex::Regex::new(r"(?s)<Contents>(.*?)</Contents>")
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        
        for cap in contents_pattern.captures_iter(xml) {
            if let Some(content) = cap.get(1) {
                let content_str = content.as_str();
                
                // Extract key
                let key = self.extract_xml_tag(content_str, "Key");
                if key.is_none() {
                    continue;
                }
                let key = key.unwrap();
                
                // Skip if key ends with / (it's a directory marker)
                if key.ends_with('/') {
                    continue;
                }
                
                // Skip if key equals current prefix
                if key == self.current_prefix || key.trim_start_matches('/') == self.current_prefix.trim_start_matches('/') {
                    continue;
                }
                
                let name = key.rsplit('/').next().unwrap_or(&key).to_string();
                
                if name.is_empty() {
                    continue;
                }
                
                let size: u64 = self.extract_xml_tag(content_str, "Size")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                
                let modified = self.extract_xml_tag(content_str, "LastModified");
                
                let etag = self.extract_xml_tag(content_str, "ETag")
                    .map(|s| s.trim_matches('"').to_string());
                
                let storage_class = self.extract_xml_tag(content_str, "StorageClass");
                
                let mut metadata = HashMap::new();
                if let Some(etag) = etag {
                    metadata.insert("etag".to_string(), etag);
                }
                if let Some(sc) = storage_class {
                    metadata.insert("storage_class".to_string(), sc);
                }
                
                entries.push(RemoteEntry {
                    name,
                    path: format!("/{}", key),
                    is_dir: false,
                    size,
                    modified,
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
        
        // Check for continuation token
        let continuation_token = self.extract_xml_tag(xml, "NextContinuationToken");
        
        Ok((entries, continuation_token))
    }
    
    /// Extract content from an XML tag
    fn extract_xml_tag(&self, xml: &str, tag: &str) -> Option<String> {
        let pattern = format!(r"<{}[^>]*>([^<]*)</{}>", tag, tag);
        if let Ok(re) = regex::Regex::new(&pattern) {
            if let Some(cap) = re.captures(xml) {
                if let Some(content) = cap.get(1) {
                    let text = content.as_str().trim().to_string();
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
            }
        }
        None
    }

    /// Minimum part size for multipart upload (5 MB)
    const MULTIPART_THRESHOLD: usize = 5 * 1024 * 1024;
    /// Part size for multipart upload chunks (5 MB)
    const MULTIPART_PART_SIZE: usize = 5 * 1024 * 1024;

    /// Initiate a multipart upload, returns the UploadId
    async fn create_multipart_upload(&self, key: &str) -> Result<String, ProviderError> {
        let response = self.s3_request(
            Method::POST,
            key,
            Some(&[("uploads", "")]),
            Some(Vec::new()),
        ).await?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(ProviderError::TransferFailed(
                format!("CreateMultipartUpload failed ({}): {}", status, body),
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
                format!("UploadPart {} failed ({}): {}", part_number, status, body),
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
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(
                format!("CompleteMultipartUpload failed ({}): {}", status, body),
            ));
        }

        Ok(())
    }

    /// Upload data using S3 multipart upload
    async fn upload_multipart(
        &self,
        key: &str,
        data: Vec<u8>,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let total_size = data.len() as u64;
        let upload_id = self.create_multipart_upload(key).await?;

        let mut parts: Vec<(u32, String)> = Vec::new();
        let mut offset = 0usize;
        let mut part_number = 1u32;

        while offset < data.len() {
            let end = std::cmp::min(offset + Self::MULTIPART_PART_SIZE, data.len());
            let chunk = data[offset..end].to_vec();
            let chunk_len = chunk.len();

            match self.upload_part(key, &upload_id, part_number, chunk).await {
                Ok(etag) => {
                    parts.push((part_number, etag));
                    offset += chunk_len;
                    if let Some(ref progress) = on_progress {
                        progress(offset as u64, total_size);
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
    async fn list_keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>, ProviderError> {
        let response = self.s3_request(
            Method::GET,
            "",
            Some(&[("list-type", "2"), ("prefix", prefix)]),
            None,
        ).await?;

        if response.status() != StatusCode::OK {
            return Err(ProviderError::ServerError("Failed to list objects by prefix".to_string()));
        }

        let xml = response.text().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let key_pattern = regex::Regex::new(r"<Key>([^<]+)</Key>")
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let keys: Vec<String> = key_pattern.captures_iter(&xml)
            .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
            .collect();

        Ok(keys)
    }
}

#[async_trait]
impl StorageProvider for S3Provider {
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
                Err(ProviderError::AuthenticationFailed(format!("S3 auth error: {}", error_msg)))
            }
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(format!("Bucket '{}' not found", self.config.bucket)))
            }
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::ConnectionFailed(format!("S3 error ({}): {}", status, body)))
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
                    info!("S3 LIST response XML:\n{}", xml_preview);
                    
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
                    return Err(ProviderError::ServerError(format!("List failed ({}): {}", status, error_msg)));
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
        } else if path.starts_with('/') {
            path.trim_matches('/').to_string()
        } else {
            if self.current_prefix.is_empty() {
                path.trim_matches('/').to_string()
            } else {
                format!("{}/{}", self.current_prefix, path.trim_matches('/'))
            }
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
        let response = self.s3_request(Method::GET, key, None, None).await?;
        
        match response.status() {
            StatusCode::OK => {
                let total_size = response.content_length().unwrap_or(0);
                let bytes = response.bytes().await
                    .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                
                if let Some(progress) = on_progress {
                    progress(bytes.len() as u64, total_size);
                }
                
                tokio::fs::write(local_path, &bytes).await
                    .map_err(|e| ProviderError::IoError(e))?;
                
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
                let bytes = response.bytes().await
                    .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                Ok(bytes.to_vec())
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

        let data = tokio::fs::read(local_path).await
            .map_err(|e| ProviderError::IoError(e))?;

        let total_size = data.len() as u64;
        let key = remote_path.trim_start_matches('/');

        // Use multipart upload for files larger than 5MB
        if data.len() > Self::MULTIPART_THRESHOLD {
            return self.upload_multipart(key, data, on_progress).await;
        }

        let response = self.s3_request(Method::PUT, key, None, Some(data)).await?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => {
                if let Some(progress) = on_progress {
                    progress(total_size, total_size);
                }
                Ok(())
            }
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::TransferFailed(format!("Upload failed ({}): {}", status, body)))
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

        for key in &keys {
            let _ = self.s3_request(Method::DELETE, key, None, None).await;
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
            // Single file rename: copy + delete
            self.server_copy(from, to).await?;
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
    
    fn supports_share_links(&self) -> bool {
        true
    }
    
    async fn create_share_link(
        &mut self,
        path: &str,
        expires_in_secs: Option<u64>,
    ) -> Result<String, ProviderError> {
        // Generate a presigned URL
        // This is a simplified implementation
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
        let path_part = parsed.path();
        
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
            path_part,
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
            format!("AWS4{}", self.config.secret_access_key).as_bytes(),
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
                return Err(ProviderError::ServerError(format!("Search failed: {}", body)));
            }

            let xml = response.text().await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            // Parse keys and filter by pattern
            let key_pattern = regex::Regex::new(r"<Key>([^<]+)</Key>")
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            for cap in key_pattern.captures_iter(&xml) {
                if let Some(key_match) = cap.get(1) {
                    let key = key_match.as_str();
                    let name = key.rsplit('/').next().unwrap_or(key);

                    if name.to_lowercase().contains(&pattern_lower) && !key.ends_with('/') {
                        let size_str = {
                            // Extract size for this key from the Contents block
                            let contents_re = regex::Regex::new(&format!(
                                r"(?s)<Contents>.*?<Key>{}</Key>.*?<Size>(\d+)</Size>.*?</Contents>",
                                regex::escape(key)
                            )).ok();
                            contents_re.and_then(|re| {
                                re.captures(&xml).and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
                            })
                        };

                        let size: u64 = size_str.and_then(|s| s.parse().ok()).unwrap_or(0);

                        all_entries.push(RemoteEntry {
                            name: name.to_string(),
                            path: format!("/{}", key),
                            is_dir: false,
                            size,
                            modified: None,
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

            let next_token = self.extract_xml_tag(&xml, "NextContinuationToken");
            match next_token {
                Some(token) if all_entries.len() < 500 => continuation_token = Some(token),
                _ => break,
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
        let authorization = self.sign_request("PUT", &url, &mut headers, &payload_hash)?;

        let mut request = self.client.put(&url);
        for (key, value) in headers.iter() {
            request = request.header(key, value);
        }
        request = request.header("Authorization", &authorization);
        request = request.header("Content-Length", "0");

        let response = request.send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => {
                info!("Copied {} to {}", from, to);
                Ok(())
            }
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::ServerError(format!("Copy failed ({}): {}", status, body)))
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
            secret_access_key: "minioadmin".to_string(),
            bucket: "test-bucket".to_string(),
            prefix: None,
            path_style: true,
        });
        
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
            secret_access_key: "secret".to_string(),
            bucket: "my-bucket".to_string(),
            prefix: None,
            path_style: false,
        });
        
        assert_eq!(
            provider.build_url("path/to/file.txt"),
            "https://my-bucket.s3.us-west-2.amazonaws.com/path/to/file.txt"
        );
    }
}
