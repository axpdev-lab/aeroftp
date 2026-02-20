//! WebDAV Storage Provider
//!
//! Implementation of the StorageProvider trait for WebDAV protocol.
//! Compatible with Nextcloud, Synology, QNAP, Koofr, and other WebDAV servers.
//!
//! WebDAV extends HTTP with methods like PROPFIND, MKCOL, MOVE, COPY, and DELETE
//! to provide full file system operations over HTTP/HTTPS.

use async_trait::async_trait;
use futures_util::StreamExt;
use md5::{Md5, Digest as _};
use quick_xml::events::Event;
use quick_xml::Reader;
use rand::Rng;
use reqwest::{Client, Method, StatusCode};
use secrecy::ExposeSecret;
use std::collections::HashMap;
use tokio::io::AsyncWriteExt;

use super::{
    StorageProvider, ProviderError, ProviderType, RemoteEntry, WebDavConfig,
    sanitize_api_error,
};

// ============ HTTP Digest Authentication (RFC 2617) ============

/// State for HTTP Digest authentication
struct DigestState {
    realm: String,
    nonce: String,
    qop: String,
    opaque: Option<String>,
    nc: u32,
}

impl DigestState {
    /// Parse a `WWW-Authenticate: Digest ...` header value
    fn parse(header: &str) -> Option<Self> {
        let s = header.strip_prefix("Digest ")?;
        Some(Self {
            realm: Self::extract_param(s, "realm")?,
            nonce: Self::extract_param(s, "nonce")?,
            qop: Self::extract_param(s, "qop").unwrap_or_default(),
            opaque: Self::extract_param(s, "opaque"),
            nc: 0,
        })
    }

    /// Extract a parameter value from the Digest challenge string
    fn extract_param(s: &str, key: &str) -> Option<String> {
        // Try quoted form: key="value"
        let quoted = format!("{}=\"", key);
        if let Some(pos) = s.find(&quoted) {
            let after = &s[pos + quoted.len()..];
            let end = after.find('"')?;
            return Some(after[..end].to_string());
        }
        // Try unquoted form: key=value
        let unquoted = format!("{}=", key);
        if let Some(pos) = s.find(&unquoted) {
            let after = &s[pos + unquoted.len()..];
            let end = after.find(|c: char| c == ',' || c == ' ').unwrap_or(after.len());
            return Some(after[..end].to_string());
        }
        None
    }

    /// Generate the `Authorization: Digest ...` header value
    fn authorization(&mut self, method: &str, uri: &str, username: &str, password: &str) -> String {
        self.nc += 1;
        let nc_str = format!("{:08x}", self.nc);
        let cnonce = Self::generate_cnonce();

        let ha1 = md5_hex(&format!("{}:{}:{}", username, self.realm, password));
        let ha2 = md5_hex(&format!("{}:{}", method, uri));

        let response = if !self.qop.is_empty() {
            md5_hex(&format!("{}:{}:{}:{}:auth:{}", ha1, self.nonce, nc_str, cnonce, ha2))
        } else {
            md5_hex(&format!("{}:{}:{}", ha1, self.nonce, ha2))
        };

        // Quote algorithm and qop for maximum server compatibility
        // (Python requests quotes these and works with all servers)
        let mut header = format!(
            r#"Digest username="{}", realm="{}", nonce="{}", uri="{}", response="{}", algorithm="MD5""#,
            username, self.realm, self.nonce, uri, response
        );

        if !self.qop.is_empty() {
            header.push_str(&format!(r#", qop="auth", nc={}, cnonce="{}""#, nc_str, cnonce));
        }

        if let Some(ref opaque) = self.opaque {
            header.push_str(&format!(r#", opaque="{}""#, opaque));
        }

        tracing::debug!("[WebDAV Digest] method={} uri={} nc={} response={}", method, uri, nc_str, response);

        header
    }

    fn generate_cnonce() -> String {
        use rand::rngs::OsRng;
        let bytes: [u8; 8] = OsRng.gen();
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

/// Compute MD5 hex digest of a string
fn md5_hex(input: &str) -> String {
    let digest = Md5::digest(input.as_bytes());
    format!("{:x}", digest)
}

/// Extract the path component from a full URL, preserving trailing slash
fn extract_uri_path(url: &str) -> String {
    if let Some(idx) = url.find("://") {
        let after_scheme = &url[idx + 3..];
        if let Some(slash_idx) = after_scheme.find('/') {
            let path = after_scheme[slash_idx..].to_string();
            if !path.is_empty() {
                return path;
            }
        }
    }
    "/".to_string()
}

/// Custom HTTP methods for WebDAV
mod webdav_methods {
    use reqwest::Method;
    
    pub fn propfind() -> Method {
        Method::from_bytes(b"PROPFIND").unwrap()
    }
    
    pub fn mkcol() -> Method {
        Method::from_bytes(b"MKCOL").unwrap()
    }
    
    #[allow(dead_code)]
    pub fn copy() -> Method {
        Method::from_bytes(b"COPY").unwrap()
    }
    
    pub fn move_method() -> Method {
        Method::from_bytes(b"MOVE").unwrap()
    }

    pub fn lock() -> Method {
        Method::from_bytes(b"LOCK").unwrap()
    }

    pub fn unlock() -> Method {
        Method::from_bytes(b"UNLOCK").unwrap()
    }
}

/// WebDAV Storage Provider
pub struct WebDavProvider {
    config: WebDavConfig,
    client: Client,
    current_path: String,
    connected: bool,
    /// Digest auth state (set during connect if server requires it)
    digest_auth: Option<DigestState>,
}

impl WebDavProvider {
    /// Create a new WebDAV provider with the given configuration
    pub fn new(config: WebDavConfig) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .danger_accept_invalid_certs(!config.verify_cert)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("HTTP client init failed: {e}")))?;

        Ok(Self {
            config,
            client,
            current_path: "/".to_string(),
            connected: false,
            digest_auth: None,
        })
    }
    
    /// Build full URL for a path
    fn build_url(&self, path: &str) -> String {
        let base = self.config.url.trim_end_matches('/');
        let path = path.trim_start_matches('/');

        if path.is_empty() || path == "/" {
            // For root/empty path, ensure trailing slash (required by some WebDAV servers
            // for Digest auth URI matching on directory endpoints)
            format!("{}/", base)
        } else {
            format!("{}/{}", base, path)
        }
    }
    
    /// Make an authenticated request (Basic or Digest depending on server)
    fn request(&mut self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let url = self.build_url(path);
        let builder = self.client.request(method.clone(), &url);

        if let Some(ref mut state) = self.digest_auth {
            let uri_path = extract_uri_path(&url);
            tracing::debug!("[WebDAV] Digest request: {} {} (uri={})", method.as_str(), url, uri_path);
            let auth = state.authorization(method.as_str(), &uri_path, &self.config.username, self.config.password.expose_secret());
            builder.header("Authorization", auth)
        } else {
            builder.basic_auth(&self.config.username, Some(self.config.password.expose_secret()))
        }
    }
    
    /// Parse PROPFIND XML response into RemoteEntry list using quick-xml
    fn parse_propfind_response(&self, xml: &str, base_path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let mut entries = Vec::new();

        tracing::debug!("[WebDAV] Parsing XML with base_path: {}, url: {}", base_path, self.config.url);

        // Event-based quick-xml parser
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();

        let mut in_response = false;
        let mut in_resourcetype = false;
        let mut current_tag: Option<String> = None;
        let mut href = String::new();
        let mut displayname = String::new();
        let mut getcontentlength = String::new();
        let mut getlastmodified = String::new();
        let mut getcontenttype = String::new();
        let mut getetag = String::new();
        let mut is_collection = false;
        let mut is_collection_by_iscollection = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Err(e) => {
                    tracing::warn!("[WebDAV] XML parse error at position {}: {}", reader.error_position(), e);
                    break;
                }
                Ok(Event::Eof) => break,

                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    match local.as_str() {
                        "response" => {
                            in_response = true;
                            href.clear();
                            displayname.clear();
                            getcontentlength.clear();
                            getlastmodified.clear();
                            getcontenttype.clear();
                            getetag.clear();
                            is_collection = false;
                            is_collection_by_iscollection = false;
                        }
                        "resourcetype" if in_response => {
                            in_resourcetype = true;
                        }
                        "collection" if in_response && in_resourcetype => {
                            is_collection = true;
                        }
                        "href" | "displayname" | "getcontentlength" | "getlastmodified"
                        | "getcontenttype" | "getetag" | "iscollection" if in_response => {
                            current_tag = Some(local);
                        }
                        _ => {}
                    }
                }

                Ok(Event::Empty(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    if local == "collection" && in_response && in_resourcetype {
                        is_collection = true;
                    }
                }

                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    match local.as_str() {
                        "response" if in_response => {
                            // Process accumulated response
                            in_response = false;
                            if href.is_empty() {
                                tracing::warn!("[WebDAV] No href found in response element");
                                continue;
                            }

                            let decoded_href = urlencoding::decode(&href)
                                .unwrap_or_else(|_| href.clone().into());
                            let clean_path = decoded_href.trim_end_matches('/');
                            let base_clean = base_path.trim_end_matches('/');
                            let url_clean = self.config.url.trim_end_matches('/');

                            let url_path_clean = url_clean
                                .find("://")
                                .and_then(|i| url_clean[i + 3..].find('/').map(|j| i + 3 + j))
                                .map(|i| url_clean[i..].trim_end_matches('/'))
                                .unwrap_or("");

                            let is_self_reference = clean_path == base_clean
                                || clean_path == url_clean
                                || (!base_clean.is_empty()
                                    && clean_path.ends_with(base_clean))
                                || (!base_clean.is_empty()
                                    && clean_path.ends_with(&format!(
                                        "/{}",
                                        base_clean.trim_start_matches('/')
                                    )))
                                || (!base_clean.is_empty()
                                    && base_clean != "/"
                                    && url_clean.ends_with(clean_path))
                                || (!url_path_clean.is_empty()
                                    && clean_path == url_path_clean);

                            if is_self_reference {
                                tracing::debug!(
                                    "[WebDAV] Skipping self-reference: {}",
                                    clean_path
                                );
                                continue;
                            }

                            let href_ends_slash = href.ends_with('/');
                            let is_dir = is_collection
                                || is_collection_by_iscollection
                                || href_ends_slash;

                            let size: u64 =
                                getcontentlength.parse().unwrap_or(0);
                            let modified = if getlastmodified.is_empty() {
                                None
                            } else {
                                Some(getlastmodified.clone())
                            };

                            // Extract name: prefer displayname, fallback to href
                            let name = if !displayname.is_empty() {
                                displayname.clone()
                            } else {
                                decoded_href
                                    .trim_end_matches('/')
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or("")
                                    .to_string()
                            };

                            if name.is_empty() || name == "." || name == ".." {
                                continue;
                            }

                            let path = if self.current_path.ends_with('/') {
                                format!("{}{}", self.current_path, name)
                            } else {
                                format!("{}/{}", self.current_path, name)
                            };

                            let mime_type = if getcontenttype.is_empty() {
                                None
                            } else {
                                Some(getcontenttype.clone())
                            };

                            let mut metadata = HashMap::new();
                            if !getetag.is_empty() {
                                metadata
                                    .insert("etag".to_string(), getetag.clone());
                            }

                            entries.push(RemoteEntry {
                                name,
                                path,
                                is_dir,
                                size,
                                modified,
                                permissions: None,
                                owner: None,
                                group: None,
                                is_symlink: false,
                                link_target: None,
                                mime_type,
                                metadata,
                            });
                        }
                        "resourcetype" if in_resourcetype => {
                            in_resourcetype = false;
                        }
                        _ => {
                            if current_tag.as_deref() == Some(local.as_str()) {
                                current_tag = None;
                            }
                        }
                    }
                }

                Ok(Event::Text(ref e)) => {
                    if let Some(ref tag) = current_tag {
                        let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                        if !text.is_empty() {
                            match tag.as_str() {
                                "href" => href = text,
                                "displayname" => displayname = text,
                                "getcontentlength" => getcontentlength = text,
                                "getlastmodified" => getlastmodified = text,
                                "getcontenttype" => getcontenttype = text,
                                "getetag" => getetag = text,
                                "iscollection" => {
                                    if text == "1" {
                                        is_collection_by_iscollection = true;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }

                Ok(Event::CData(ref e)) => {
                    if let Some(ref tag) = current_tag {
                        let text = String::from_utf8_lossy(e.as_ref())
                            .trim()
                            .to_string();
                        if !text.is_empty() {
                            match tag.as_str() {
                                "href" => href = text,
                                "displayname" => displayname = text,
                                "getcontentlength" => getcontentlength = text,
                                "getlastmodified" => getlastmodified = text,
                                "getcontenttype" => getcontenttype = text,
                                "getetag" => getetag = text,
                                "iscollection" => {
                                    if text == "1" {
                                        is_collection_by_iscollection = true;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }

                _ => {}
            }
            buf.clear();
        }

        tracing::debug!("[WebDAV] Parsed {} entries", entries.len());
        Ok(entries)
    }

    /// Extract a single property value from a PROPFIND Depth:0 XML response using quick-xml.
    /// Used by stat() and storage_info() for simple single-response parsing.
    fn extract_xml_properties(&self, xml: &str) -> HashMap<String, String> {
        let mut props = HashMap::new();
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut current_tag: Option<String> = None;
        let mut in_resourcetype = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Err(_) | Ok(Event::Eof) => break,
                Ok(Event::Start(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    match local.as_str() {
                        "resourcetype" => { in_resourcetype = true; }
                        "collection" if in_resourcetype => {
                            props.insert("_is_collection".to_string(), "true".to_string());
                        }
                        _ => { current_tag = Some(local); }
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    if local == "collection" && in_resourcetype {
                        props.insert("_is_collection".to_string(), "true".to_string());
                    }
                }
                Ok(Event::End(ref e)) => {
                    let local = local_name(e.name().as_ref());
                    if local == "resourcetype" { in_resourcetype = false; }
                    if current_tag.as_deref() == Some(local.as_str()) { current_tag = None; }
                }
                Ok(Event::Text(ref e)) => {
                    if let Some(ref tag) = current_tag {
                        let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                        if !text.is_empty() {
                            if tag == "iscollection" && text == "1" {
                                props.insert("_is_collection".to_string(), "true".to_string());
                            }
                            props.insert(tag.clone(), text);
                        }
                    }
                }
                Ok(Event::CData(ref e)) => {
                    if let Some(ref tag) = current_tag {
                        let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                        if !text.is_empty() {
                            if tag == "iscollection" && text == "1" {
                                props.insert("_is_collection".to_string(), "true".to_string());
                            }
                            props.insert(tag.clone(), text);
                        }
                    }
                }
                _ => {}
            }
            buf.clear();
        }
        props
    }
}

/// Strip namespace prefix from an XML element name, returning an owned String.
/// e.g. "d:response" -> "response", "DAV:href" -> "href", "response" -> "response"
fn local_name(raw: &[u8]) -> String {
    let s = std::str::from_utf8(raw).unwrap_or("");
    match s.rfind(':') {
        Some(pos) => s[pos + 1..].to_string(),
        None => s.to_string(),
    }
}

#[async_trait]
impl StorageProvider for WebDavProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }

    fn provider_type(&self) -> ProviderType {
        ProviderType::WebDav
    }
    
    fn display_name(&self) -> String {
        format!("{}@{}", self.config.username, 
            self.config.url
                .replace("https://", "")
                .replace("http://", "")
                .split('/')
                .next()
                .unwrap_or(&self.config.url)
        )
    }
    
    async fn connect(&mut self) -> Result<(), ProviderError> {
        let propfind_body = r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                    </d:prop>
                </d:propfind>"#;

        // First attempt with Basic auth
        let response = self.request(webdav_methods::propfind(), "/")
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(propfind_body)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                self.connected = true;
                if let Some(ref initial_path) = self.config.initial_path {
                    if !initial_path.is_empty() {
                        self.current_path = initial_path.clone();
                    }
                }
                Ok(())
            }
            StatusCode::UNAUTHORIZED => {
                // Check if server requires Digest authentication
                let www_auth = response.headers()
                    .get("www-authenticate")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();

                if let Some(state) = DigestState::parse(&www_auth) {
                    tracing::debug!("[WebDAV] Server requires Digest auth (realm: {}, qop: {}, nonce: {}...)",
                        state.realm, state.qop, &state.nonce[..state.nonce.len().min(12)]);
                    self.digest_auth = Some(state);

                    // Retry with Digest auth
                    let response2 = self.request(webdav_methods::propfind(), "/")
                        .header("Depth", "0")
                        .header("Content-Type", "application/xml")
                        .body(propfind_body)
                        .send()
                        .await
                        .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

                    let retry_status = response2.status();
                    tracing::debug!("[WebDAV] Digest auth retry status: {}", retry_status);

                    match retry_status {
                        StatusCode::OK | StatusCode::MULTI_STATUS => {
                            tracing::debug!("[WebDAV] Digest auth successful");
                            self.connected = true;
                            if let Some(ref initial_path) = self.config.initial_path {
                                if !initial_path.is_empty() {
                                    self.current_path = initial_path.clone();
                                }
                            }
                            Ok(())
                        }
                        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                            // Log the response body for debugging
                            let body = response2.text().await.unwrap_or_default();
                            tracing::warn!("[WebDAV] Digest auth failed ({}): {}", retry_status, &body[..body.len().min(200)]);
                            self.digest_auth = None;
                            Err(ProviderError::AuthenticationFailed("Invalid credentials".to_string()))
                        }
                        status => {
                            self.digest_auth = None;
                            Err(ProviderError::ConnectionFailed(format!("Server returned status: {}", status)))
                        }
                    }
                } else {
                    Err(ProviderError::AuthenticationFailed("Invalid credentials".to_string()))
                }
            }
            StatusCode::FORBIDDEN => {
                Err(ProviderError::AuthenticationFailed("Invalid credentials".to_string()))
            }
            status => {
                Err(ProviderError::ConnectionFailed(format!("Server returned status: {}", status)))
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
        
        let list_path = if path.is_empty() || path == "." {
            self.current_path.clone()
        } else {
            path.to_string()
        };
        
        tracing::debug!("[WebDAV] Listing path: {}", list_path);
        
        let response = self.request(webdav_methods::propfind(), &list_path)
            .header("Depth", "1")
            .header("Content-Type", "application/xml")
            .body(r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                        <d:getcontentlength/>
                        <d:getlastmodified/>
                        <d:getcontenttype/>
                        <d:getetag/>
                        <d:displayname/>
                    </d:prop>
                </d:propfind>"#)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        let status = response.status();
        tracing::debug!("[WebDAV] List response status: {}", status);
        
        match status {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                let xml = response.text().await
                    .map_err(|e| ProviderError::ParseError(e.to_string()))?;
                
                tracing::debug!("[WebDAV] Response XML length: {} bytes", xml.len());
                tracing::debug!("[WebDAV] Full XML response:\n{}", xml);
                
                let entries = self.parse_propfind_response(&xml, &list_path)?;
                tracing::debug!("[WebDAV] Parsed {} entries", entries.len());
                Ok(entries)
            }
            StatusCode::NOT_FOUND => {
                tracing::warn!("[WebDAV] Path not found: {}", list_path);
                Err(ProviderError::NotFound(list_path))
            }
            StatusCode::UNAUTHORIZED => {
                self.connected = false;
                tracing::error!("[WebDAV] Unauthorized - session expired");
                Err(ProviderError::AuthenticationFailed("Session expired".to_string()))
            }
            status => {
                tracing::error!("[WebDAV] Server error: {}", status);
                Err(ProviderError::ServerError(format!("Server returned status: {}", status)))
            }
        }
    }
    
    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }
    
    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        // Verify the path exists and is a directory
        let response = self.request(webdav_methods::propfind(), path)
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                    </d:prop>
                </d:propfind>"#)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                let xml = response.text().await
                    .map_err(|e| ProviderError::ParseError(e.to_string()))?;

                let props = self.extract_xml_properties(&xml);
                let is_collection = props.contains_key("_is_collection");

                if is_collection {
                    self.current_path = path.to_string();
                    Ok(())
                } else {
                    Err(ProviderError::InvalidPath(format!("{} is not a directory", path)))
                }
            }
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(path.to_string()))
            }
            status => {
                Err(ProviderError::ServerError(format!("Server returned status: {}", status)))
            }
        }
    }
    
    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        let parent = std::path::Path::new(&self.current_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".to_string());
        
        self.current_path = if parent.is_empty() { "/".to_string() } else { parent };
        Ok(())
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
        
        let response = self.request(Method::GET, remote_path)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::OK => {
                let total_size = response.content_length().unwrap_or(0);
                let mut stream = response.bytes_stream();
                let mut file = tokio::fs::File::create(local_path)
                    .await
                    .map_err(ProviderError::IoError)?;
                let mut downloaded: u64 = 0;

                while let Some(chunk_result) = stream.next().await {
                    let chunk = chunk_result
                        .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                    file.write_all(&chunk)
                        .await
                        .map_err(ProviderError::IoError)?;
                    downloaded += chunk.len() as u64;
                    if let Some(ref progress) = on_progress {
                        progress(downloaded, total_size);
                    }
                }
                file.flush().await.map_err(ProviderError::IoError)?;

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
        
        let response = self.request(Method::GET, remote_path)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
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

        let file = tokio::fs::File::open(local_path).await
            .map_err(ProviderError::IoError)?;
        let total_size = file.metadata().await
            .map_err(ProviderError::IoError)?
            .len();

        let stream = tokio_util::io::ReaderStream::new(file);
        let body = reqwest::Body::wrap_stream(stream);

        let response = self.request(Method::PUT, remote_path)
            .body(body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => {
                if let Some(progress) = on_progress {
                    progress(total_size, total_size);
                }
                Ok(())
            }
            StatusCode::CONFLICT => {
                Err(ProviderError::InvalidPath("Parent directory does not exist".to_string()))
            }
            StatusCode::INSUFFICIENT_STORAGE => {
                Err(ProviderError::ServerError("Insufficient storage space".to_string()))
            }
            status => {
                Err(ProviderError::TransferFailed(format!("Upload failed with status: {}", status)))
            }
        }
    }
    
    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let response = self.request(webdav_methods::mkcol(), path)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::CREATED | StatusCode::OK => Ok(()),
            StatusCode::METHOD_NOT_ALLOWED => {
                Err(ProviderError::AlreadyExists(path.to_string()))
            }
            StatusCode::CONFLICT => {
                Err(ProviderError::InvalidPath("Parent directory does not exist".to_string()))
            }
            StatusCode::FORBIDDEN => {
                Err(ProviderError::PermissionDenied(path.to_string()))
            }
            status => {
                Err(ProviderError::ServerError(format!("MKCOL failed with status: {}", status)))
            }
        }
    }
    
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let response = self.request(Method::DELETE, path)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::ACCEPTED => Ok(()),
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(path.to_string()))
            }
            StatusCode::FORBIDDEN => {
                Err(ProviderError::PermissionDenied(path.to_string()))
            }
            status => {
                Err(ProviderError::ServerError(format!("DELETE failed with status: {}", status)))
            }
        }
    }
    
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // WebDAV DELETE works for both files and directories
        self.delete(path).await
    }
    
    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // WebDAV DELETE automatically deletes recursively
        self.delete(path).await
    }
    
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let destination = self.build_url(to);
        
        let response = self.request(webdav_methods::move_method(), from)
            .header("Destination", destination)
            .header("Overwrite", "F") // Don't overwrite existing
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(from.to_string()))
            }
            StatusCode::PRECONDITION_FAILED => {
                Err(ProviderError::AlreadyExists(to.to_string()))
            }
            StatusCode::CONFLICT => {
                Err(ProviderError::InvalidPath("Destination parent does not exist".to_string()))
            }
            status => {
                Err(ProviderError::ServerError(format!("MOVE failed with status: {}", status)))
            }
        }
    }
    
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let response = self.request(webdav_methods::propfind(), path)
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                        <d:getcontentlength/>
                        <d:getlastmodified/>
                        <d:getcontenttype/>
                        <d:getetag/>
                    </d:prop>
                </d:propfind>"#)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                let xml = response.text().await
                    .map_err(|e| ProviderError::ParseError(e.to_string()))?;

                let props = self.extract_xml_properties(&xml);
                let is_dir = props.contains_key("_is_collection");
                let size: u64 = props.get("getcontentlength")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let modified = props.get("getlastmodified").cloned();
                let mime_type = props.get("getcontenttype").cloned();

                let name = std::path::Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string());

                Ok(RemoteEntry {
                    name,
                    path: path.to_string(),
                    is_dir,
                    size,
                    modified,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type,
                    metadata: Default::default(),
                })
            }
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(path.to_string()))
            }
            status => {
                Err(ProviderError::ServerError(format!("PROPFIND failed with status: {}", status)))
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
        // WebDAV uses HTTP which is stateless, no keep-alive needed
        // Just verify we can still authenticate
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let response = self.request(Method::OPTIONS, "/")
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        if response.status() == StatusCode::UNAUTHORIZED {
            self.connected = false;
            return Err(ProviderError::AuthenticationFailed("Session expired".to_string()));
        }
        
        Ok(())
    }
    
    async fn server_info(&mut self) -> Result<String, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let response = self.request(Method::OPTIONS, "/")
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        let server = response
            .headers()
            .get("server")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("Unknown WebDAV Server");
        
        let dav = response
            .headers()
            .get("dav")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("1");
        
        Ok(format!("WebDAV Server: {} (DAV compliance: {})", server, dav))
    }
    
    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let pattern_lower = pattern.to_lowercase();
        let mut results = Vec::new();
        let mut dirs_to_scan = vec![path.to_string()];

        while let Some(dir) = dirs_to_scan.pop() {
            let entries = match self.list(&dir).await {
                Ok(e) => e,
                Err(_) => continue,
            };

            for entry in entries {
                if entry.is_dir {
                    dirs_to_scan.push(entry.path.clone());
                }

                if entry.name.to_lowercase().contains(&pattern_lower) {
                    results.push(entry);
                    if results.len() >= 500 {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }

    async fn storage_info(&mut self) -> Result<super::StorageInfo, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // RFC 4331: WebDAV quota properties
        let response = self.request(webdav_methods::propfind(), &self.current_path.clone())
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:quota-available-bytes/>
                        <d:quota-used-bytes/>
                    </d:prop>
                </d:propfind>"#)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !response.status().is_success() && response.status() != StatusCode::MULTI_STATUS {
            return Err(ProviderError::NotSupported("storage_info".to_string()));
        }

        let xml = response.text().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let props = self.extract_xml_properties(&xml);
        let used = props.get("quota-used-bytes")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let free = props.get("quota-available-bytes")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let total = used + free;

        Ok(super::StorageInfo { used, total, free })
    }

    fn supports_locking(&self) -> bool {
        true
    }

    async fn lock_file(&mut self, path: &str, timeout: u64) -> Result<super::LockInfo, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let timeout_header = if timeout == 0 {
            "Infinite".to_string()
        } else {
            format!("Second-{}", timeout)
        };

        let lock_body = r#"<?xml version="1.0" encoding="utf-8"?>
            <d:lockinfo xmlns:d="DAV:">
                <d:lockscope><d:exclusive/></d:lockscope>
                <d:locktype><d:write/></d:locktype>
                <d:owner><d:href>AeroFTP</d:href></d:owner>
            </d:lockinfo>"#;

        let response = self.request(webdav_methods::lock(), path)
            .header("Depth", "0")
            .header("Timeout", &timeout_header)
            .header("Content-Type", "application/xml")
            .body(lock_body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!("LOCK failed ({}): {}", status, sanitize_api_error(&text))));
        }

        // Extract lock token from Lock-Token header or XML response
        let lock_token = response.headers()
            .get("lock-token")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_matches(|c| c == '<' || c == '>').to_string())
            .unwrap_or_default();

        Ok(super::LockInfo {
            token: lock_token,
            owner: Some("AeroFTP".to_string()),
            timeout,
            exclusive: true,
        })
    }

    async fn unlock_file(&mut self, path: &str, lock_token: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let token_header = format!("<{}>", lock_token);

        let response = self.request(webdav_methods::unlock(), path)
            .header("Lock-Token", &token_header)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            reqwest::StatusCode::OK | reqwest::StatusCode::NO_CONTENT => Ok(()),
            status => {
                let text = response.text().await.unwrap_or_default();
                Err(ProviderError::ServerError(format!("UNLOCK failed ({}): {}", status, sanitize_api_error(&text))))
            }
        }
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let destination = self.build_url(to);
        
        let response = self.request(webdav_methods::copy(), from)
            .header("Destination", destination)
            .header("Overwrite", "F")
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(from.to_string()))
            }
            StatusCode::PRECONDITION_FAILED => {
                Err(ProviderError::AlreadyExists(to.to_string()))
            }
            status => {
                Err(ProviderError::ServerError(format!("COPY failed with status: {}", status)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(url: &str) -> WebDavConfig {
        WebDavConfig {
            url: url.to_string(),
            username: "user".to_string(),
            password: secrecy::SecretString::from("pass".to_string()),
            initial_path: None,
            verify_cert: true,
        }
    }

    #[test]
    fn test_build_url() {
        let provider = WebDavProvider::new(
            test_config("https://cloud.example.com/remote.php/dav/files/user/"),
        ).expect("Failed to create WebDavProvider");

        assert_eq!(
            provider.build_url("/Documents"),
            "https://cloud.example.com/remote.php/dav/files/user/Documents"
        );
    }

    #[test]
    fn test_extract_xml_properties() {
        let provider = WebDavProvider::new(test_config("https://example.com"))
            .expect("Failed to create WebDavProvider");

        // Test with d: prefix
        let xml = r#"<d:multistatus xmlns:d="DAV:">
            <d:response>
                <d:propstat>
                    <d:prop>
                        <d:getcontentlength>12345</d:getcontentlength>
                        <d:getcontenttype>text/plain</d:getcontenttype>
                    </d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;
        let props = provider.extract_xml_properties(xml);
        assert_eq!(props.get("getcontentlength"), Some(&"12345".to_string()));
        assert_eq!(props.get("getcontenttype"), Some(&"text/plain".to_string()));

        // Test with D: prefix
        let xml2 = r#"<D:multistatus xmlns:D="DAV:">
            <D:response>
                <D:propstat>
                    <D:prop>
                        <D:getcontentlength>99</D:getcontentlength>
                    </D:prop>
                </D:propstat>
            </D:response>
        </D:multistatus>"#;
        let props2 = provider.extract_xml_properties(xml2);
        assert_eq!(props2.get("getcontentlength"), Some(&"99".to_string()));

        // Test collection detection
        let xml3 = r#"<d:multistatus xmlns:d="DAV:">
            <d:response>
                <d:propstat>
                    <d:prop>
                        <d:resourcetype><d:collection/></d:resourcetype>
                    </d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;
        let props3 = provider.extract_xml_properties(xml3);
        assert!(props3.contains_key("_is_collection"));
    }

    #[test]
    fn test_parse_propfind_response() {
        let provider = WebDavProvider::new(test_config("https://example.com/dav"))
            .expect("Failed to create WebDavProvider");

        let xml = r#"<?xml version="1.0"?>
        <d:multistatus xmlns:d="DAV:">
            <d:response>
                <d:href>/dav/</d:href>
                <d:propstat>
                    <d:prop>
                        <d:resourcetype><d:collection/></d:resourcetype>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/file.txt</d:href>
                <d:propstat>
                    <d:prop>
                        <d:resourcetype/>
                        <d:getcontentlength>1024</d:getcontentlength>
                        <d:getlastmodified>Mon, 01 Jan 2024 00:00:00 GMT</d:getlastmodified>
                    </d:prop>
                </d:propstat>
            </d:response>
            <d:response>
                <d:href>/dav/subdir/</d:href>
                <d:propstat>
                    <d:prop>
                        <d:resourcetype><d:collection/></d:resourcetype>
                    </d:prop>
                </d:propstat>
            </d:response>
        </d:multistatus>"#;

        let entries = provider.parse_propfind_response(xml, "/dav").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "file.txt");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[0].size, 1024);
        assert_eq!(entries[1].name, "subdir");
        assert!(entries[1].is_dir);
    }
}
