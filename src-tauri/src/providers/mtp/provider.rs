//! `MtpProvider`: StorageProvider adapter over an [`MtpBackend`].
//!
//! Phase 1: Null backend + honest transfer capabilities. Path/cwd plumbing is
//! real so Phase 2/3 only replace the backend. See APPENDIX-MTP.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::HashMap;
use std::path::Path;

use async_trait::async_trait;

use crate::providers::mtp::backend::{
    MtpBackend, MtpObject, MtpObjectId, MtpStorage, NullMtpBackend,
};
use crate::providers::mtp::path::{
    join_virtual, leaf_name, normalize_virtual_path, parent_path, sanitize_leaf_for_download,
    split_segments,
};
use crate::providers::types::{ProviderError, ProviderType, RemoteEntry};
use crate::providers::StorageProvider;
use crate::transfer_dag::{Capability, TransferCapabilities};

/// Cap for `download_to_bytes` materialization (10 MiB). Larger objects must
/// stream to disk via `download`.
const BYTES_CAP: u64 = 10 * 1024 * 1024;

/// Session-scoped MTP provider (not created via the profile factory).
pub struct MtpProvider {
    backend: Box<dyn MtpBackend>,
    device_id: Option<String>,
    display_name: String,
    cwd: String,
    /// Virtual path -> object id (files and folders). Storages use a synthetic
    /// handle of `"storage:{storage_id}"` with matching storage_id.
    path_cache: HashMap<String, MtpObjectId>,
    storages: Vec<MtpStorage>,
}

impl MtpProvider {
    pub fn new(backend: Box<dyn MtpBackend>) -> Self {
        Self {
            backend,
            device_id: None,
            display_name: "Portable device".to_string(),
            cwd: "/".to_string(),
            path_cache: HashMap::new(),
            storages: Vec::new(),
        }
    }

    /// Scaffold constructor used until platform backends are selected by OS.
    pub fn with_null_backend() -> Self {
        Self::new(Box::new(NullMtpBackend::new()))
    }

    /// Open a specific device id (call after construction).
    pub async fn open_device(&mut self, device_id: &str) -> Result<(), ProviderError> {
        self.backend.open(device_id).await?;
        self.device_id = Some(device_id.to_string());
        if let Some(name) = self.backend.device_display_name() {
            self.display_name = name;
        }
        self.cwd = "/".to_string();
        self.path_cache.clear();
        self.storages.clear();
        Ok(())
    }

    fn require_open(&self) -> Result<(), ProviderError> {
        if self.backend.is_open() {
            Ok(())
        } else {
            Err(ProviderError::NotConnected)
        }
    }

    /// Honest capability surface: whole-file only, single slot, no resume.
    pub fn honest_transfer_capabilities() -> TransferCapabilities {
        TransferCapabilities {
            file_parallel: Capability::Unsupported,
            session_pool: Capability::Unsupported,
            strict_concurrent_range_download: Capability::Unsupported,
            resume_download: Capability::Unsupported,
            resume_upload: Capability::Unsupported,
            multipart_upload: Capability::Unsupported,
            offset_upload: Capability::Unsupported,
            upload_session: Capability::Unsupported,
            server_side_copy: Capability::Unsupported,
            list_parallel: Capability::Unsupported,
            batch_list: Capability::Unsupported,
            server_checksum: Capability::Unsupported,
            atomic_rename: Capability::Unsupported,
            rate_limited_api: Capability::Unsupported,
            max_file_slots: Some(1),
            max_chunk_slots: Some(1),
            max_checker_slots: Some(1),
            preferred_chunk_size: None,
            multipart_threshold: u64::MAX,
        }
    }

    fn object_to_entry(obj: &MtpObject, parent_vpath: &str) -> Result<RemoteEntry, ProviderError> {
        let path = join_virtual(parent_vpath, &obj.name)?;
        let mut entry = if obj.is_dir {
            RemoteEntry::directory(obj.name.clone(), path)
        } else {
            RemoteEntry::file(obj.name.clone(), path, obj.size)
        };
        entry.modified = obj.modified.clone();
        Ok(entry)
    }

    async fn ensure_storages(&mut self) -> Result<(), ProviderError> {
        self.require_open()?;
        if self.storages.is_empty() {
            self.storages = self.backend.list_storages().await?;
            for s in &self.storages {
                let vpath = join_virtual("/", &s.display_name)?;
                self.path_cache.insert(
                    vpath,
                    MtpObjectId {
                        storage_id: s.storage_id.clone(),
                        handle: format!("storage:{}", s.storage_id),
                    },
                );
            }
        }
        Ok(())
    }

    async fn resolve_id(&mut self, vpath: &str) -> Result<Option<MtpObjectId>, ProviderError> {
        let norm = normalize_virtual_path(vpath)?;
        if norm == "/" {
            return Ok(None);
        }
        if let Some(id) = self.path_cache.get(&norm) {
            return Ok(Some(id.clone()));
        }
        // Walk from root, listing each level (slow but correct after reconnect).
        let segments = split_segments(&norm)?;
        if segments.is_empty() {
            return Ok(None);
        }
        self.ensure_storages().await?;
        let storage_name = segments[0].as_str();
        let storage = self
            .storages
            .iter()
            .find(|s| s.display_name == storage_name || s.storage_id == storage_name)
            .ok_or_else(|| ProviderError::NotFound(format!("storage {storage_name}")))?
            .clone();
        let mut current_path = join_virtual("/", &storage.display_name)?;
        let mut current_id: Option<MtpObjectId> = Some(MtpObjectId {
            storage_id: storage.storage_id.clone(),
            handle: format!("storage:{}", storage.storage_id),
        });
        self.path_cache
            .insert(current_path.clone(), current_id.clone().unwrap());

        for seg in segments.iter().skip(1) {
            let parent_for_list = current_id
                .as_ref()
                .filter(|id| !id.handle.starts_with("storage:"));
            let storage_id = Some(storage.storage_id.as_str());
            let kids = self
                .backend
                .list_objects(parent_for_list, storage_id)
                .await?;
            let found = kids
                .into_iter()
                .find(|o| o.name == *seg)
                .ok_or_else(|| ProviderError::NotFound(format!("{current_path}/{seg}")))?;
            current_path = join_virtual(&current_path, &found.name)?;
            self.path_cache
                .insert(current_path.clone(), found.id.clone());
            current_id = Some(found.id);
        }
        Ok(current_id)
    }
}

#[async_trait]
impl StorageProvider for MtpProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Mtp
    }

    fn display_name(&self) -> String {
        self.display_name.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        if let Some(id) = self.device_id.clone() {
            self.open_device(&id).await
        } else {
            // No device chosen yet: stay disconnected until open_device.
            Err(ProviderError::InvalidConfig(
                "MTP connect requires open_device(device_id) first".to_string(),
            ))
        }
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.backend.close().await?;
        self.path_cache.clear();
        self.storages.clear();
        self.cwd = "/".to_string();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.backend.is_open()
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        self.require_open()?;
        let norm = if path.is_empty() {
            self.cwd.clone()
        } else {
            normalize_virtual_path(path)?
        };

        if norm == "/" {
            self.ensure_storages().await?;
            return Ok(self
                .storages
                .iter()
                .map(|s| {
                    let p = format!("/{}", s.display_name);
                    RemoteEntry::directory(s.display_name.clone(), p)
                })
                .collect());
        }

        let segments = split_segments(&norm)?;
        if segments.len() == 1 {
            // Storage root: list with storage_id, no parent object.
            self.ensure_storages().await?;
            let storage_name = segments[0].as_str();
            let storage = self
                .storages
                .iter()
                .find(|s| s.display_name == storage_name || s.storage_id == storage_name)
                .ok_or_else(|| ProviderError::NotFound(norm.clone()))?
                .clone();
            let kids = self
                .backend
                .list_objects(None, Some(&storage.storage_id))
                .await?;
            let mut out = Vec::with_capacity(kids.len());
            for obj in kids {
                let entry = Self::object_to_entry(&obj, &norm)?;
                self.path_cache.insert(entry.path.clone(), obj.id);
                out.push(entry);
            }
            return Ok(out);
        }

        let id = self
            .resolve_id(&norm)
            .await?
            .ok_or_else(|| ProviderError::NotFound(norm.clone()))?;
        if id.handle.starts_with("storage:") {
            let kids = self
                .backend
                .list_objects(None, Some(&id.storage_id))
                .await?;
            let mut out = Vec::with_capacity(kids.len());
            for obj in kids {
                let entry = Self::object_to_entry(&obj, &norm)?;
                self.path_cache.insert(entry.path.clone(), obj.id);
                out.push(entry);
            }
            return Ok(out);
        }
        let kids = self
            .backend
            .list_objects(Some(&id), Some(&id.storage_id))
            .await?;
        let mut out = Vec::with_capacity(kids.len());
        for obj in kids {
            let entry = Self::object_to_entry(&obj, &norm)?;
            self.path_cache.insert(entry.path.clone(), obj.id);
            out.push(entry);
        }
        Ok(out)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        self.require_open()?;
        Ok(self.cwd.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        self.require_open()?;
        let target = if path == ".." {
            parent_path(&self.cwd)?
        } else if path.starts_with('/') {
            normalize_virtual_path(path)?
        } else {
            join_virtual(&self.cwd, path)?
        };
        if target == "/" {
            self.cwd = "/".to_string();
            return Ok(());
        }
        // Ensure path exists as storage or folder.
        let segs = split_segments(&target)?;
        if segs.len() == 1 {
            self.ensure_storages().await?;
            let name = segs[0].as_str();
            let ok = self
                .storages
                .iter()
                .any(|s| s.display_name == name || s.storage_id == name);
            if !ok {
                return Err(ProviderError::NotFound(target));
            }
            self.cwd = target;
            return Ok(());
        }
        let id = self
            .resolve_id(&target)
            .await?
            .ok_or_else(|| ProviderError::NotFound(target.clone()))?;
        // Folders and storage roots are cd-able; files are not (backend list will tell).
        let _ = id;
        self.cwd = target;
        Ok(())
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
        self.require_open()?;
        let norm = normalize_virtual_path(remote_path)?;
        let id = self
            .resolve_id(&norm)
            .await?
            .ok_or_else(|| ProviderError::NotFound(norm.clone()))?;
        if id.handle.starts_with("storage:") {
            return Err(ProviderError::InvalidPath(format!(
                "{norm} is a storage root, not a file"
            )));
        }
        let dest = Path::new(local_path);
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(ProviderError::IoError)?;
            }
        }
        self.backend.get_object(&id, dest, on_progress).await
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        self.download_to_bytes_capped(remote_path, BYTES_CAP).await
    }

    async fn download_to_bytes_capped(
        &mut self,
        remote_path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        self.require_open()?;
        let tmp = tempfile::NamedTempFile::new().map_err(ProviderError::IoError)?;
        let tmp_path = tmp.path().to_path_buf();
        self.download(remote_path, tmp_path.to_str().unwrap_or(""), None)
            .await?;
        let data = std::fs::read(&tmp_path).map_err(ProviderError::IoError)?;
        if data.len() as u64 > max_bytes {
            return Err(ProviderError::TransferFailed(format!(
                "MTP object exceeded the {:.0} MB in-memory cap (stream to disk instead).",
                max_bytes as f64 / 1_048_576.0,
            )));
        }
        Ok(data)
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.require_open()?;
        let norm = normalize_virtual_path(remote_path)?;
        let name = leaf_name(&norm)?;
        if name.is_empty() {
            return Err(ProviderError::InvalidPath(
                "upload remote path must include a file name".to_string(),
            ));
        }
        let parent = parent_path(&norm)?;
        let segs = split_segments(&parent)?;
        if segs.is_empty() {
            return Err(ProviderError::InvalidPath(
                "cannot upload to device root; pick a storage first".to_string(),
            ));
        }
        self.ensure_storages().await?;
        let storage_key = segs[0].as_str();
        let storage = self
            .storages
            .iter()
            .find(|s| s.display_name == storage_key || s.storage_id == storage_key)
            .ok_or_else(|| ProviderError::NotFound(parent.clone()))?
            .clone();

        let parent_id = if segs.len() == 1 {
            None
        } else {
            self.resolve_id(&parent).await?
        };
        let parent_ref = parent_id
            .as_ref()
            .filter(|id| !id.handle.starts_with("storage:"));

        let safe_name = sanitize_leaf_for_download(&name);
        let id = self
            .backend
            .send_object(
                parent_ref,
                &storage.storage_id,
                Path::new(local_path),
                &safe_name,
                on_progress,
            )
            .await?;
        self.path_cache.insert(norm, id);
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.require_open()?;
        let norm = normalize_virtual_path(path)?;
        let name = leaf_name(&norm)?;
        let parent = parent_path(&norm)?;
        let segs = split_segments(&parent)?;
        if segs.is_empty() {
            return Err(ProviderError::InvalidPath(
                "cannot create a folder at device root; pick a storage first".to_string(),
            ));
        }
        self.ensure_storages().await?;
        let storage_key = segs[0].as_str();
        let storage = self
            .storages
            .iter()
            .find(|s| s.display_name == storage_key || s.storage_id == storage_key)
            .ok_or_else(|| ProviderError::NotFound(parent.clone()))?
            .clone();
        let parent_id = if segs.len() == 1 {
            None
        } else {
            self.resolve_id(&parent).await?
        };
        let parent_ref = parent_id
            .as_ref()
            .filter(|id| !id.handle.starts_with("storage:"));
        let id = self
            .backend
            .create_folder(parent_ref, &storage.storage_id, &name)
            .await?;
        self.path_cache.insert(norm, id);
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        self.require_open()?;
        let norm = normalize_virtual_path(path)?;
        let id = self
            .resolve_id(&norm)
            .await?
            .ok_or_else(|| ProviderError::NotFound(norm.clone()))?;
        if id.handle.starts_with("storage:") {
            return Err(ProviderError::NotSupported(
                "cannot delete a storage root".to_string(),
            ));
        }
        self.backend.delete_object(&id).await?;
        self.path_cache.remove(&norm);
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // Client-side walk: list children, recurse, then delete folder.
        let norm = normalize_virtual_path(path)?;
        let children = self.list(&norm).await?;
        for child in children {
            if child.is_walkable_dir() {
                Box::pin(self.rmdir_recursive(&child.path)).await?;
            } else {
                self.delete(&child.path).await?;
            }
        }
        self.delete(&norm).await
    }

    async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported(
            "MTP rename is not available in this build (device support varies)".to_string(),
        ))
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        self.require_open()?;
        let norm = normalize_virtual_path(path)?;
        if norm == "/" {
            return Ok(RemoteEntry::directory(
                self.display_name.clone(),
                "/".to_string(),
            ));
        }
        let parent = parent_path(&norm)?;
        let name = leaf_name(&norm)?;
        let siblings = self.list(&parent).await?;
        siblings
            .into_iter()
            .find(|e| e.name == name)
            .ok_or(ProviderError::NotFound(norm))
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        Ok(self.stat(path).await?.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        // USB has no noop; re-probe open state only.
        self.require_open()
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!(
            "MTP portable device: {} ({})",
            self.display_name,
            self.device_id.as_deref().unwrap_or("unopened")
        ))
    }

    fn transfer_capabilities(&self) -> TransferCapabilities {
        Self::honest_transfer_capabilities()
    }

    fn supports_delta_sync(&self) -> bool {
        false
    }

    fn supports_resume(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::types::{ProviderConfig, ProviderType};
    use crate::providers::ProviderFactory;

    #[test]
    fn capabilities_are_honest() {
        let caps = MtpProvider::honest_transfer_capabilities();
        assert_eq!(caps.resume_download, Capability::Unsupported);
        assert_eq!(caps.resume_upload, Capability::Unsupported);
        assert_eq!(caps.multipart_upload, Capability::Unsupported);
        assert_eq!(
            caps.strict_concurrent_range_download,
            Capability::Unsupported
        );
        assert_eq!(caps.server_side_copy, Capability::Unsupported);
        assert_eq!(caps.file_parallel, Capability::Unsupported);
        assert_eq!(caps.session_pool, Capability::Unsupported);
        assert_eq!(caps.max_file_slots, Some(1));
        assert_eq!(caps.max_chunk_slots, Some(1));
        assert_eq!(caps.multipart_threshold, u64::MAX);
    }

    #[test]
    fn factory_rejects_mtp_profile() {
        let cfg = ProviderConfig {
            name: "phone".to_string(),
            provider_type: ProviderType::Mtp,
            host: String::new(),
            port: None,
            username: None,
            password: None,
            initial_path: None,
            extra: Default::default(),
        };
        match ProviderFactory::create(&cfg) {
            Ok(_) => panic!("factory must reject ProviderType::Mtp profiles"),
            Err(err) => {
                assert!(matches!(err, ProviderError::InvalidConfig(_)));
                let msg = err.to_string();
                assert!(
                    msg.contains("PLACES") || msg.contains("MTP"),
                    "unexpected message: {msg}"
                );
            }
        }
    }

    #[test]
    fn provider_type_and_display() {
        let p = MtpProvider::with_null_backend();
        assert_eq!(p.provider_type(), ProviderType::Mtp);
        assert!(!p.is_connected());
        assert_eq!(format!("{}", ProviderType::Mtp), "MTP");
        assert_eq!(ProviderType::Mtp.default_port(), 0);
        assert_eq!(ProviderType::from_lowercase("mtp"), Some(ProviderType::Mtp));
        assert_eq!(ProviderType::from_lowercase("wpd"), Some(ProviderType::Mtp));
    }

    #[tokio::test]
    async fn open_device_connects_null_backend() {
        let mut p = MtpProvider::with_null_backend();
        p.open_device("dev-1").await.unwrap();
        assert!(p.is_connected());
        // list root needs list_storages which is NotSupported on null
        let err = p.list("/").await.unwrap_err();
        assert!(matches!(err, ProviderError::NotSupported(_)));
        p.disconnect().await.unwrap();
        assert!(!p.is_connected());
    }
}
