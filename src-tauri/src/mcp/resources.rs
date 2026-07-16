//! MCP resource definitions and handlers
//!
//! 5 resources:
//! - `aeroftp://profiles`: catalog of saved profiles (no credentials)
//! - `aeroftp://profiles/status`: vault availability status
//! - `aeroftp://profiles/{id}`: individual profile detail
//! - `aeroftp://capabilities`: supported protocols and transfer capabilities
//! - `aeroftp://connections`: active pooled connections and their state

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::mcp::pool::ConnectionPool;
use crate::providers::ProviderType;
use serde_json::{json, Value};

/// Build the resource list for `resources/list`.
pub fn resource_list(profiles: &[Value], vault_error: &Option<String>) -> Vec<Value> {
    let mut resources = vec![
        json!({
            "uri": "aeroftp://profiles",
            "name": "AeroFTP saved profiles",
            "description": "Safe list of saved AeroFTP profiles without credentials",
            "mimeType": "application/json",
        }),
        json!({
            "uri": "aeroftp://profiles/status",
            "name": "AeroFTP profiles status",
            "description": "Availability status for saved profile resources",
            "mimeType": "application/json",
        }),
        json!({
            "uri": "aeroftp://capabilities",
            "name": "AeroFTP protocol capabilities",
            "description": "Supported protocols with transfer capability limits",
            "mimeType": "application/json",
        }),
        json!({
            "uri": "aeroftp://connections",
            "name": "AeroFTP active connections",
            "description": "Currently pooled server connections and their idle status",
            "mimeType": "application/json",
        }),
    ];

    // Individual profile resources
    for profile in profiles {
        let name = profile
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed");
        let id = profile.get("id").and_then(|v| v.as_str()).unwrap_or(name);
        resources.push(json!({
            "uri": format!("aeroftp://profiles/{}", id),
            "name": format!("AeroFTP profile: {}", name),
            "description": format!("Saved AeroFTP profile for {}", name),
            "mimeType": "application/json",
        }));
    }

    let _ = vault_error; // used for context, not filtered here
    resources
}

/// Resource templates for `resources/templates/list` (MCP 2024-11-05).
pub fn resource_templates() -> Vec<Value> {
    vec![json!({
        "uriTemplate": "aeroftp://profiles/{profile_id}",
        "name": "AeroFTP profile by ID",
        "description": "Look up a specific saved server profile by its ID",
        "mimeType": "application/json",
    })]
}

/// Read a resource by URI. Returns `Some((mime, text))` or `None` if not found.
pub async fn read_resource(
    uri: &str,
    profiles: &[Value],
    vault_error: &Option<String>,
    pool: &ConnectionPool,
) -> Option<(String, String)> {
    let mime = "application/json".to_string();

    if uri == "aeroftp://profiles" {
        let text = serde_json::to_string_pretty(&json!({
            "status": if vault_error.is_some() { "unavailable" } else { "ok" },
            "error": vault_error,
            "profiles": profiles
                .iter()
                .map(profile_with_transfer_capabilities)
                .collect::<Vec<_>>(),
        }))
        .unwrap_or_default();
        return Some((mime, text));
    }

    if uri == "aeroftp://profiles/status" {
        let text = serde_json::to_string_pretty(&json!({
            "status": if vault_error.is_some() { "unavailable" } else { "ok" },
            "error": vault_error,
            "count": profiles.len(),
        }))
        .unwrap_or_default();
        return Some((mime, text));
    }

    if uri == "aeroftp://capabilities" {
        let text = serde_json::to_string_pretty(&capabilities()).unwrap_or_default();
        return Some((mime, text));
    }

    if uri == "aeroftp://connections" {
        let conns = pool.status().await;
        let text = serde_json::to_string_pretty(&json!({
            "connections": conns,
            "count": conns.len(),
            "max_pool_size": pool.max_size(),
            "idle_timeout_secs": pool.idle_timeout().as_secs(),
        }))
        .unwrap_or_default();
        return Some((mime, text));
    }

    if let Some(profile_id) = uri.strip_prefix("aeroftp://profiles/") {
        let profile = profiles
            .iter()
            .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(profile_id));
        return profile.map(|p| {
            (
                mime,
                serde_json::to_string_pretty(&profile_with_transfer_capabilities(p))
                    .unwrap_or_default(),
            )
        });
    }

    None
}

/// Protocol capabilities matrix.
fn capabilities() -> Value {
    let protocols = vec![
        proto_cap("FTP", ProviderType::Ftp),
        proto_cap("FTPS", ProviderType::Ftps),
        proto_cap("SFTP", ProviderType::Sftp),
        proto_cap("WebDAV", ProviderType::WebDav),
        proto_cap("S3", ProviderType::S3),
        proto_cap("Google Drive", ProviderType::GoogleDrive),
        proto_cap("Dropbox", ProviderType::Dropbox),
        proto_cap("OneDrive", ProviderType::OneDrive),
        proto_cap("MEGA", ProviderType::Mega),
        proto_cap("Box", ProviderType::Box),
        proto_cap("pCloud", ProviderType::PCloud),
        proto_cap("Azure Blob", ProviderType::Azure),
        proto_cap("Filen", ProviderType::Filen),
        proto_cap("Internxt", ProviderType::Internxt),
        proto_cap("kDrive", ProviderType::KDrive),
        proto_cap("Jottacloud", ProviderType::Jottacloud),
        proto_cap("DrimeCloud", ProviderType::DrimeCloud),
        proto_cap("FileLu", ProviderType::FileLu),
        proto_cap("Koofr", ProviderType::Koofr),
        proto_cap("OpenDrive", ProviderType::OpenDrive),
        proto_cap("Yandex Disk", ProviderType::YandexDisk),
        proto_cap("GitHub", ProviderType::GitHub),
        proto_cap("GitLab", ProviderType::GitLab),
        proto_cap("Swift", ProviderType::Swift),
        proto_cap("Zoho WorkDrive", ProviderType::ZohoWorkdrive),
        proto_cap("4shared", ProviderType::FourShared),
        proto_cap("Google Photos", ProviderType::GooglePhotos),
        proto_cap("Immich", ProviderType::Immich),
        proto_cap("ImageKit", ProviderType::ImageKit),
        proto_cap("Uploadcare", ProviderType::Uploadcare),
        proto_cap("Backblaze B2", ProviderType::Backblaze),
        proto_cap("Cloudinary", ProviderType::Cloudinary),
    ];

    json!({
        "total_protocols": protocols.len(),
        "protocols": protocols,
    })
}

fn proto_cap(name: &str, pt: ProviderType) -> Value {
    let feature_key = protocol_feature_key(pt);
    json!({
        "name": name,
        "requires_oauth2": pt.requires_oauth2(),
        "encrypted": pt.uses_encryption(),
        "transfer_capabilities": {
            "status": "ok",
            "source": "protocol_defaults",
            "capabilities": crate::agent_session::transfer_capabilities_for_provider_type(
                pt,
                feature_key,
            ),
        },
    })
}

fn protocol_feature_key(pt: ProviderType) -> &'static str {
    match pt {
        ProviderType::Ftp => "ftp",
        ProviderType::Ftps => "ftps",
        ProviderType::Sftp => "sftp",
        ProviderType::WebDav => "webdav",
        ProviderType::S3 => "s3",
        ProviderType::AeroCloud => "aerocloud",
        ProviderType::GoogleDrive => "googledrive",
        ProviderType::Dropbox => "dropbox",
        ProviderType::OneDrive => "onedrive",
        ProviderType::Mega => "mega",
        ProviderType::Box => "box",
        ProviderType::PCloud => "pcloud",
        ProviderType::Azure => "azure",
        ProviderType::Filen => "filen",
        ProviderType::FourShared => "fourshared",
        ProviderType::ZohoWorkdrive => "zohoworkdrive",
        ProviderType::Internxt => "internxt",
        ProviderType::KDrive => "kdrive",
        ProviderType::Jottacloud => "jottacloud",
        ProviderType::DrimeCloud => "drime",
        ProviderType::FileLu => "filelu",
        ProviderType::Koofr => "koofr",
        ProviderType::OpenDrive => "opendrive",
        ProviderType::YandexDisk => "yandexdisk",
        ProviderType::GitHub => "github",
        ProviderType::GitLab => "gitlab",
        ProviderType::Swift => "swift",
        ProviderType::GooglePhotos => "googlephotos",
        ProviderType::Immich => "immich",
        ProviderType::ImageKit => "imagekit",
        ProviderType::Uploadcare => "uploadcare",
        ProviderType::Backblaze => "backblaze",
        ProviderType::Cloudinary => "cloudinary",
        // Synthetic local mount provider; never reaches the MCP protocol table.
        ProviderType::AeroVaultMount => "aerovaultmount",
        ProviderType::Peer => "peer",
        ProviderType::Mtp => "mtp",
    }
}

fn profile_with_transfer_capabilities(profile: &Value) -> Value {
    let mut enriched = profile.clone();
    let protocol = profile
        .get("protocol")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    enriched["transfer_capabilities"] =
        crate::agent_session::transfer_capabilities_block(protocol, None, "profile_defaults");
    enriched
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_resource_includes_transfer_limits() {
        let caps = capabilities();
        let protocols = caps["protocols"].as_array().expect("protocols array");
        let b2 = protocols
            .iter()
            .find(|p| p["name"] == "Backblaze B2")
            .expect("Backblaze B2 protocol entry");
        assert_eq!(b2["transfer_capabilities"]["status"], "ok");
        assert_eq!(
            b2["transfer_capabilities"]["capabilities"]["multipart_upload"],
            "supported"
        );
        assert_eq!(
            b2["transfer_capabilities"]["capabilities"]["max_chunk_slots"],
            4
        );
    }

    #[test]
    fn profile_resource_includes_profile_default_transfer_capabilities() {
        let profile = json!({
            "id": "srv_1",
            "name": "Example",
            "protocol": "sftp",
        });
        let enriched = profile_with_transfer_capabilities(&profile);
        assert_eq!(enriched["transfer_capabilities"]["status"], "ok");
        assert_eq!(
            enriched["transfer_capabilities"]["source"],
            "profile_defaults"
        );
        assert_eq!(
            enriched["transfer_capabilities"]["capabilities"]["strict_concurrent_range_download"],
            "unsupported"
        );
    }
}
