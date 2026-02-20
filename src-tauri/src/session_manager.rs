//! Session Manager - Multi-session provider management
//!
//! This module provides session-based provider management, allowing multiple
//! concurrent connections to different providers (FTP, OAuth, WebDAV, S3, etc.)
//!
//! Each session has a unique ID that corresponds to the frontend's activeSessionId.

use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::providers::{StorageProvider, ProviderConfig, ProviderError, RemoteEntry};

/// Session information stored alongside the provider
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// Unique session identifier (matches frontend activeSessionId)
    pub session_id: String,
    /// Display name for the connection (e.g., "Google Drive", "user@ftp.example.com")
    pub display_name: String,
    /// Protocol type as string
    pub protocol: String,
    /// Current remote path
    pub current_path: String,
    /// Creation timestamp (for future session timeout/metrics)
    #[allow(dead_code)]
    pub created_at: std::time::Instant,
    /// Last activity timestamp
    pub last_activity: std::time::Instant,
}

/// A session wrapping a provider with its metadata
pub struct ProviderSession {
    pub info: SessionInfo,
    pub provider: Box<dyn StorageProvider>,
    /// Provider configuration (for reconnection/serialization)
    #[allow(dead_code)]
    pub config: Option<ProviderConfig>,
}

/// Multi-session state manager
/// 
/// Replaces the single-provider ProviderState with a HashMap that can
/// hold multiple concurrent provider connections.
pub struct MultiProviderState {
    /// Map of session_id -> provider session
    sessions: RwLock<HashMap<String, ProviderSession>>,
    /// Currently active session ID (for commands without explicit session_id)
    active_session_id: RwLock<Option<String>>,
}

impl MultiProviderState {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            active_session_id: RwLock::new(None),
        }
    }

    /// Create a new session with the given provider
    pub async fn create_session(
        &self,
        session_id: String,
        provider: Box<dyn StorageProvider>,
        config: Option<ProviderConfig>,
    ) -> Result<SessionInfo, ProviderError> {
        let display_name = provider.display_name();
        let protocol = format!("{:?}", provider.provider_type());
        
        let info = SessionInfo {
            session_id: session_id.clone(),
            display_name: display_name.clone(),
            protocol,
            current_path: "/".to_string(),
            created_at: std::time::Instant::now(),
            last_activity: std::time::Instant::now(),
        };

        let session = ProviderSession {
            info: info.clone(),
            provider,
            config,
        };

        let mut sessions = self.sessions.write().await;
        sessions.insert(session_id.clone(), session);
        
        // Set as active if it's the first session
        {
            let mut active = self.active_session_id.write().await;
            if active.is_none() {
                *active = Some(session_id.clone());
            }
        }

        info!("Created session {} for {}", session_id, display_name);
        Ok(info)
    }

    /// Get a mutable reference to a session's provider
    /// Returns None if session doesn't exist
    #[allow(dead_code)]
    pub async fn get_session_mut<F, R>(&self, session_id: &str, f: F) -> Result<R, ProviderError>
    where
        F: FnOnce(&mut ProviderSession) -> Result<R, ProviderError>,
    {
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(session_id)
            .ok_or(ProviderError::NotConnected)?;

        // Update last activity
        session.info.last_activity = std::time::Instant::now();

        f(session)
    }

    /// Get session info without mutable access
    pub async fn get_session_info(&self, session_id: &str) -> Option<SessionInfo> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).map(|s| s.info.clone())
    }

    /// Close and remove a session
    pub async fn close_session(&self, session_id: &str) -> Result<(), ProviderError> {
        let mut sessions = self.sessions.write().await;
        
        if let Some(mut session) = sessions.remove(session_id) {
            info!("Closing session {} ({})", session_id, session.info.display_name);
            
            // Disconnect the provider
            if let Err(e) = session.provider.disconnect().await {
                warn!("Error disconnecting session {}: {}", session_id, e);
            }
            
            // If this was the active session, clear or switch to another
            let mut active = self.active_session_id.write().await;
            if active.as_ref() == Some(&session_id.to_string()) {
                *active = sessions.keys().next().cloned();
            }
            
            Ok(())
        } else {
            Err(ProviderError::NotConnected)
        }
    }

    /// Set the active session
    pub async fn set_active_session(&self, session_id: &str) -> Result<(), ProviderError> {
        let sessions = self.sessions.read().await;
        if sessions.contains_key(session_id) {
            let mut active = self.active_session_id.write().await;
            *active = Some(session_id.to_string());
            info!("Switched active session to {}", session_id);
            Ok(())
        } else {
            Err(ProviderError::NotConnected)
        }
    }

    /// Get the active session ID
    pub async fn get_active_session_id(&self) -> Option<String> {
        self.active_session_id.read().await.clone()
    }

    /// Get the active session, or a specific session if session_id is provided
    /// This allows backwards compatibility with commands that don't pass session_id
    pub async fn resolve_session_id(&self, session_id: Option<&str>) -> Result<String, ProviderError> {
        match session_id {
            Some(id) => {
                let sessions = self.sessions.read().await;
                if sessions.contains_key(id) {
                    Ok(id.to_string())
                } else {
                    Err(ProviderError::NotConnected)
                }
            }
            None => {
                self.active_session_id.read().await
                    .clone()
                    .ok_or(ProviderError::NotConnected)
            }
        }
    }

    /// List all active sessions
    pub async fn list_sessions(&self) -> Vec<SessionInfo> {
        let sessions = self.sessions.read().await;
        sessions.values().map(|s| s.info.clone()).collect()
    }

    /// Get the count of active sessions
    #[allow(dead_code)]
    pub async fn session_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Check if a session exists
    #[allow(dead_code)]
    pub async fn has_session(&self, session_id: &str) -> bool {
        self.sessions.read().await.contains_key(session_id)
    }

    /// Close all sessions (cleanup on app shutdown)
    #[allow(dead_code)]
    pub async fn close_all_sessions(&self) {
        let mut sessions = self.sessions.write().await;
        
        for (id, mut session) in sessions.drain() {
            info!("Closing session {} on shutdown", id);
            if let Err(e) = session.provider.disconnect().await {
                warn!("Error disconnecting session {}: {}", id, e);
            }
        }
        
        let mut active = self.active_session_id.write().await;
        *active = None;
    }

    // ============ Provider Operations (delegated to session) ============

    /// List files in a session (sync closure version - use list_files_async instead)
    #[allow(dead_code)]
    pub async fn list_files(
        &self,
        session_id: Option<&str>,
        _path: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        let sid = self.resolve_session_id(session_id).await?;

        self.get_session_mut(&sid, |_session| {
            // We need to return a future, but we're in a sync closure
            // This is a limitation - we'll handle it differently
            Err(ProviderError::Other("Use async version".to_string()))
        }).await
    }

    /// Async list files - proper async implementation
    pub async fn list_files_async(
        &self,
        session_id: Option<&str>,
        path: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        let sid = self.resolve_session_id(session_id).await?;
        
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(&sid)
            .ok_or(ProviderError::NotConnected)?;
        
        session.info.last_activity = std::time::Instant::now();
        session.provider.list(path).await
    }

    /// Change directory in a session
    pub async fn change_dir(
        &self,
        session_id: Option<&str>,
        path: &str,
    ) -> Result<String, ProviderError> {
        let sid = self.resolve_session_id(session_id).await?;
        
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(&sid)
            .ok_or(ProviderError::NotConnected)?;
        
        session.info.last_activity = std::time::Instant::now();
        session.provider.cd(path).await?;
        
        let new_path = session.provider.pwd().await?;
        session.info.current_path = new_path.clone();
        
        Ok(new_path)
    }

    /// Get current directory for a session
    pub async fn pwd(&self, session_id: Option<&str>) -> Result<String, ProviderError> {
        let sid = self.resolve_session_id(session_id).await?;
        
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(&sid)
            .ok_or(ProviderError::NotConnected)?;
        
        session.provider.pwd().await
    }

    /// Create directory in a session
    pub async fn mkdir(
        &self,
        session_id: Option<&str>,
        path: &str,
    ) -> Result<(), ProviderError> {
        let sid = self.resolve_session_id(session_id).await?;
        
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(&sid)
            .ok_or(ProviderError::NotConnected)?;
        
        session.info.last_activity = std::time::Instant::now();
        session.provider.mkdir(path).await
    }

    /// Delete file/folder in a session
    pub async fn delete(
        &self,
        session_id: Option<&str>,
        path: &str,
    ) -> Result<(), ProviderError> {
        let sid = self.resolve_session_id(session_id).await?;
        
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(&sid)
            .ok_or(ProviderError::NotConnected)?;
        
        session.info.last_activity = std::time::Instant::now();
        session.provider.delete(path).await
    }

    /// Rename file/folder in a session
    pub async fn rename(
        &self,
        session_id: Option<&str>,
        from: &str,
        to: &str,
    ) -> Result<(), ProviderError> {
        let sid = self.resolve_session_id(session_id).await?;
        
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(&sid)
            .ok_or(ProviderError::NotConnected)?;
        
        session.info.last_activity = std::time::Instant::now();
        session.provider.rename(from, to).await
    }

    /// Download file
    pub async fn download(
        &self,
        session_id: Option<&str>,
        remote_path: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        let sid = self.resolve_session_id(session_id).await?;
        
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(&sid)
            .ok_or(ProviderError::NotConnected)?;
        
        session.info.last_activity = std::time::Instant::now();
        // No progress callback for now - can be added later
        session.provider.download(remote_path, local_path, None).await
    }

    /// Upload file
    pub async fn upload(
        &self,
        session_id: Option<&str>,
        local_path: &str,
        remote_path: &str,
    ) -> Result<(), ProviderError> {
        let sid = self.resolve_session_id(session_id).await?;
        
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(&sid)
            .ok_or(ProviderError::NotConnected)?;
        
        session.info.last_activity = std::time::Instant::now();
        // No progress callback for now - can be added later
        session.provider.upload(local_path, remote_path, None).await
    }

    /// Create share link
    pub async fn create_share_link(
        &self,
        session_id: Option<&str>,
        path: &str,
    ) -> Result<String, ProviderError> {
        let sid = self.resolve_session_id(session_id).await?;
        
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(&sid)
            .ok_or(ProviderError::NotConnected)?;
        
        if !session.provider.supports_share_links() {
            return Err(ProviderError::Other(format!(
                "{} does not support share links",
                session.info.protocol
            )));
        }
        
        session.provider.create_share_link(path, None).await
    }
}

impl Default for MultiProviderState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_lifecycle() {
        let state = MultiProviderState::new();
        
        // Initially no sessions
        assert_eq!(state.session_count().await, 0);
        assert!(state.get_active_session_id().await.is_none());
        
        // After closing all, should be empty
        state.close_all_sessions().await;
        assert_eq!(state.session_count().await, 0);
    }
}
