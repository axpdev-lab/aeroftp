use std::fmt;

use ftp_client_gui_lib::providers::StorageProvider;

use crate::cli_tui::app::{TuiProfile, TuiUser};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TuiSessionIdentity {
    pub user_name: String,
    pub profile_selector: String,
    pub profile_name: String,
    pub protocol: String,
    pub host: String,
}

impl TuiSessionIdentity {
    pub fn from_selection(user: &TuiUser, profile: &TuiProfile) -> Self {
        Self {
            user_name: user.name.clone(),
            profile_selector: profile.selector.clone(),
            profile_name: profile.name.clone(),
            protocol: profile.protocol.clone(),
            host: profile.host.clone(),
        }
    }

    pub fn label(&self) -> String {
        format!(
            "{}:{} ({})",
            self.user_name, self.profile_name, self.protocol
        )
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TuiSessionPhase {
    Disconnected,
    Connecting,
    Connected,
    Failed(String),
    Cancelled,
}

impl TuiSessionPhase {
    pub fn label(&self) -> &str {
        match self {
            TuiSessionPhase::Disconnected => "disconnected",
            TuiSessionPhase::Connecting => "connecting",
            TuiSessionPhase::Connected => "connected",
            TuiSessionPhase::Failed(_) => "failed",
            TuiSessionPhase::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TuiSessionState {
    pub identity: Option<TuiSessionIdentity>,
    pub cwd: String,
    pub phase: TuiSessionPhase,
}

#[allow(dead_code)]
impl TuiSessionState {
    pub fn planned_from_selection(user: &TuiUser, profile: &TuiProfile) -> Self {
        Self {
            identity: Some(TuiSessionIdentity::from_selection(user, profile)),
            cwd: normalize_remote_cwd(&profile.initial_path),
            phase: TuiSessionPhase::Disconnected,
        }
    }

    pub fn begin_connect(&mut self) {
        self.phase = TuiSessionPhase::Connecting;
    }

    pub fn mark_connected(&mut self, cwd: impl Into<String>) {
        self.cwd = normalize_remote_cwd(&cwd.into());
        self.phase = TuiSessionPhase::Connected;
    }

    pub fn mark_failed(&mut self, message: impl Into<String>) {
        self.phase = TuiSessionPhase::Failed(message.into());
    }

    pub fn mark_cancelled(&mut self) {
        self.phase = TuiSessionPhase::Cancelled;
    }

    pub fn label(&self) -> String {
        match &self.identity {
            Some(identity) => format!(
                "{} {} cwd:{}",
                identity.label(),
                self.phase.label(),
                self.cwd
            ),
            None => "no session".to_string(),
        }
    }
}

impl Default for TuiSessionState {
    fn default() -> Self {
        Self {
            identity: None,
            cwd: "/".to_string(),
            phase: TuiSessionPhase::Disconnected,
        }
    }
}

/// Phase-2 live session carrier.
///
/// Construction receives an already-created provider so the CLI remains the
/// single source of truth for vault/profile resolution through `create_and_connect`.
#[allow(dead_code)]
pub struct TuiSession {
    provider: Box<dyn StorageProvider>,
    state: TuiSessionState,
    initial_path: String,
}

#[allow(dead_code)]
impl TuiSession {
    pub fn new(
        provider: Box<dyn StorageProvider>,
        identity: TuiSessionIdentity,
        initial_cwd: impl Into<String>,
    ) -> Self {
        let initial_cwd = normalize_remote_cwd(&initial_cwd.into());
        let mut state = TuiSessionState {
            identity: Some(identity),
            cwd: "/".to_string(),
            phase: TuiSessionPhase::Disconnected,
        };
        state.mark_connected(&initial_cwd);
        Self {
            provider,
            state,
            initial_path: initial_cwd,
        }
    }

    pub fn state(&self) -> &TuiSessionState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut TuiSessionState {
        &mut self.state
    }

    pub fn provider_mut(&mut self) -> &mut dyn StorageProvider {
        self.provider.as_mut()
    }

    pub fn initial_path(&self) -> &str {
        &self.initial_path
    }
}

impl fmt::Debug for TuiSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TuiSession")
            .field("state", &self.state)
            .field("initial_path", &self.initial_path)
            .finish_non_exhaustive()
    }
}

fn normalize_remote_cwd(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "." {
        return "/".to_string();
    }

    let with_root = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{}", trimmed)
    };

    let normalized = with_root.trim_end_matches('/');
    if normalized.is_empty() {
        "/".to_string()
    } else {
        normalized.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_user() -> TuiUser {
        TuiUser {
            id: 1,
            name: "default".to_string(),
            is_active: true,
            is_locked: false,
            is_admin: true,
            profile_count: 1,
            profiles: Vec::new(),
        }
    }

    fn sample_profile(initial_path: &str) -> TuiProfile {
        TuiProfile {
            selector: "1".to_string(),
            name: "Production".to_string(),
            protocol: "sftp".to_string(),
            host: "example.com".to_string(),
            username: "user".to_string(),
            initial_path: initial_path.to_string(),
            default_local_path: "".to_string(),
            favorite: true,
        }
    }

    #[test]
    fn planned_session_identity_comes_from_selected_user_and_profile() {
        let state = TuiSessionState::planned_from_selection(&sample_user(), &sample_profile("www"));

        assert_eq!(
            state.identity,
            Some(TuiSessionIdentity {
                user_name: "default".to_string(),
                profile_selector: "1".to_string(),
                profile_name: "Production".to_string(),
                protocol: "sftp".to_string(),
                host: "example.com".to_string(),
            })
        );
        assert_eq!(state.cwd, "/www");
        assert_eq!(state.phase, TuiSessionPhase::Disconnected);
    }

    #[test]
    fn session_state_transitions_are_render_friendly_labels() {
        let mut state =
            TuiSessionState::planned_from_selection(&sample_user(), &sample_profile("/srv/"));

        state.begin_connect();
        assert_eq!(state.phase.label(), "connecting");

        state.mark_connected("/srv/app/");
        assert_eq!(state.cwd, "/srv/app");
        assert!(state.label().contains("connected"));

        state.mark_failed("network closed");
        assert_eq!(state.phase.label(), "failed");

        state.mark_cancelled();
        assert_eq!(state.phase, TuiSessionPhase::Cancelled);
    }
}
