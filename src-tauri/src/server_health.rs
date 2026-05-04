//! Server Health Check: real-time network diagnostics for saved servers.
//!
//! Provides DNS resolution timing, TCP connect latency, TLS certificate inspection,
//! and HTTP endpoint probing. All measurements are taken from the user's position
//! to deliver genuine, actionable performance data.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::Serialize;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;

/// Individual check result
#[derive(Serialize, Clone, Debug)]
pub struct CheckDetail {
    pub name: String,
    pub status: String, // "pass" | "fail" | "skip"
    pub latency_ms: Option<f64>,
    pub details: Option<String>,
}

/// Overall health check result for one server
#[derive(Serialize, Clone, Debug)]
pub struct HealthCheckResult {
    pub server_id: String,
    pub host: String,
    pub status: String, // "healthy" | "degraded" | "unreachable" | "error"
    pub score: u8,      // 0-100
    pub checks: Vec<CheckDetail>,
    pub checked_at: String, // ISO 8601
    pub error: Option<String>,
}

/// Input for a single health check request
#[derive(serde::Deserialize, Clone, Debug)]
pub struct HealthCheckRequest {
    pub server_id: String,
    pub host: String,
    pub port: u16,
    pub protocol: String,
    pub endpoint: Option<String>,
}

/// Cloud provider configuration: (API host, health probe URL)
/// The probe URL is a lightweight endpoint that responds without authentication,
/// used for the HTTP health check instead of HEAD / which often returns 404/400.
struct CloudProviderInfo {
    host: &'static str,
    probe_url: &'static str,
}

fn cloud_provider_info(protocol: &str) -> Option<CloudProviderInfo> {
    match protocol {
        "googledrive" => Some(CloudProviderInfo {
            host: "www.googleapis.com",
            probe_url: "https://www.googleapis.com/discovery/v1/apis/drive/v3/rest",
        }),
        "dropbox" => Some(CloudProviderInfo {
            host: "api.dropboxapi.com",
            probe_url: "https://api.dropboxapi.com/2/check/user",
        }),
        "onedrive" => Some(CloudProviderInfo {
            host: "graph.microsoft.com",
            probe_url: "https://graph.microsoft.com/v1.0/$metadata",
        }),
        "mega" => Some(CloudProviderInfo {
            host: "g.api.mega.co.nz",
            probe_url: "https://g.api.mega.co.nz/cs",
        }),
        "box" => Some(CloudProviderInfo {
            host: "api.box.com",
            probe_url: "https://api.box.com/2.0/",
        }),
        "pcloud" => Some(CloudProviderInfo {
            host: "api.pcloud.com",
            probe_url: "https://api.pcloud.com/getapiserver",
        }),
        "filen" => Some(CloudProviderInfo {
            host: "gateway.filen.io",
            probe_url: "https://gateway.filen.io/v3/health",
        }),
        "fourshared" => Some(CloudProviderInfo {
            host: "api.4shared.com",
            probe_url: "https://api.4shared.com/v1_2/serverTime",
        }),
        "zohoworkdrive" => Some(CloudProviderInfo {
            host: "www.zohoapis.com",
            probe_url: "https://accounts.zoho.com/.well-known/openid-configuration",
        }),
        "internxt" => Some(CloudProviderInfo {
            host: "drive.internxt.com",
            probe_url: "https://drive.internxt.com/api/health",
        }),
        "kdrive" => Some(CloudProviderInfo {
            host: "api.infomaniak.com",
            probe_url: "https://api.infomaniak.com/1/ping",
        }),
        "jottacloud" => Some(CloudProviderInfo {
            host: "jfs.jottacloud.com",
            probe_url: "https://id.jottacloud.com/.well-known/openid-configuration",
        }),
        "filelu" => Some(CloudProviderInfo {
            host: "filelu.com",
            probe_url: "https://filelu.com/api/info",
        }),
        "koofr" => Some(CloudProviderInfo {
            host: "app.koofr.net",
            probe_url: "https://app.koofr.net/api/v2/info",
        }),
        "opendrive" => Some(CloudProviderInfo {
            host: "dev.opendrive.com",
            probe_url: "https://dev.opendrive.com/api/v1/branding.json",
        }),
        "azure" => Some(CloudProviderInfo {
            host: "login.microsoftonline.com",
            probe_url:
                "https://login.microsoftonline.com/common/v2.0/.well-known/openid-configuration",
        }),
        "drime" => Some(CloudProviderInfo {
            host: "api.drimecloud.com",
            probe_url: "https://api.drimecloud.com/health",
        }),
        "github" => Some(CloudProviderInfo {
            host: "api.github.com",
            probe_url: "https://api.github.com/zen",
        }),
        "gitlab" => Some(CloudProviderInfo {
            host: "gitlab.com",
            probe_url: "https://gitlab.com/api/v4/version",
        }),
        "yandexdisk" => Some(CloudProviderInfo {
            host: "cloud-api.yandex.net",
            probe_url: "https://cloud-api.yandex.net/v1/disk/",
        }),
        _ => None,
    }
}

/// Map cloud provider protocol names to their API hostnames
fn cloud_api_host(protocol: &str) -> Option<&'static str> {
    cloud_provider_info(protocol).map(|p| p.host)
}

/// Get the provider-specific health probe URL
fn cloud_probe_url(protocol: &str) -> Option<&'static str> {
    cloud_provider_info(protocol).map(|p| p.probe_url)
}

/// Whether this protocol is a cloud API (not direct TCP)
fn is_cloud_protocol(protocol: &str) -> bool {
    cloud_api_host(protocol).is_some()
}

/// Whether this protocol should get a TLS handshake check.
/// SFTP uses SSH (not TLS), FTP plain has no TLS.
/// WebDAV on non-443 ports may be plain HTTP.
fn should_check_tls(protocol: &str, port: u16) -> bool {
    if protocol == "sftp" || protocol == "ftp" {
        return false;
    }
    if protocol == "webdav" && port != 443 {
        return false;
    }
    matches!(protocol, "ftps" | "webdav" | "s3") || is_cloud_protocol(protocol)
}

/// Default port for a protocol
fn default_port(protocol: &str) -> u16 {
    match protocol {
        "ftp" => 21,
        "ftps" => 990,
        "sftp" => 22,
        "webdav" => 443,
        "s3" => 443,
        _ => 443,
    }
}

/// Perform DNS resolution with timing
async fn check_dns(host: &str, port: u16) -> CheckDetail {
    let start = Instant::now();
    let addr_str = format!("{}:{}", host, port);

    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let addrs = tokio::net::lookup_host(&addr_str).await?;
        Ok::<Vec<SocketAddr>, std::io::Error>(addrs.collect())
    })
    .await;

    match result {
        Ok(Ok(resolved)) => {
            let elapsed = start.elapsed();
            if resolved.is_empty() {
                CheckDetail {
                    name: "dns_resolution".into(),
                    status: "fail".into(),
                    latency_ms: Some(elapsed.as_secs_f64() * 1000.0),
                    details: Some("No addresses resolved".into()),
                }
            } else {
                let ip_str = resolved[0].ip().to_string();
                let extra = if resolved.len() > 1 {
                    format!("{} (+{} more)", ip_str, resolved.len() - 1)
                } else {
                    ip_str
                };
                CheckDetail {
                    name: "dns_resolution".into(),
                    status: "pass".into(),
                    latency_ms: Some(elapsed.as_secs_f64() * 1000.0),
                    details: Some(extra),
                }
            }
        }
        Ok(Err(e)) => CheckDetail {
            name: "dns_resolution".into(),
            status: "fail".into(),
            latency_ms: Some(start.elapsed().as_secs_f64() * 1000.0),
            details: Some(format!("{}", e)),
        },
        Err(_) => CheckDetail {
            name: "dns_resolution".into(),
            status: "fail".into(),
            latency_ms: Some(5000.0),
            details: Some("Timeout (5s)".into()),
        },
    }
}

/// Perform TCP connect with timing
async fn check_tcp(host: &str, port: u16) -> CheckDetail {
    let addr_str = format!("{}:{}", host, port);
    let start = Instant::now();

    match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&addr_str)).await {
        Ok(Ok(_stream)) => {
            let elapsed = start.elapsed();
            CheckDetail {
                name: "tcp_connect".into(),
                status: "pass".into(),
                latency_ms: Some(elapsed.as_secs_f64() * 1000.0),
                details: Some(format!("Port {} open", port)),
            }
        }
        Ok(Err(e)) => CheckDetail {
            name: "tcp_connect".into(),
            status: "fail".into(),
            latency_ms: Some(start.elapsed().as_secs_f64() * 1000.0),
            details: Some(format!("{}", e)),
        },
        Err(_) => CheckDetail {
            name: "tcp_connect".into(),
            status: "fail".into(),
            latency_ms: Some(10000.0),
            details: Some("Timeout (10s)".into()),
        },
    }
}

/// Perform TLS handshake timing via HTTPS HEAD request.
/// Uses a shorter timeout (5s) since we only care about the TLS negotiation,
/// not the full HTTP response.
async fn check_tls(host: &str, port: u16) -> CheckDetail {
    let url = format!("https://{}:{}/", host, port);
    let start = Instant::now();

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return CheckDetail {
                name: "tls_handshake".into(),
                status: "fail".into(),
                latency_ms: None,
                details: Some(format!("Client build error: {}", e)),
            };
        }
    };

    match client.head(&url).send().await {
        Ok(resp) => {
            let elapsed = start.elapsed();
            let version = format!("{:?}", resp.version());
            CheckDetail {
                name: "tls_handshake".into(),
                status: "pass".into(),
                latency_ms: Some(elapsed.as_secs_f64() * 1000.0),
                details: Some(format!("{} ({})", version, resp.status())),
            }
        }
        Err(e) => {
            let elapsed = start.elapsed();
            let msg = format!("{}", e);
            if msg.contains("Connection refused") {
                CheckDetail {
                    name: "tls_handshake".into(),
                    status: "skip".into(),
                    latency_ms: Some(elapsed.as_secs_f64() * 1000.0),
                    details: Some("Port does not accept HTTPS".into()),
                }
            } else {
                CheckDetail {
                    name: "tls_handshake".into(),
                    status: "fail".into(),
                    latency_ms: Some(elapsed.as_secs_f64() * 1000.0),
                    details: Some(msg),
                }
            }
        }
    }
}

/// Check if an HTTP status indicates the server is reachable.
/// For cloud APIs, 400/404/429 mean "server is up, auth required": count as pass.
fn is_reachable_status(status: reqwest::StatusCode, is_cloud: bool) -> bool {
    status.is_success()
        || status.is_redirection()
        || status == reqwest::StatusCode::UNAUTHORIZED
        || status == reqwest::StatusCode::FORBIDDEN
        || status == reqwest::StatusCode::METHOD_NOT_ALLOWED
        || (is_cloud && status == reqwest::StatusCode::BAD_REQUEST)
        || (is_cloud && status == reqwest::StatusCode::NOT_FOUND)
        || (is_cloud && status == reqwest::StatusCode::TOO_MANY_REQUESTS)
}

/// Probe HTTP endpoint. Tries HEAD first, falls back to GET if HEAD fails/times out.
/// Some APIs (MEGA, Jottacloud) don't respond to HEAD at all.
async fn check_http(url: &str, is_cloud: bool) -> CheckDetail {
    let start = Instant::now();

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return CheckDetail {
                name: "http_response".into(),
                status: "fail".into(),
                latency_ms: None,
                details: Some(format!("{}", e)),
            };
        }
    };

    // Try HEAD first
    match client.head(url).send().await {
        Ok(resp) => {
            let elapsed = start.elapsed();
            let status = resp.status();
            if is_reachable_status(status, is_cloud) {
                return CheckDetail {
                    name: "http_response".into(),
                    status: "pass".into(),
                    latency_ms: Some(elapsed.as_secs_f64() * 1000.0),
                    details: Some(format!("HTTP {}", status.as_u16())),
                };
            }
            // HEAD returned unexpected status: still reachable, just report it
            return CheckDetail {
                name: "http_response".into(),
                status: if is_cloud { "pass" } else { "fail" }.into(),
                latency_ms: Some(elapsed.as_secs_f64() * 1000.0),
                details: Some(format!("HTTP {}", status.as_u16())),
            };
        }
        Err(_head_err) => {
            // HEAD failed (timeout, connection error, etc.): try GET as fallback
            // Some APIs (MEGA, Jottacloud) only respond to POST/GET
        }
    }

    // GET fallback
    let get_start = Instant::now();
    match client.get(url).send().await {
        Ok(resp) => {
            let elapsed = get_start.elapsed();
            let status = resp.status();
            let pass = is_reachable_status(status, is_cloud);
            CheckDetail {
                name: "http_response".into(),
                status: if pass { "pass" } else { "fail" }.into(),
                latency_ms: Some(elapsed.as_secs_f64() * 1000.0),
                details: Some(format!("HTTP {} (GET)", status.as_u16())),
            }
        }
        Err(e) => CheckDetail {
            name: "http_response".into(),
            status: "fail".into(),
            latency_ms: Some(get_start.elapsed().as_secs_f64() * 1000.0),
            details: Some(format!("{}", e)),
        },
    }
}

/// Calculate health score from check results
fn calculate_score(checks: &[CheckDetail]) -> u8 {
    let mut score: i32 = 100;

    for check in checks {
        if check.status == "fail" {
            match check.name.as_str() {
                "dns_resolution" | "tcp_connect" => return 0, // Fatal
                "tls_handshake" => score -= 30,
                "http_response" => score -= 15,
                _ => score -= 10,
            }
            continue;
        }
        if check.status == "skip" {
            continue;
        }

        // Penalize slow responses
        if let Some(ms) = check.latency_ms {
            match check.name.as_str() {
                "dns_resolution" => {
                    if ms > 500.0 {
                        score -= 20;
                    } else if ms > 100.0 {
                        score -= 10;
                    }
                }
                "tcp_connect" | "tls_handshake" | "http_response" => {
                    if ms > 1000.0 {
                        score -= 25;
                    } else if ms > 500.0 {
                        score -= 15;
                    } else if ms > 200.0 {
                        score -= 10;
                    }
                }
                _ => {}
            }
        }
    }

    score.clamp(0, 100) as u8
}

/// Derive status label from score
fn score_to_status(score: u8) -> &'static str {
    match score {
        80..=100 => "healthy",
        50..=79 => "degraded",
        1..=49 => "degraded",
        0 => "unreachable",
        _ => "error",
    }
}

/// Run a full health check for one server
async fn run_health_check(req: &HealthCheckRequest) -> HealthCheckResult {
    let now = chrono::Utc::now().to_rfc3339();
    let mut checks = Vec::new();
    let is_cloud = is_cloud_protocol(&req.protocol);

    // Determine effective host and port
    let (effective_host, effective_port) = if is_cloud {
        let api_host = cloud_api_host(&req.protocol).unwrap_or("unknown");
        (api_host.to_string(), 443u16)
    } else if req.protocol == "s3" {
        // S3: prefer endpoint over host (host is often "localhost" placeholder)
        let ep_host = req.endpoint.as_deref().filter(|e| !e.is_empty()).map(|e| {
            let h = e.replace("https://", "").replace("http://", "");
            h.split('/').next().unwrap_or(&h).to_string()
        });
        match ep_host {
            Some(h) => {
                let (host, port) = parse_host_port(&h, req.port, &req.protocol);
                (host, port)
            }
            None => {
                let host = sanitize_host(&req.host);
                let port = if req.port > 0 {
                    req.port
                } else {
                    default_port(&req.protocol)
                };
                (host, port)
            }
        }
    } else {
        let host = sanitize_host(&req.host);
        let (h, p) = parse_host_port(&host, req.port, &req.protocol);
        (h, p)
    };

    // 1. DNS Resolution
    let dns = check_dns(&effective_host, effective_port).await;
    let dns_ok = dns.status == "pass";
    checks.push(dns);

    if !dns_ok {
        let score = calculate_score(&checks);
        return HealthCheckResult {
            server_id: req.server_id.clone(),
            host: effective_host,
            status: score_to_status(score).into(),
            score,
            checks,
            checked_at: now,
            error: Some("DNS resolution failed".into()),
        };
    }

    // 2. TCP Connect
    let tcp = check_tcp(&effective_host, effective_port).await;
    let tcp_ok = tcp.status == "pass";
    checks.push(tcp);

    if !tcp_ok {
        let score = calculate_score(&checks);
        return HealthCheckResult {
            server_id: req.server_id.clone(),
            host: effective_host,
            status: score_to_status(score).into(),
            score,
            checks,
            checked_at: now,
            error: Some(format!("Port {} unreachable", effective_port)),
        };
    }

    // 3. TLS Handshake (skip for SFTP/FTP/WebDAV-on-HTTP)
    if should_check_tls(&req.protocol, effective_port) {
        let tls = check_tls(&effective_host, effective_port).await;
        // If TLS times out but TCP was open, the server likely uses a non-standard
        // TLS setup (e.g., MEGA, Jottacloud). Mark as "skip" instead of "fail"
        // to avoid penalizing reachable servers.
        let tls = if tls.status == "fail" && tcp_ok {
            if let Some(ms) = tls.latency_ms {
                if ms >= 4900.0 {
                    CheckDetail {
                        name: "tls_handshake".into(),
                        status: "skip".into(),
                        latency_ms: tls.latency_ms,
                        details: Some("Timeout: non-standard TLS (HEAD not supported)".into()),
                    }
                } else {
                    tls
                }
            } else {
                tls
            }
        } else {
            tls
        };
        checks.push(tls);
    }

    // 4. HTTP Response (for HTTP-based protocols and cloud APIs)
    // Use provider-specific probe URLs when available for accurate results
    if matches!(req.protocol.as_str(), "webdav" | "s3") || is_cloud {
        let is_cloud_like = is_cloud || req.protocol == "s3";
        let url = if let Some(probe) = cloud_probe_url(&req.protocol) {
            // Provider-specific lightweight health endpoint
            probe.to_string()
        } else if let Some(ref ep) = req.endpoint {
            let scheme = if effective_port == 80 {
                "http"
            } else {
                "https"
            };
            if ep.starts_with("http") {
                ep.clone()
            } else {
                format!("{}://{}", scheme, ep)
            }
        } else {
            let scheme = if effective_port == 80 {
                "http"
            } else {
                "https"
            };
            format!("{}://{}:{}/", scheme, effective_host, effective_port)
        };
        let http = check_http(&url, is_cloud_like).await;
        checks.push(http);
    }

    let score = calculate_score(&checks);
    HealthCheckResult {
        server_id: req.server_id.clone(),
        host: effective_host,
        status: score_to_status(score).into(),
        score,
        checks,
        checked_at: now,
        error: None,
    }
}

/// Strip scheme and path from host string
fn sanitize_host(host: &str) -> String {
    let h = host.replace("https://", "").replace("http://", "");
    h.split('/').next().unwrap_or(&h).to_string()
}

/// Parse host:port from a string, falling back to provided port or protocol default
fn parse_host_port(host: &str, fallback_port: u16, protocol: &str) -> (String, u16) {
    if let Some(idx) = host.rfind(':') {
        if let Ok(port) = host[idx + 1..].parse::<u16>() {
            return (host[..idx].to_string(), port);
        }
    }
    let port = if fallback_port > 0 {
        fallback_port
    } else {
        default_port(protocol)
    };
    (host.to_string(), port)
}

/// Check health of a single server
#[tauri::command]
pub async fn server_health_check(
    server_id: String,
    host: String,
    port: u16,
    protocol: String,
    endpoint: Option<String>,
) -> Result<HealthCheckResult, String> {
    let req = HealthCheckRequest {
        server_id,
        host,
        port,
        protocol,
        endpoint,
    };

    Ok(
        tokio::time::timeout(Duration::from_secs(30), run_health_check(&req))
            .await
            .unwrap_or_else(|_| HealthCheckResult {
                server_id: req.server_id.clone(),
                host: req.host.clone(),
                status: "error".into(),
                score: 0,
                checks: vec![],
                checked_at: chrono::Utc::now().to_rfc3339(),
                error: Some("Global timeout (30s)".into()),
            }),
    )
}

/// Check health of multiple servers in parallel
#[tauri::command]
pub async fn server_health_check_batch(
    servers: Vec<HealthCheckRequest>,
) -> Result<Vec<HealthCheckResult>, String> {
    let handles: Vec<_> = servers
        .into_iter()
        .map(|req| {
            tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(30), run_health_check(&req))
                    .await
                    .unwrap_or_else(|_| HealthCheckResult {
                        server_id: req.server_id.clone(),
                        host: req.host.clone(),
                        status: "error".into(),
                        score: 0,
                        checks: vec![],
                        checked_at: chrono::Utc::now().to_rfc3339(),
                        error: Some("Global timeout (30s)".into()),
                    })
            })
        })
        .collect();

    let mut results = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(r) => results.push(r),
            Err(e) => {
                tracing::error!("Health check task panicked: {}", e);
            }
        }
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, status: &str, latency_ms: Option<f64>) -> CheckDetail {
        CheckDetail {
            name: name.into(),
            status: status.into(),
            latency_ms,
            details: None,
        }
    }

    #[test]
    fn default_port_knows_every_common_protocol() {
        assert_eq!(default_port("ftp"), 21);
        assert_eq!(default_port("ftps"), 990);
        assert_eq!(default_port("sftp"), 22);
        assert_eq!(default_port("webdav"), 443);
        assert_eq!(default_port("s3"), 443);
        // unknown → https default
        assert_eq!(default_port("unknown-protocol"), 443);
    }

    #[test]
    fn is_cloud_protocol_matches_known_providers() {
        assert!(is_cloud_protocol("googledrive"));
        assert!(is_cloud_protocol("dropbox"));
        assert!(is_cloud_protocol("onedrive"));
        assert!(is_cloud_protocol("mega"));
        assert!(!is_cloud_protocol("ftp"));
        assert!(!is_cloud_protocol("sftp"));
        assert!(!is_cloud_protocol(""));
    }

    #[test]
    fn cloud_api_host_maps_protocol_to_stable_hostname() {
        assert_eq!(cloud_api_host("googledrive"), Some("www.googleapis.com"));
        assert_eq!(cloud_api_host("dropbox"), Some("api.dropboxapi.com"));
        assert_eq!(cloud_api_host("onedrive"), Some("graph.microsoft.com"));
        assert_eq!(cloud_api_host("ftp"), None);
    }

    #[test]
    fn cloud_probe_url_is_https_for_known_providers() {
        for protocol in ["googledrive", "dropbox", "onedrive"] {
            let url = cloud_probe_url(protocol).expect("probe url should exist");
            assert!(
                url.starts_with("https://"),
                "{} probe url must be https",
                protocol
            );
        }
    }

    #[test]
    fn should_check_tls_respects_protocol_and_port() {
        // SSH-based protocols never get TLS checks
        assert!(!should_check_tls("sftp", 22));
        // Plain FTP: no TLS
        assert!(!should_check_tls("ftp", 21));
        // Implicit FTPS: yes
        assert!(should_check_tls("ftps", 990));
        // WebDAV on 443: yes; on non-443: plaintext assumption
        assert!(should_check_tls("webdav", 443));
        assert!(!should_check_tls("webdav", 8080));
        // S3 endpoints always TLS
        assert!(should_check_tls("s3", 443));
        // Known cloud protocols
        assert!(should_check_tls("googledrive", 443));
    }

    #[test]
    fn is_reachable_status_accepts_expected_codes() {
        use reqwest::StatusCode;
        assert!(is_reachable_status(StatusCode::OK, false));
        assert!(is_reachable_status(StatusCode::MOVED_PERMANENTLY, false));
        // Auth-required codes are still "server is up"
        assert!(is_reachable_status(StatusCode::UNAUTHORIZED, false));
        assert!(is_reachable_status(StatusCode::FORBIDDEN, false));
        assert!(is_reachable_status(StatusCode::METHOD_NOT_ALLOWED, false));
        // Cloud-only leniency for 400/404/429
        assert!(is_reachable_status(StatusCode::BAD_REQUEST, true));
        assert!(!is_reachable_status(StatusCode::BAD_REQUEST, false));
        assert!(is_reachable_status(StatusCode::NOT_FOUND, true));
        assert!(!is_reachable_status(StatusCode::NOT_FOUND, false));
        assert!(is_reachable_status(StatusCode::TOO_MANY_REQUESTS, true));
        // Hard 5xx: not reachable
        assert!(!is_reachable_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            true
        ));
    }

    #[test]
    fn calculate_score_returns_zero_on_fatal_dns_or_tcp_failure() {
        let checks = vec![
            check("dns_resolution", "fail", None),
            check("tcp_connect", "pass", Some(50.0)),
        ];
        assert_eq!(calculate_score(&checks), 0);

        let checks = vec![
            check("dns_resolution", "pass", Some(20.0)),
            check("tcp_connect", "fail", None),
        ];
        assert_eq!(calculate_score(&checks), 0);
    }

    #[test]
    fn calculate_score_penalizes_slow_responses() {
        let fast = vec![
            check("dns_resolution", "pass", Some(20.0)),
            check("tcp_connect", "pass", Some(50.0)),
            check("http_response", "pass", Some(100.0)),
        ];
        let slow = vec![
            check("dns_resolution", "pass", Some(20.0)),
            check("tcp_connect", "pass", Some(50.0)),
            check("http_response", "pass", Some(1500.0)),
        ];
        assert!(calculate_score(&fast) > calculate_score(&slow));
    }

    #[test]
    fn calculate_score_all_pass_fast_is_perfect() {
        let checks = vec![
            check("dns_resolution", "pass", Some(10.0)),
            check("tcp_connect", "pass", Some(20.0)),
            check("tls_handshake", "pass", Some(50.0)),
            check("http_response", "pass", Some(80.0)),
        ];
        assert_eq!(calculate_score(&checks), 100);
    }

    #[test]
    fn calculate_score_clamps_at_zero() {
        // Multiple soft failures stacked should still clamp at 0
        let checks = vec![
            check("tls_handshake", "fail", None),
            check("http_response", "fail", None),
            check("extra_check", "fail", None),
            check("another", "fail", None),
        ];
        let score = calculate_score(&checks);
        assert!(score <= 100);
    }

    #[test]
    fn score_to_status_maps_thresholds_correctly() {
        assert_eq!(score_to_status(100), "healthy");
        assert_eq!(score_to_status(85), "healthy");
        assert_eq!(score_to_status(80), "healthy");
        assert_eq!(score_to_status(79), "degraded");
        assert_eq!(score_to_status(50), "degraded");
        assert_eq!(score_to_status(49), "degraded");
        assert_eq!(score_to_status(1), "degraded");
        assert_eq!(score_to_status(0), "unreachable");
    }

    #[test]
    fn sanitize_host_strips_scheme_and_path() {
        assert_eq!(sanitize_host("https://example.com"), "example.com");
        assert_eq!(sanitize_host("http://example.com/api/v3"), "example.com");
        assert_eq!(sanitize_host("example.com/folder/"), "example.com");
        assert_eq!(sanitize_host("example.com"), "example.com");
        assert_eq!(
            sanitize_host("https://storage.googleapis.com/bucket/path"),
            "storage.googleapis.com"
        );
    }

    #[test]
    fn parse_host_port_extracts_explicit_port() {
        let (h, p) = parse_host_port("example.com:2222", 22, "sftp");
        assert_eq!(h, "example.com");
        assert_eq!(p, 2222);
    }

    #[test]
    fn parse_host_port_falls_back_to_provided_port() {
        let (h, p) = parse_host_port("example.com", 8443, "webdav");
        assert_eq!(h, "example.com");
        assert_eq!(p, 8443);
    }

    #[test]
    fn parse_host_port_uses_protocol_default_when_no_port_given() {
        let (h, p) = parse_host_port("example.com", 0, "sftp");
        assert_eq!(h, "example.com");
        assert_eq!(p, 22);

        let (_, p) = parse_host_port("example.com", 0, "ftps");
        assert_eq!(p, 990);
    }

    #[test]
    fn parse_host_port_ignores_invalid_port_segment() {
        // When the part after ":" is not a valid u16, fall back to defaults.
        let (h, p) = parse_host_port("example.com:not-a-port", 443, "webdav");
        assert_eq!(h, "example.com:not-a-port");
        assert_eq!(p, 443);
    }
}
