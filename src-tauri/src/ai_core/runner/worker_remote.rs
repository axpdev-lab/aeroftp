//! Provider-specific remote boundary for workers. Only exact, pinned S3
//! profiles can be resolved; paths stay inside the delegated key prefix.
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::worker_coordinator::{PreparedWorker, WorkerCoordinator};
use super::worker_credentials::PinnedServerSnapshot;
use crate::providers::StorageProvider;
use serde_json::{json, Value};
use std::time::Duration;

/// Resolve a single profile/secret snapshot. The provider factory consumes
/// this value directly; it must not call create_temp_provider or reread vault.
pub fn validate_exact_profile(
    coordinator: &WorkerCoordinator,
    child: &PreparedWorker,
    profile_id: &str,
) -> Result<(PinnedServerSnapshot, String), String> {
    if profile_id.is_empty()
        || profile_id.trim() != profile_id
        || profile_id.eq_ignore_ascii_case("active")
    {
        return Err("An exact saved profile ID is required".into());
    }
    coordinator.resolve_remote_snapshot(child, profile_id)
}

/// S3 object keys have no symlink traversal. Keep the granted root and the
/// model-supplied suffix in one unambiguous lexical namespace.
pub(crate) fn confined_s3_path(root: &str, relative: &str) -> Result<String, String> {
    if !root.starts_with('/')
        || root.len() > 4096
        || root.contains("//")
        || root.contains('%')
        || root.contains('\\')
        || root.chars().any(char::is_control)
        || root.split('/').any(|part| part == "." || part == "..")
        || relative.is_empty()
        || relative.len() > 4096
        || root.len().saturating_add(relative.len()).saturating_add(1) > 4096
        || relative.starts_with('/')
        || relative.contains("//")
        || relative.contains('%')
        || relative.contains('\\')
        || relative.contains(':')
        || relative.chars().any(char::is_control)
        || relative
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("Worker remote path escapes or obscures its granted root".into());
    }
    let root = root.trim_end_matches('/');
    Ok(format!("{root}/{relative}"))
}

pub(crate) fn confined_s3_list_path(root: &str, relative: &str) -> Result<String, String> {
    if relative.is_empty() {
        confined_s3_path(root, "scope-validation")?;
        return Ok(if root == "/" {
            "/".into()
        } else {
            root.trim_end_matches('/').into()
        });
    }
    confined_s3_path(root, relative)
}

pub(crate) fn saved_s3_root(initial_path: Option<&str>) -> Result<String, String> {
    // As given, as the app lists it: `team ` is its own prefix.
    let path = initial_path.unwrap_or("").trim_matches('/');
    let root = if path.is_empty() {
        "/".to_string()
    } else {
        format!("/{path}")
    };
    confined_s3_list_path(&root, "")?;
    Ok(root)
}

/// Each awaited network operation owns its provider until connection,
/// request and disconnect have quiesced. Timeout drops the transport before
/// the ledger step may be released.
fn provider_from_snapshot(
    mut snapshot: PinnedServerSnapshot,
) -> Result<Box<dyn StorageProvider>, String> {
    snapshot.config.password = Some(snapshot.secret.to_string());
    let created = crate::providers::ProviderFactory::create(&snapshot.config);
    snapshot.config.zeroize_password();
    let mut provider = created.map_err(|_| "Worker S3 provider creation failed")?;
    // The normal S3 connect probe parses an unbounded error body on 403.
    // Worker reads perform their own bounded requests, so skip that probe.
    let s3 = provider
        .as_any_mut()
        .downcast_mut::<crate::providers::S3Provider>()
        .ok_or("Worker provider is not S3")?;
    if s3.worker_endpoint_may_reconcile_bridge() {
        return Err("Worker S3 endpoint can change after its grant is pinned".into());
    }
    s3.set_no_check_bucket(true);
    Ok(provider)
}

pub(crate) async fn stat_s3(snapshot: PinnedServerSnapshot, path: &str) -> Result<Value, String> {
    let mut provider = provider_from_snapshot(snapshot)?;
    let operation = async {
        provider
            .connect()
            .await
            .map_err(|_| "Worker S3 connection failed")?;
        let result = provider
            .stat(path)
            .await
            .map_err(|_| "Worker S3 stat failed");
        let _ = provider.disconnect().await;
        result
    };
    let entry = tokio::time::timeout(Duration::from_secs(20), operation)
        .await
        .map_err(|_| "Worker S3 operation timed out")??;
    if entry.is_symlink || entry.path != path {
        return Err("Worker S3 response is outside the granted path".into());
    }
    Ok(json!({
        "path": path,
        "name": entry.name,
        "size": entry.size,
        "is_dir": entry.is_dir,
        "modified": entry.modified.as_deref().filter(|value| value.len() <= 64),
    }))
}

pub(crate) async fn read_s3(snapshot: PinnedServerSnapshot, path: &str) -> Result<Value, String> {
    const MAX_READ_BYTES: usize = 4096;
    let mut provider = provider_from_snapshot(snapshot)?;
    let operation = async {
        provider
            .connect()
            .await
            .map_err(|_| "Worker S3 connection failed")?;
        let result = async {
            let entry = provider.stat(path).await.map_err(|_| "Worker S3 stat failed")?;
            if entry.is_dir || entry.is_symlink || entry.path != path {
                return Err("Worker S3 read requires an exact file inside the grant");
            }
            let limit = entry.size.min((MAX_READ_BYTES + 1) as u64);
            let bytes = provider.read_range(path, 0, limit).await
                .map_err(|_| "Worker S3 bounded read failed")?;
            if bytes.len() > limit as usize {
                return Err("Worker S3 exceeded the bounded range");
            }
            let mut content = String::from_utf8_lossy(&bytes).into_owned();
            let mut truncated = entry.size > MAX_READ_BYTES as u64 || content.len() > MAX_READ_BYTES;
            if content.len() > MAX_READ_BYTES {
                let mut end = MAX_READ_BYTES;
                while !content.is_char_boundary(end) { end -= 1; }
                content.truncate(end);
                truncated = true;
            }
            Ok(json!({"path": path, "size": entry.size, "content": content, "truncated": truncated}))
        }.await;
        let _ = provider.disconnect().await;
        result
    };
    tokio::time::timeout(Duration::from_secs(20), operation)
        .await
        .map_err(|_| "Worker S3 operation timed out".to_string())?
        .map_err(str::to_owned)
}

pub(crate) async fn list_s3(snapshot: PinnedServerSnapshot, path: &str) -> Result<Value, String> {
    let mut provider = provider_from_snapshot(snapshot)?;
    let operation = async {
        provider
            .connect()
            .await
            .map_err(|_| "Worker S3 connection failed")?;
        let result = async {
            let s3 = provider
                .as_any_mut()
                .downcast_mut::<crate::providers::S3Provider>()
                .ok_or("Worker S3 provider type changed")?;
            let (entries, mut truncated) = s3
                .list_worker_capped(path, 20)
                .await
                .map_err(|_| "Worker S3 bounded list failed")?;
            let mut selected = Vec::new();
            let prefix = format!("{}/", path.trim_end_matches('/'));
            for entry in entries {
                if entry.name.len() > 256
                    || entry.path.len() > 4096
                    || !entry.path.starts_with(&prefix)
                    || entry.is_symlink
                {
                    return Err("Worker S3 list returned an invalid entry");
                }
                let item = json!({
                    "name": entry.name,
                    "path": entry.path,
                    "is_dir": entry.is_dir,
                    "size": entry.size,
                });
                selected.push(item);
                if serde_json::to_vec(&selected)
                    .map_err(|_| "Worker S3 list encoding failed")?
                    .len()
                    > 4096
                {
                    selected.pop();
                    truncated = true;
                    break;
                }
            }
            Ok(json!({"path": path, "entries": selected, "truncated": truncated}))
        }
        .await;
        let _ = provider.disconnect().await;
        result
    };
    tokio::time::timeout(Duration::from_secs(20), operation)
        .await
        .map_err(|_| "Worker S3 operation timed out".to_string())?
        .map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_core::runner::worker_credentials::ServerPin;
    use crate::providers::{ProviderConfig, ProviderType};
    use std::collections::HashMap;
    use zeroize::Zeroizing;
    #[test]
    fn s3_paths_stay_under_the_exact_grant() {
        assert_eq!(
            confined_s3_path("/team/a", "notes/readme.txt").unwrap(),
            "/team/a/notes/readme.txt"
        );
        assert_eq!(confined_s3_path("/", "readme.txt").unwrap(), "/readme.txt");
        assert_eq!(confined_s3_list_path("/team/a", "").unwrap(), "/team/a");
        assert_eq!(confined_s3_list_path("/", "").unwrap(), "/");
        assert_eq!(saved_s3_root(Some("/team/a/")).unwrap(), "/team/a");
        assert_eq!(saved_s3_root(Some("/team/a ")).unwrap(), "/team/a ");
        assert_eq!(saved_s3_root(None).unwrap(), "/");
        assert!(saved_s3_root(Some("../other")).is_err());
        for path in [
            "",
            "/other",
            "../other",
            "a/../other",
            "./x",
            "a//b",
            "a\\b",
            "a%2fb",
            "a:b",
            "a\0b",
            "a\nb",
        ] {
            assert!(confined_s3_path("/team/a", path).is_err(), "{path:?}");
        }
        for root in ["team", "/team/../x", "/team//a", "/team%2fa", "/team\\a"] {
            assert!(confined_s3_path(root, "x").is_err(), "{root:?}");
        }
    }

    async fn s3_fixture(body: &'static str) -> String {
        let app = axum::Router::new().fallback(axum::routing::any(
            move |request: axum::extract::Request| async move {
                if request.method() == axum::http::Method::HEAD {
                    return axum::response::Response::builder()
                        .status(200)
                        .header("content-length", body.len().to_string())
                        .body(axum::body::Body::empty())
                        .unwrap();
                }
                axum::response::Response::builder()
                    .status(206)
                    .header(
                        "content-range",
                        format!("bytes 0-{}/{}", body.len() - 1, body.len()),
                    )
                    .body(axum::body::Body::from(body))
                    .unwrap()
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        endpoint
    }

    fn snapshot(id: &str, endpoint: String, secret: &str) -> PinnedServerSnapshot {
        PinnedServerSnapshot {
            pin: ServerPin {
                profile_id: id.into(),
                revision: "pinned".into(),
            },
            config: ProviderConfig {
                name: id.into(),
                provider_type: ProviderType::S3,
                host: endpoint,
                port: None,
                username: Some(id.into()),
                password: None,
                initial_path: None,
                extra: HashMap::from([
                    ("bucket".into(), "fixture".into()),
                    ("region".into(), "us-east-1".into()),
                ]),
            },
            secret: Zeroizing::new(secret.into()),
        }
    }

    #[tokio::test]
    async fn distinct_pinned_s3_profiles_cannot_cross_read_responses() {
        let first = s3_fixture("alpha").await;
        let second = s3_fixture("bravo").await;
        let a = read_s3(snapshot("one", first, "secret-a"), "/safe/note.txt")
            .await
            .unwrap();
        let b = read_s3(snapshot("two", second, "secret-b"), "/safe/note.txt")
            .await
            .unwrap();
        assert_eq!(a["content"], "alpha");
        assert_eq!(b["content"], "bravo");
        assert!(!a.to_string().contains("secret-a"));
        assert!(!b.to_string().contains("secret-b"));
    }

    #[test]
    fn reconcilable_bridge_endpoints_are_rejected_before_connect() {
        for endpoint in [
            "https://[::1]:1800",
            "https://localhost:1800",
            "http://127.0.0.1:1800",
        ] {
            assert!(
                provider_from_snapshot(snapshot("custom-s3", endpoint.into(), "secret")).is_err(),
                "{endpoint}"
            );
        }
    }
}
