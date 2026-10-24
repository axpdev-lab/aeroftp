//! A test provider whose existence probes can be made to fail, for the guards
//! that may write only after a probe answered "missing" (D8): `stat`, `size`
//! and `exists` fail with `probe_error` when it is set, `list` with
//! `list_error`, and every write is recorded so a test can assert none went
//! out.

use std::collections::HashMap;

use async_trait::async_trait;

use super::{ProviderError, ProviderType, RemoteEntry, StorageProvider};

pub(crate) struct ProbeFake {
    /// Files by absolute path, with their size.
    pub files: HashMap<String, u64>,
    pub probe_error: Option<fn() -> ProviderError>,
    pub list_error: Option<fn() -> ProviderError>,
    pub listing_authoritative: bool,
    pub lose_listing_authority: bool,
    /// `upload <path>`, `mkdir <path>`, `delete <path>`, `rename <from> <to>`.
    pub writes: Vec<String>,
    /// With folders on, `mkdir` follows a real filesystem: it refuses a
    /// folder whose parent is missing (NotFound) or that exists already
    /// (AlreadyExists). Off, every `mkdir` succeeds.
    pub folders: Option<std::collections::HashSet<String>>,
    /// `mkdir` of this path is refused (PermissionDenied).
    pub refuse_mkdir: Option<String>,
}

impl ProbeFake {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
            probe_error: None,
            list_error: None,
            listing_authoritative: true,
            lose_listing_authority: false,
            writes: Vec::new(),
            folders: None,
            refuse_mkdir: None,
        }
    }

    /// Folders on, with `existing` there already.
    pub fn with_folders(mut self, existing: &[&str]) -> Self {
        self.folders = Some(existing.iter().map(|path| path.to_string()).collect());
        self
    }

    pub fn has_folder(&self, path: &str) -> bool {
        self.folders
            .as_ref()
            .is_some_and(|folders| folders.contains(path))
    }

    pub fn with_file(mut self, path: &str, size: u64) -> Self {
        self.files.insert(path.to_string(), size);
        self
    }

    fn probe(&self, path: &str) -> Result<u64, ProviderError> {
        if let Some(error) = self.probe_error {
            return Err(error());
        }
        self.files
            .get(path)
            .copied()
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))
    }
}

#[async_trait]
impl StorageProvider for ProbeFake {
    fn listing_is_authoritative(&self) -> bool {
        self.listing_authoritative
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn provider_type(&self) -> ProviderType {
        ProviderType::S3
    }
    fn display_name(&self) -> String {
        "probe-fake".to_string()
    }
    async fn connect(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    fn is_connected(&self) -> bool {
        true
    }
    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if self.lose_listing_authority {
            self.listing_authoritative = false;
        }
        if let Some(error) = self.list_error {
            return Err(error());
        }
        let prefix = format!("{}/", path.trim_end_matches('/'));
        Ok(self
            .files
            .iter()
            .filter_map(|(p, size)| {
                let name = p.strip_prefix(&prefix)?;
                (!name.contains('/')).then(|| RemoteEntry::file(name.to_string(), p.clone(), *size))
            })
            .collect())
    }
    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok("/".to_string())
    }
    async fn cd(&mut self, _path: &str) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn download(
        &mut self,
        _remote_path: &str,
        _local_path: &str,
        _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("download".to_string()))
    }
    async fn download_to_bytes(&mut self, _remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        Err(ProviderError::NotSupported("download_to_bytes".to_string()))
    }
    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let size = std::fs::metadata(local_path).map(|m| m.len()).unwrap_or(0);
        self.files.insert(remote_path.to_string(), size);
        self.writes.push(format!("upload {remote_path}"));
        Ok(())
    }
    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.writes.push(format!("mkdir {path}"));
        if self.refuse_mkdir.as_deref() == Some(path) {
            return Err(ProviderError::PermissionDenied(path.to_string()));
        }
        let Some(folders) = self.folders.as_mut() else {
            return Ok(());
        };
        let parent = match path.rsplit_once('/') {
            Some(("", _)) | None => None,
            Some((parent, _)) => Some(parent.to_string()),
        };
        if parent.is_some_and(|parent| !folders.contains(&parent)) {
            return Err(ProviderError::NotFound(path.to_string()));
        }
        if !folders.insert(path.to_string()) {
            return Err(ProviderError::AlreadyExists(path.to_string()));
        }
        Ok(())
    }
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        self.files.remove(path);
        self.writes.push(format!("delete {path}"));
        Ok(())
    }
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.writes.push(format!("rmdir {path}"));
        Ok(())
    }
    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.writes.push(format!("rmdir_recursive {path}"));
        Ok(())
    }
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.writes.push(format!("rename {from} {to}"));
        Ok(())
    }
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let size = self.probe(path)?;
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        Ok(RemoteEntry::file(name, path.to_string(), size))
    }
    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        self.probe(path)
    }
    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.probe(path) {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }
    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok("probe-fake".to_string())
    }
}
