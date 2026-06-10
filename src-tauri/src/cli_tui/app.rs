use crate::cli_tui::{
    event::{OverlayKey, TuiAction},
    overlay::{ConfirmKind, ConfirmState, PromptKind, PromptState, TuiOverlay},
    panes::{
        browser::BrowserPaneState, profiles::ProfilesPaneState, transfers::TransfersPaneState,
    },
    session::{TuiSessionIdentity, TuiSessionPhase, TuiSessionState},
    worker::{TransferDirection, TuiWorkerOperation, WorkerCommand, WorkerEvent},
};

#[derive(Debug, Clone)]
pub struct TuiContext {
    pub users: Vec<TuiUser>,
    pub initial_user: usize,
    /// Launch CWD captured at TUI entry boundary for absolute download defaults.
    /// Never compute inside pure AppState; threaded from caller.
    pub download_base: String,
}

impl TuiContext {
    pub fn empty() -> Self {
        Self {
            users: Vec::new(),
            initial_user: 0,
            download_base: ".".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TuiUser {
    pub name: String,
    pub is_active: bool,
    pub is_locked: bool,
    pub is_admin: bool,
    pub profile_count: usize,
    pub profiles: Vec<TuiProfile>,
}

#[derive(Debug, Clone, Default)]
pub struct TuiProfile {
    pub selector: String,
    /// Stable saved-profile id (vault `id`), used to persist the favorite flag
    /// via `toggle_favorite_in_vault`. Empty for ad-hoc/unsaved entries.
    pub id: String,
    pub name: String,
    pub protocol: String,
    pub host: String, // server host or subtitle; always rendered in clear (hosts stay clear)
    pub username: String, // raw auth username/account id (email, AKIA*, token, etc); mask by default
    pub initial_path: String,
    /// Default local directory for this profile (if saved in the bookmark as "localPath" or "defaultLocalPath").
    /// If present and non-empty, the Local pane should open here on connect.
    pub default_local_path: String,
    pub favorite: bool,
    /// Cached storage usage from the saved bookmark (the GUI/CLI `lastQuota`),
    /// surfaced read-only in the IntroHub table. Computed in `build_tui_context`
    /// with the same CLI helpers (`profile_effective_used`/`_total`); the TUI
    /// only renders them. `None` when the bookmark has no cached quota.
    pub used: Option<u64>,
    pub total: Option<u64>,
    /// Pre-formatted "last connected" label (the CLI `format_time_ago` output),
    /// or `None` when the profile was never connected.
    pub last_connected_label: Option<String>,
}

#[derive(Debug)]
pub struct AppState {
    pub context: TuiContext,
    pub focus: TuiFocus,
    pub selected_user: usize,
    pub selected_profile: usize,
    pub selected_action: usize,
    pub should_quit: bool,
    pub status: String,
    pub browser: BrowserPaneState, // remote (after connect)
    pub local: BrowserPaneState,   // local filesystem (always available in Phase 3+)
    pub profiles: ProfilesPaneState,
    pub transfers: TransfersPaneState,
    pub overlay: TuiOverlay,
    pub session: TuiSessionState,
    pub worker: WorkerEvent,
    live_worker_enabled: bool,
    intent: Option<TuiIntent>,
    /// Default false (masked). Toggled via 's' for the current TUI session only; never persisted.
    pub show_credentials: bool,
    /// When in Browser focus in dual-pane mode (Phase 3), which side is active for navigation/ops.
    pub active_browser_side: BrowserSide,
}

impl AppState {
    pub fn new(context: TuiContext) -> Self {
        let selected_user = context
            .initial_user
            .min(context.users.len().saturating_sub(1));
        let mut state = Self {
            context,
            focus: TuiFocus::Users,
            selected_user,
            selected_profile: 0,
            selected_action: 0,
            should_quit: false,
            status: "Select a user, then a profile, then an action.".to_string(),
            browser: BrowserPaneState::default(),
            local: BrowserPaneState::default(),
            profiles: ProfilesPaneState::default(),
            transfers: TransfersPaneState::default(),
            overlay: TuiOverlay::None,
            session: TuiSessionState::default(),
            worker: WorkerEvent::Idle,
            live_worker_enabled: false,
            intent: None,
            show_credentials: false,
            active_browser_side: BrowserSide::Remote,
        };
        state.sync_pane_state();
        // Seed a reasonable starting point for the local pane (Phase 3 dual-pane).
        // Real population via worker LocalList will happen on first focus/refresh.
        if let Ok(cwd) = std::env::current_dir() {
            state.local.path = cwd.to_string_lossy().to_string();
        }
        state
    }

    pub fn new_live(context: TuiContext) -> Self {
        let mut state = Self::new(context);
        state.live_worker_enabled = true;
        state
    }

    pub fn selected_user(&self) -> Option<&TuiUser> {
        self.context.users.get(self.selected_user)
    }

    pub fn selected_profile(&self) -> Option<&TuiProfile> {
        self.selected_user()
            .and_then(|user| user.profiles.get(self.selected_profile))
    }

    pub fn selected_action(&self) -> &'static TuiActionItem {
        &TUI_ACTION_ITEMS[self
            .selected_action
            .min(TUI_ACTION_ITEMS.len().saturating_sub(1))]
    }

    pub fn take_intent(&mut self) -> Option<TuiIntent> {
        self.intent.take()
    }

    pub fn apply_action(&mut self, action: TuiAction) -> Vec<WorkerCommand> {
        // Shape B IntroHub: while a live TUI is not yet connected, the screen is
        // the My Servers table with a header user switcher. Navigation is
        // decoupled from the old pane-focus model: Up/Down move the highlighted
        // profile, Left/Right switch user, Enter connects. Other keys (Quit,
        // reveal toggle) fall through to the shared handlers below. The old
        // non-live launcher (dead-code at runtime) keeps its pane-focus flow.
        if self.live_worker_enabled
            && !self.is_live_connected()
            && !matches!(self.focus, TuiFocus::Browser | TuiFocus::Transfers)
        {
            if let Some(commands) = self.introhub_apply(action) {
                self.sync_pane_state();
                return commands;
            }
        }
        let commands = match action {
            TuiAction::Quit => {
                self.finish(TuiIntent::Quit);
                Vec::new()
            }
            TuiAction::MoveDown => self.move_selection(1),
            TuiAction::MoveUp => self.move_selection(-1),
            TuiAction::MoveLeft => self.focus_prev(),
            TuiAction::MoveRight => self.focus_next(),
            TuiAction::Activate => self.activate(),
            TuiAction::Parent => self.navigate_parent(),
            TuiAction::NewDir => self.trigger_new_dir(),
            TuiAction::Delete => self.trigger_delete(),
            TuiAction::Rename => self.trigger_rename(),
            TuiAction::Download => self.trigger_download(),
            TuiAction::Upload => self.trigger_upload(),
            TuiAction::CancelOp => self.cancel_operation(),
            TuiAction::ClearTransfers => self.clear_transfers(),
            TuiAction::ToggleShowCredentials => {
                self.show_credentials = !self.show_credentials;
                self.status = if self.show_credentials {
                    "Credentials shown for this session (not persisted).".to_string()
                } else {
                    "Credentials masked (default). Press 's' to show.".to_string()
                };
                Vec::new()
            }
            TuiAction::SwitchBrowserSide => {
                if self.focus == TuiFocus::Browser && self.live_worker_enabled {
                    self.flip_browser_side();
                    // If switching to local and it has no listing yet, trigger list (local is always available).
                    if self.active_browser_side == BrowserSide::Local
                        && self.local.entries.is_empty()
                        && !self.local.path.is_empty()
                    {
                        return vec![WorkerCommand::LocalList {
                            path: self.local.path.clone(),
                        }];
                    }
                } else {
                    // Fallback: behave like normal right move if not in dual browser
                    let _ = self.focus_next();
                }
                Vec::new()
            }
            // Favorite toggling only applies on the IntroHub (handled in
            // introhub_apply); a no-op in the connected browser.
            TuiAction::ToggleFavorite => Vec::new(),
            TuiAction::Noop => Vec::new(),
        };
        self.sync_pane_state();
        commands
    }

    /// IntroHub (Shape B pre-connection) action routing. Returns `Some(commands)`
    /// when the action is handled by the My Servers screen, or `None` to fall
    /// through to the shared handlers (Quit, reveal toggle, Noop).
    fn introhub_apply(&mut self, action: TuiAction) -> Option<Vec<WorkerCommand>> {
        match action {
            // Up/Down move the highlighted profile within the current user.
            TuiAction::MoveUp => Some(self.introhub_move_profile(-1)),
            TuiAction::MoveDown => Some(self.introhub_move_profile(1)),
            // Left/Right switch the active user (the header switcher).
            TuiAction::MoveLeft => Some(self.introhub_cycle_user(-1)),
            TuiAction::MoveRight => Some(self.introhub_cycle_user(1)),
            // Tab has no Local/Remote side to flip yet, so it also cycles users.
            TuiAction::SwitchBrowserSide => Some(self.introhub_cycle_user(1)),
            // `f` toggles the favorite flag of the highlighted profile.
            TuiAction::ToggleFavorite => Some(self.introhub_toggle_favorite()),
            // Enter connects to the selected profile (or unlocks a locked user).
            TuiAction::Activate => Some(self.introhub_activate()),
            // Backspace/Parent is meaningless on the picker; swallow it.
            TuiAction::Parent => Some(Vec::new()),
            _ => None,
        }
    }

    /// Move the highlighted profile in the IntroHub table, reusing the picker's
    /// Profiles-focus move (which also refreshes the local-pane preview).
    fn introhub_move_profile(&mut self, delta: isize) -> Vec<WorkerCommand> {
        self.focus = TuiFocus::Profiles;
        self.move_selection(delta)
    }

    /// Switch the active user in the header switcher, reset the profile cursor,
    /// and refresh the local-pane preview for the newly selected profile.
    fn introhub_cycle_user(&mut self, delta: isize) -> Vec<WorkerCommand> {
        let len = self.context.users.len();
        if len == 0 {
            return Vec::new();
        }
        let next = (self.selected_user as isize + delta).clamp(0, len as isize - 1) as usize;
        self.selected_user = next;
        self.selected_profile = 0;
        // Reuse the Profiles-focus move (delta 0 keeps the cursor) so the local
        // preview and planned-session refresh for the new user's first profile.
        self.focus = TuiFocus::Profiles;
        let commands = self.move_selection(0);
        self.status = self.contextual_status();
        commands
    }

    /// Enter on the IntroHub: connect to the highlighted profile, or hand a
    /// locked user off to the existing CLI unlock prompt.
    fn introhub_activate(&mut self) -> Vec<WorkerCommand> {
        let Some(user) = self.selected_user().cloned() else {
            self.status = "No users found in the local vault.".to_string();
            return Vec::new();
        };
        if user.is_locked {
            self.finish(TuiIntent::ProfilesInteractive {
                user_name: user.name,
            });
            return Vec::new();
        }
        if let Some(profile) = self.selected_profile().cloned() {
            self.connect_to_profile(&user, &profile)
        } else {
            self.status = format!("User '{}' has no visible profiles.", user.name);
            Vec::new()
        }
    }

    /// Toggle the favorite flag of the highlighted profile. The display state is
    /// flipped optimistically and the persistence (vault `config_favorite_servers`
    /// via `toggle_favorite_in_vault`) is delegated to the worker; the TUI never
    /// writes the vault itself.
    fn introhub_toggle_favorite(&mut self) -> Vec<WorkerCommand> {
        let (user_idx, profile_idx) = (self.selected_user, self.selected_profile);
        let Some(profile) = self
            .context
            .users
            .get_mut(user_idx)
            .and_then(|user| user.profiles.get_mut(profile_idx))
        else {
            return Vec::new();
        };
        if profile.id.is_empty() {
            self.status = "This profile has no saved id; favorite not persisted.".to_string();
            return Vec::new();
        }
        profile.favorite = !profile.favorite;
        let now_favorite = profile.favorite;
        let profile_id = profile.id.clone();
        let name = profile.name.clone();
        self.status = if now_favorite {
            format!("Marked '{}' as favorite.", name)
        } else {
            format!("Removed '{}' from favorites.", name)
        };
        vec![WorkerCommand::ToggleFavorite { profile_id }]
    }

    fn move_selection(&mut self, delta: isize) -> Vec<WorkerCommand> {
        if matches!(self.focus, TuiFocus::Browser) {
            // Move the *active* side (local or remote), not always the remote pane,
            // or the local highlight stays stuck on the first entry while the status
            // line (which reads the active side) appears to move.
            self.active_browser_mut().move_selection(delta);
            self.status = self.contextual_status();
            return Vec::new();
        }
        if matches!(self.focus, TuiFocus::Transfers) {
            self.transfers.move_selection(delta);
            self.status = self.contextual_status();
            return Vec::new();
        }

        let len = match self.focus {
            TuiFocus::Users => self.context.users.len(),
            TuiFocus::Profiles => self
                .selected_user()
                .map(|user| user.profiles.len())
                .unwrap_or_default(),
            TuiFocus::Actions => TUI_ACTION_ITEMS.len(),
            TuiFocus::Browser | TuiFocus::Transfers => {
                unreachable!("list panes are handled above")
            }
        };
        if len == 0 {
            return Vec::new();
        }
        let target = match self.focus {
            TuiFocus::Users => &mut self.selected_user,
            TuiFocus::Profiles => &mut self.selected_profile,
            TuiFocus::Actions => &mut self.selected_action,
            TuiFocus::Browser | TuiFocus::Transfers => {
                unreachable!("list panes are handled above")
            }
        };
        let next = (*target as isize + delta).clamp(0, len.saturating_sub(1) as isize);
        *target = next as usize;
        if matches!(self.focus, TuiFocus::Users) {
            self.selected_profile = 0;
        }
        // Phase 3: when selecting a profile in the picker (Profiles or Actions pane),
        // update the local pane to the profile's saved default local path (if present)
        // and send LocalList so the preview populates with actual files.
        if matches!(self.focus, TuiFocus::Profiles | TuiFocus::Actions) {
            let default_local_path = self
                .selected_profile()
                .map(|p| p.default_local_path.clone())
                .unwrap_or_default();
            if !default_local_path.is_empty() && self.local.path != default_local_path {
                // clear() resets path to "", so clear FIRST then set the path.
                self.local.clear();
                self.local.path = default_local_path;
                if self.live_worker_enabled {
                    self.status = self.contextual_status();
                    return vec![WorkerCommand::LocalList {
                        path: self.local.path.clone(),
                    }];
                }
            }
        }
        self.status = self.contextual_status();
        Vec::new()
    }

    fn focus_next(&mut self) -> Vec<WorkerCommand> {
        // In the connected full-screen view only the Browser and Transfers panes
        // exist; never cycle focus back onto the hidden picker panes.
        if self.is_live_connected() {
            self.focus = match self.focus {
                TuiFocus::Transfers => TuiFocus::Browser,
                _ => TuiFocus::Transfers,
            };
            self.status = self.contextual_status();
            return Vec::new();
        }
        self.focus = match self.focus {
            TuiFocus::Users => TuiFocus::Profiles,
            TuiFocus::Profiles => TuiFocus::Actions,
            TuiFocus::Actions => TuiFocus::Browser,
            TuiFocus::Browser => TuiFocus::Transfers,
            TuiFocus::Transfers => TuiFocus::Users,
        };
        self.status = self.contextual_status();
        Vec::new()
    }

    fn focus_prev(&mut self) -> Vec<WorkerCommand> {
        if matches!(self.focus, TuiFocus::Browser) {
            return self.navigate_parent();
        }

        self.focus = match self.focus {
            TuiFocus::Users => TuiFocus::Transfers,
            TuiFocus::Profiles => TuiFocus::Users,
            TuiFocus::Actions => TuiFocus::Profiles,
            TuiFocus::Transfers => TuiFocus::Browser,
            TuiFocus::Browser => unreachable!("browser left navigation is handled above"),
        };
        self.status = self.contextual_status();
        Vec::new()
    }

    fn activate(&mut self) -> Vec<WorkerCommand> {
        let Some(user) = self.selected_user().cloned() else {
            self.status = "No users found in the local vault.".to_string();
            return Vec::new();
        };

        if user.is_locked {
            self.finish(TuiIntent::ProfilesInteractive {
                user_name: user.name,
            });
            return Vec::new();
        }

        match self.focus {
            TuiFocus::Users => {
                self.focus = TuiFocus::Profiles;
                self.status = self.contextual_status();
                Vec::new()
            }
            TuiFocus::Profiles => {
                if let Some(profile) = self.selected_profile().cloned() {
                    if self.live_worker_enabled {
                        // Direct connect on Enter from Profiles (first-use polish).
                        return self.connect_to_profile(&user, &profile);
                    }
                    self.focus = TuiFocus::Actions;
                    self.status = self.contextual_status();
                } else {
                    self.status = format!("User '{}' has no visible profiles.", user.name);
                }
                Vec::new()
            }
            TuiFocus::Actions => {
                let action = self.selected_action();
                match action.intent {
                    TuiActionIntent::ProfilesInteractive => {
                        self.finish(TuiIntent::ProfilesInteractive {
                            user_name: user.name,
                        });
                        Vec::new()
                    }
                    TuiActionIntent::Profile(action) => {
                        let Some(profile) = self.selected_profile().cloned() else {
                            self.status =
                                "Select a profile before running profile actions.".to_string();
                            return Vec::new();
                        };
                        if self.live_worker_enabled && action == TuiProfileAction::ListRoot {
                            return self.connect_to_profile(&user, &profile);
                        }
                        self.finish(TuiIntent::ProfileAction {
                            user_name: user.name,
                            profile_selector: profile.selector,
                            action,
                        });
                        Vec::new()
                    }
                    TuiActionIntent::Planned => {
                        self.status = format!(
                            "{} - {} ({})",
                            action.title, action.description, action.phase
                        );
                        Vec::new()
                    }
                }
            }
            TuiFocus::Browser => self.open_selected_browser_entry(),
            TuiFocus::Transfers => {
                self.status = self.contextual_status();
                Vec::new()
            }
        }
    }

    /// Shared connect logic for live "List root" (both from Actions "Connect & browse"
    /// and direct Enter on a profile when live_worker_enabled). Refactored out of
    /// activate() per connect-flow polish.
    fn connect_to_profile(&mut self, user: &TuiUser, profile: &TuiProfile) -> Vec<WorkerCommand> {
        let identity = TuiSessionIdentity::from_selection(user, profile);
        let mut session = TuiSessionState::planned_from_selection(user, profile);
        session.begin_connect();
        self.session = session;
        self.browser.clear();
        self.focus = TuiFocus::Browser;

        // Phase 3: when connecting to a profile, initialize BOTH panes with the profile's saved paths if present.
        // Remote: always profile.initial_path (passed to OpenSession)
        // Local: prefer profile.default_local_path (from saved "localPath" or "defaultLocalPath" in the server bookmark),
        //        else fall back to whatever was seeded at launch (CWD / download_base).
        let local_start_path = if !profile.default_local_path.is_empty() {
            profile.default_local_path.clone()
        } else if !self.local.path.is_empty() {
            self.local.path.clone()
        } else {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "/".to_string())
        };

        // clear() resets the pane to default (path becomes ""), so wipe stale
        // state FIRST and set the start path AFTER, or the LocalList below would
        // be dispatched with an empty path ("cannot read local dir ''").
        self.local.clear();
        self.local.path = local_start_path;

        let commands = vec![
            WorkerCommand::OpenSession {
                identity: identity.clone(),
                initial_cwd: profile.initial_path.clone(),
            },
            WorkerCommand::List {
                path: "/".to_string(),
            },
            WorkerCommand::LocalList {
                path: self.local.path.clone(),
            },
        ];

        self.worker = WorkerEvent::Busy {
            operation: TuiWorkerOperation::Connect,
            identity: Some(identity.clone()),
        };
        self.status = format!(
            "Connecting to '{}' for a live read-only listing.",
            profile.name
        );
        commands
    }

    fn open_selected_browser_entry(&mut self) -> Vec<WorkerCommand> {
        // Phase 3: operations on the active side (for now remote uses worker, local will too).
        let active = self.active_browser_mut();
        if let Some(path) = active.selected_directory_path() {
            if self.active_browser_side == BrowserSide::Local {
                // Local list can be handled in worker or directly; for consistency go through worker.
                self.status = format!("Listing local {}.", path);
                return vec![WorkerCommand::LocalList { path }];
            }
            self.worker = WorkerEvent::Busy {
                operation: TuiWorkerOperation::List,
                identity: self.session.identity.clone(),
            };
            self.status = format!("Listing {}.", path);
            return vec![WorkerCommand::List { path }];
        }

        let Some(path) = active.selected_file_path() else {
            self.status = "No browser entry selected.".to_string();
            return Vec::new();
        };

        active.clear_preview();
        if self.active_browser_side == BrowserSide::Local {
            self.status = format!("Loading local metadata for {}.", path);
            return vec![WorkerCommand::LocalStat { path }];
        }
        self.worker = WorkerEvent::Busy {
            operation: TuiWorkerOperation::Stat,
            identity: self.session.identity.clone(),
        };
        self.status = format!("Loading metadata for {}.", path);
        vec![WorkerCommand::Stat { path }]
    }

    fn navigate_parent(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            return self.focus_prev();
        }

        let active = self.active_browser();
        let Some(path) = active.parent_path() else {
            self.status = if active.summary.is_some() {
                format!("Already at {}.", active.path)
            } else {
                "No live listing loaded.".to_string()
            };
            return Vec::new();
        };

        if self.active_browser_side == BrowserSide::Local {
            self.status = format!("Listing local parent {}.", path);
            return vec![WorkerCommand::LocalList { path }];
        }

        self.worker = WorkerEvent::Busy {
            operation: TuiWorkerOperation::List,
            identity: self.session.identity.clone(),
        };
        self.status = format!("Listing parent {}.", path);
        vec![WorkerCommand::List { path }]
    }

    /// Whether a live, connected session exists. Mutating actions and transfers
    /// are gated on this so the TUI never tries to act without a provider.
    pub(crate) fn is_live_connected(&self) -> bool {
        self.live_worker_enabled && matches!(self.session.phase, TuiSessionPhase::Connected)
    }

    /// Phase 3 dual-pane: flip between Local and Remote browser side.
    /// Called from focus navigation when focus==Browser and live.
    fn flip_browser_side(&mut self) {
        self.active_browser_side = match self.active_browser_side {
            BrowserSide::Remote => BrowserSide::Local,
            BrowserSide::Local => BrowserSide::Remote,
        };
        let path = if self.active_browser_side == BrowserSide::Local {
            &self.local.path
        } else {
            &self.browser.path
        };
        self.status = format!("Browser side: {:?} — {}", self.active_browser_side, path);
    }

    pub(crate) fn active_browser(&self) -> &BrowserPaneState {
        match self.active_browser_side {
            BrowserSide::Local => &self.local,
            BrowserSide::Remote => &self.browser,
        }
    }

    pub(crate) fn active_browser_mut(&mut self) -> &mut BrowserPaneState {
        match self.active_browser_side {
            BrowserSide::Local => &mut self.local,
            BrowserSide::Remote => &mut self.browser,
        }
    }

    fn require_live_connection(&mut self) -> bool {
        if self.is_live_connected() {
            return true;
        }
        self.status =
            "Connect first: run 'List root' on a profile to open a live session.".to_string();
        false
    }

    fn trigger_new_dir(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            self.status = "Switch to the Browser pane to create a folder.".to_string();
            return Vec::new();
        }
        if !self.require_live_connection() {
            return Vec::new();
        }
        // Phase 3: mkdir targets the active pane (works for both local and remote).
        let parent = self.active_browser().path.clone();
        self.overlay = TuiOverlay::Prompt(PromptState::new(
            PromptKind::Mkdir {
                parent: parent.clone(),
            },
            format!("New folder in {}", display_dir(&parent)),
            "type a name, Enter to create, Esc to cancel",
            String::new(),
        ));
        self.status = format!("Creating a folder in {:?}.", self.active_browser_side);
        Vec::new()
    }

    fn trigger_delete(&mut self) -> Vec<WorkerCommand> {
        if matches!(self.focus, TuiFocus::Transfers) {
            if let Some(local_path) = self.transfers.remove_selected_if_finished() {
                self.status = "Removed finished transfer from the queue.".to_string();
                // Discarding the row also drops its resumable `.aerotmp` leftover.
                return vec![WorkerCommand::DiscardPartial { local_path }];
            }
            self.status = "Active transfers cannot be removed; cancel them first.".to_string();
            return Vec::new();
        }
        if !matches!(self.focus, TuiFocus::Browser) {
            self.status = "Switch to the Browser pane to delete an entry.".to_string();
            return Vec::new();
        }
        if !self.require_live_connection() {
            return Vec::new();
        }
        // Phase 3: delete on active pane (local or remote selection).
        let active = self.active_browser();
        let Some(entry) = active.selected_entry() else {
            self.status = "No entry selected to delete.".to_string();
            return Vec::new();
        };
        let is_dir = entry.is_dir;
        let Some(path) = active.selected_entry_path() else {
            self.status = "No entry selected to delete.".to_string();
            return Vec::new();
        };
        let message = if is_dir {
            format!("Delete directory '{}' and all its contents?", path)
        } else {
            format!("Delete file '{}'?", path)
        };
        self.overlay = TuiOverlay::Confirm(ConfirmState {
            kind: ConfirmKind::Delete {
                path,
                recursive: is_dir,
            },
            message,
        });
        self.status = format!("Confirm delete (y/n) on {:?}.", self.active_browser_side);
        Vec::new()
    }

    fn trigger_rename(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            self.status = "Switch to the Browser pane to rename an entry.".to_string();
            return Vec::new();
        }
        if !self.require_live_connection() {
            return Vec::new();
        }
        // Phase 3: rename on active pane.
        let active = self.active_browser();
        let Some(entry) = active.selected_entry().cloned() else {
            self.status = "No entry selected to rename.".to_string();
            return Vec::new();
        };
        let Some(from) = active.selected_entry_path() else {
            self.status = "No entry selected to rename.".to_string();
            return Vec::new();
        };
        self.overlay = TuiOverlay::Prompt(PromptState::new(
            PromptKind::Rename { from },
            format!("Rename '{}'", entry.name),
            "edit the name, Enter to apply, Esc to cancel",
            entry.name.clone(),
        ));
        self.status = format!("Renaming an entry on {:?}.", self.active_browser_side);
        Vec::new()
    }

    fn trigger_download(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            self.status = "Switch to the Browser pane to download a file.".to_string();
            return Vec::new();
        }
        if !self.require_live_connection() {
            return Vec::new();
        }
        // Phase 3 cross get/put: 'g' always sources from the remote pane's selection,
        // defaults the destination to the local pane's current directory (smart cross default).
        let remote_state = &self.browser;
        let Some(entry) = remote_state.selected_entry() else {
            self.status = "No (remote) entry selected to download.".to_string();
            return Vec::new();
        };
        if entry.is_dir {
            self.status = "Directory downloads are not supported yet; pick a file.".to_string();
            return Vec::new();
        }
        let name = entry.name.clone();
        let Some(remote) = remote_state.selected_file_path() else {
            self.status = "No file selected to download.".to_string();
            return Vec::new();
        };
        // Cross default: prefer the launch download_base (for test stability and launch CWD),
        // else fall back to the local pane's current path.
        let base = if !self.context.download_base.is_empty() {
            self.context.download_base.clone()
        } else if !self.local.path.is_empty() {
            self.local.path.clone()
        } else {
            ".".to_string()
        };
        let default_local = format!("{}/{}", base.trim_end_matches('/'), name);
        self.overlay = TuiOverlay::Prompt(PromptState::new(
            PromptKind::Download { remote },
            format!("Download '{}'", name),
            "local destination (cross from remote pane), Enter to start, Esc to cancel",
            default_local,
        ));
        self.status = "Downloading a file (cross remote→local).".to_string();
        Vec::new()
    }

    fn trigger_upload(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            self.status = "Switch to the Browser pane to upload into the directory.".to_string();
            return Vec::new();
        }
        if !self.require_live_connection() {
            return Vec::new();
        }
        // Phase 3 cross: 'u' sources from local pane, targets the remote pane's current dir.
        // For simplicity we still prompt for the exact local source (user can have selected in local).
        let remote_dir = self.browser.path.clone();
        // If active is local, we could default the prompt to the active selected local file, but keep prompt for now.
        self.overlay = TuiOverlay::Prompt(PromptState::new(
            PromptKind::Upload {
                remote_dir: remote_dir.clone(),
            },
            format!("Upload into {}", display_dir(&remote_dir)),
            "local source file path (cross to remote pane), Enter to start, Esc to cancel",
            String::new(),
        ));
        self.status = "Uploading a file (cross local→remote).".to_string();
        Vec::new()
    }

    fn cancel_operation(&mut self) -> Vec<WorkerCommand> {
        if self.transfers.has_active() || matches!(self.worker, WorkerEvent::Busy { .. }) {
            self.status = "Cancelling the current operation.".to_string();
            return vec![WorkerCommand::Cancel];
        }
        self.status = "Nothing to cancel.".to_string();
        Vec::new()
    }

    /// Clear every finished transfer from the queue at once, discarding each
    /// `.aerotmp` leftover. In-flight transfers are kept.
    fn clear_transfers(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Transfers) {
            self.status = "Switch to the Transfers pane to clear the queue.".to_string();
            return Vec::new();
        }
        let discarded = self.transfers.clear_finished();
        if discarded.is_empty() {
            self.status = "No finished transfers to clear.".to_string();
            return Vec::new();
        }
        self.status = format!("Cleared {} finished transfer(s).", discarded.len());
        discarded
            .into_iter()
            .map(|local_path| WorkerCommand::DiscardPartial { local_path })
            .collect()
    }

    pub fn overlay_active(&self) -> bool {
        self.overlay.is_active()
    }

    /// Route a key press to the active overlay. The caller (the input loop) only
    /// invokes this while [`Self::overlay_active`] is true.
    pub fn handle_overlay_key(&mut self, key: OverlayKey) -> Vec<WorkerCommand> {
        let commands = match self.overlay {
            TuiOverlay::None => Vec::new(),
            TuiOverlay::Prompt(_) => self.handle_prompt_key(key),
            TuiOverlay::Confirm(_) => self.handle_confirm_key(key),
        };
        self.sync_pane_state();
        commands
    }

    fn handle_prompt_key(&mut self, key: OverlayKey) -> Vec<WorkerCommand> {
        match key {
            OverlayKey::Char(c) => {
                if let TuiOverlay::Prompt(prompt) = &mut self.overlay {
                    prompt.push_char(c);
                }
                Vec::new()
            }
            OverlayKey::Backspace => {
                if let TuiOverlay::Prompt(prompt) = &mut self.overlay {
                    prompt.backspace();
                }
                Vec::new()
            }
            OverlayKey::Submit => self.submit_prompt(),
            OverlayKey::Cancel => self.cancel_overlay(),
            OverlayKey::Noop => Vec::new(),
        }
    }

    fn handle_confirm_key(&mut self, key: OverlayKey) -> Vec<WorkerCommand> {
        match key {
            OverlayKey::Char('y') | OverlayKey::Char('Y') | OverlayKey::Submit => {
                self.confirm_overlay()
            }
            OverlayKey::Char('n') | OverlayKey::Char('N') | OverlayKey::Cancel => {
                self.cancel_overlay()
            }
            _ => Vec::new(),
        }
    }

    fn cancel_overlay(&mut self) -> Vec<WorkerCommand> {
        self.overlay = TuiOverlay::None;
        self.status = "Cancelled.".to_string();
        Vec::new()
    }

    fn submit_prompt(&mut self) -> Vec<WorkerCommand> {
        let TuiOverlay::Prompt(prompt) = &self.overlay else {
            return Vec::new();
        };
        let value = prompt.trimmed().to_string();
        let kind = prompt.kind.clone();

        match kind {
            PromptKind::Mkdir { parent } => {
                if !is_valid_segment(&value) {
                    self.status = "Enter a folder name without '/' or '..'.".to_string();
                    return Vec::new();
                }
                let path = join_remote(&parent, &value);
                self.overlay = TuiOverlay::None;
                self.begin_mutation(TuiWorkerOperation::Mkdir, format!("Creating {}.", path));
                vec![WorkerCommand::Mkdir { path }]
            }
            PromptKind::Rename { from } => {
                if !is_valid_segment(&value) {
                    self.status = "Enter a new name without '/' or '..'.".to_string();
                    return Vec::new();
                }
                let to = join_remote(&parent_remote(&from), &value);
                if to == from {
                    self.overlay = TuiOverlay::None;
                    self.status = "Name unchanged.".to_string();
                    return Vec::new();
                }
                self.overlay = TuiOverlay::None;
                self.begin_mutation(TuiWorkerOperation::Rename, format!("Renaming to {}.", to));
                vec![WorkerCommand::Rename { from, to }]
            }
            PromptKind::Download { remote } => {
                if value.is_empty() {
                    self.status = "Enter a local destination path.".to_string();
                    return Vec::new();
                }
                let name = remote_basename(&remote);
                let id = self.transfers.enqueue(
                    TransferDirection::Download,
                    name,
                    remote.clone(),
                    value.clone(),
                );
                self.overlay = TuiOverlay::None;
                self.focus = TuiFocus::Transfers;
                self.begin_mutation(
                    TuiWorkerOperation::Transfer,
                    format!("Downloading {} -> {}.", remote, value),
                );
                vec![WorkerCommand::Download {
                    id,
                    remote_path: remote,
                    local_path: value,
                }]
            }
            PromptKind::Upload { remote_dir } => {
                if value.is_empty() {
                    self.status = "Enter a local source file path.".to_string();
                    return Vec::new();
                }
                let name = local_basename(&value);
                if name.is_empty() {
                    self.status = "Could not read a file name from that path.".to_string();
                    return Vec::new();
                }
                let remote = join_remote(&remote_dir, &name);
                let id = self.transfers.enqueue(
                    TransferDirection::Upload,
                    name,
                    remote.clone(),
                    value.clone(),
                );
                self.overlay = TuiOverlay::None;
                self.focus = TuiFocus::Transfers;
                self.begin_mutation(
                    TuiWorkerOperation::Transfer,
                    format!("Uploading {} -> {}.", value, remote),
                );
                vec![WorkerCommand::Upload {
                    id,
                    local_path: value,
                    remote_path: remote,
                }]
            }
        }
    }

    fn confirm_overlay(&mut self) -> Vec<WorkerCommand> {
        let TuiOverlay::Confirm(confirm) = &self.overlay else {
            return Vec::new();
        };
        let kind = confirm.kind.clone();
        match kind {
            ConfirmKind::Delete { path, recursive } => {
                self.overlay = TuiOverlay::None;
                self.begin_mutation(TuiWorkerOperation::Remove, format!("Deleting {}.", path));
                vec![WorkerCommand::Remove { path, recursive }]
            }
        }
    }

    fn begin_mutation(&mut self, operation: TuiWorkerOperation, status: String) {
        self.worker = WorkerEvent::Busy {
            operation,
            identity: self.session.identity.clone(),
        };
        self.status = status;
    }

    /// Path of the directory the *active* browser pane (local or remote) currently shows.
    /// Phase 3 dual-pane support.
    fn current_browser_dir(&self) -> String {
        let active = self.active_browser();
        if active.path.is_empty() {
            "/".to_string()
        } else {
            active.path.clone()
        }
    }

    pub fn apply_worker_event(&mut self, event: WorkerEvent) -> Vec<WorkerCommand> {
        if let Some(identity) = event_identity(&event) {
            if self.session.identity.as_ref() != Some(identity) {
                return Vec::new();
            }
        }

        let mut follow_up = Vec::new();
        match &event {
            WorkerEvent::Idle => {
                self.status = "Worker idle.".to_string();
            }
            WorkerEvent::Busy { operation, .. } => {
                if *operation == TuiWorkerOperation::Connect {
                    self.session.begin_connect();
                }
                self.status = format!("{} in progress.", operation.label());
            }
            WorkerEvent::SessionReady { cwd, .. } => {
                self.session.mark_connected(cwd);
                self.status = format!("Session ready at {}.", cwd);
            }
            WorkerEvent::PathReady { operation, path } => {
                self.status = format!("{} ready at {}.", operation.label(), path);
                // A successful mutation invalidates the current listing: refresh
                // the active side (local or remote). Phase 3.
                if matches!(
                    operation,
                    TuiWorkerOperation::Mkdir
                        | TuiWorkerOperation::Remove
                        | TuiWorkerOperation::Rename
                ) {
                    let dir = self.current_browser_dir();
                    if self.active_browser_side == BrowserSide::Local {
                        follow_up.push(WorkerCommand::LocalList { path: dir });
                    } else {
                        follow_up.push(WorkerCommand::List { path: dir });
                    }
                }
            }
            WorkerEvent::ListReady {
                path,
                result,
                identity,
            } => {
                if identity.is_none() {
                    // Local filesystem result (Phase 3 dual-pane)
                    self.local.apply_list_result(path.clone(), result.clone());
                    self.status = format!(
                        "Listed local {}: {} item(s), {} dir(s), {} file(s).",
                        path, result.summary.total, result.summary.dirs, result.summary.files
                    );
                } else {
                    self.session.mark_connected(path);
                    self.browser.apply_list_result(path.clone(), result.clone());
                    self.status = format!(
                        "Listed {}: {} item(s), {} dir(s), {} file(s).",
                        path, result.summary.total, result.summary.dirs, result.summary.files
                    );
                }
            }
            WorkerEvent::StatReady {
                path,
                result,
                identity,
            } => {
                if identity.is_none() {
                    // Local stat
                    if self.local.apply_stat_result(result.clone()) {
                        self.status = stat_status_line(path, self.local.preview.as_ref());
                    } else {
                        self.status = format!("Local metadata ready for {}.", path);
                    }
                } else if self.browser.apply_stat_result(result.clone()) {
                    self.status = stat_status_line(path, self.browser.preview.as_ref());
                } else {
                    self.status = format!("Metadata ready for {}.", path);
                }
            }
            WorkerEvent::TransferProgress {
                id,
                transferred,
                total,
            } => {
                self.transfers.update_progress(*id, *transferred, *total);
                self.status = format!("Transfer #{}: {} / {} bytes.", id, transferred, total);
            }
            WorkerEvent::TransferDone { id, message } => {
                let was_upload = self.transfers.items.iter().any(|item| {
                    item.id == *id && matches!(item.direction, TransferDirection::Upload)
                });
                self.transfers.mark_done(*id);
                self.status = message.clone();
                // An upload adds a file to the current remote directory; refresh
                // so it appears. Downloads never change the remote listing.
                if was_upload {
                    follow_up.push(WorkerCommand::List {
                        path: self.current_browser_dir(),
                    });
                }
            }
            WorkerEvent::TransferFailed { id, message } => {
                self.transfers.mark_failed(*id, message.clone());
                self.status = message.clone();
            }
            WorkerEvent::TransferCancelled { id } => {
                // A cancelled transfer is a per-item terminal state. The live
                // session stays Connected so navigation and further transfers
                // keep working; the provider kept the partial `.aerotmp` for a
                // later resume.
                self.transfers.mark_cancelled(*id);
                self.status = format!("Transfer #{} cancelled.", id);
            }
            WorkerEvent::Failed {
                operation, message, ..
            } => {
                // Only connection-fatal operations tear the session down. A
                // failed stat/mutation/transfer leaves the session usable.
                if matches!(
                    operation,
                    TuiWorkerOperation::Connect | TuiWorkerOperation::List
                ) {
                    self.session.mark_failed(message.clone());
                }
                self.status = format!("{} failed: {}", operation.label(), message);
            }
            WorkerEvent::Cancelled { operation } => {
                // A bare Cancel only reaches here when nothing was in flight (an
                // in-flight transfer is aborted via TransferCancelled instead).
                // It must NOT tear the session down: marking it cancelled would
                // flip the phase off Connected and block every later operation.
                self.status = format!("{} cancelled.", operation.label());
            }
        }
        self.worker = event;
        follow_up
    }

    fn finish(&mut self, intent: TuiIntent) {
        self.intent = Some(intent);
        self.should_quit = true;
    }

    fn contextual_status(&self) -> String {
        match self.focus {
            TuiFocus::Users => self
                .selected_user()
                .map(|user| {
                    if user.is_locked {
                        format!(
                            "User '{}' is locked. Enter leaves the TUI and uses the existing unlock prompt.",
                            user.name
                        )
                    } else {
                        format!(
                            "User '{}' selected. Right/Enter moves to profiles.",
                            user.name
                        )
                    }
                })
                .unwrap_or_else(|| "No users found in the local vault.".to_string()),
            TuiFocus::Profiles => self
                .selected_profile()
                .map(|profile| {
                    // Polish: Enter now connects directly in live mode; Right/Tab for actions.
                    format!(
                        "Profile '{}' selected. Enter connects; Right for more actions.",
                        profile.name
                    )
                })
                .unwrap_or_else(|| "No profile available for this user yet.".to_string()),
            TuiFocus::Actions => {
                let action = self.selected_action();
                format!("Action '{}': {}", action.title, action.description)
            }
            TuiFocus::Browser => self
                .browser
                .selected_entry()
                .map(|entry| {
                    if entry.is_dir {
                        format!("Directory '{}' selected.", entry.name)
                    } else {
                        format!("File '{}' selected.", entry.name)
                    }
                })
                .unwrap_or_else(|| {
                    if self.browser.summary.is_some() {
                        format!("{} is empty.", self.browser.path)
                    } else {
                        "No live listing loaded.".to_string()
                    }
                }),
            TuiFocus::Transfers => self
                .transfers
                .selected_item()
                .map(|item| {
                    format!(
                        "{} {} -> {} ({}).",
                        item.direction.label(),
                        item.remote_path,
                        item.local_path,
                        item.status.label()
                    )
                })
                .unwrap_or_else(|| {
                    "No transfers yet. From Browser: g downloads a file, u uploads into the directory."
                        .to_string()
                }),
        }
    }

    fn sync_pane_state(&mut self) {
        self.profiles.selected = self.selected_profile;
        let planned_session = self
            .selected_user()
            .and_then(|user| {
                if user.is_locked {
                    None
                } else {
                    user.profiles
                        .get(self.selected_profile)
                        .map(|profile| TuiSessionState::planned_from_selection(user, profile))
                }
            })
            .unwrap_or_default();
        if planned_session.identity != self.session.identity {
            self.browser.clear();
        }
        let same_identity =
            planned_session.identity.is_some() && planned_session.identity == self.session.identity;
        if self.live_worker_enabled
            && same_identity
            && !matches!(self.session.phase, TuiSessionPhase::Disconnected)
        {
            return;
        }
        self.session = planned_session;
    }
}

fn event_identity(event: &WorkerEvent) -> Option<&TuiSessionIdentity> {
    match event {
        WorkerEvent::Busy { identity, .. } | WorkerEvent::Failed { identity, .. } => {
            identity.as_ref()
        }
        WorkerEvent::SessionReady { identity, .. }
        | WorkerEvent::ListReady { identity, .. }
        | WorkerEvent::StatReady { identity, .. } => identity.as_ref(),
        WorkerEvent::Idle
        | WorkerEvent::PathReady { .. }
        | WorkerEvent::TransferProgress { .. }
        | WorkerEvent::TransferDone { .. }
        | WorkerEvent::TransferFailed { .. }
        | WorkerEvent::TransferCancelled { .. }
        | WorkerEvent::Cancelled { .. } => None,
    }
}

fn is_valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
}

fn display_dir(path: &str) -> &str {
    if path.is_empty() {
        "/"
    } else {
        path
    }
}

fn join_remote(parent: &str, child: &str) -> String {
    let parent = parent.trim_end_matches('/');
    let child = child.trim_matches('/');
    if parent.is_empty() {
        format!("/{}", child)
    } else {
        format!("{}/{}", parent, child)
    }
}

fn parent_remote(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some(("", _)) => "/".to_string(),
        Some((parent, _)) => parent.to_string(),
        None => "/".to_string(),
    }
}

fn remote_basename(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string()
}

fn local_basename(path: &str) -> String {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_string()
}

/// Compact byte size for the status line (the Shape B fullscreen view has no
/// separate details box, so file metadata is folded into the footer status).
fn format_size_compact(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// One-line file metadata for the status footer after a stat (name, size or
/// "directory", mime, permissions, symlink target, modified time), skipping
/// fields the backend did not provide.
fn stat_status_line(
    path: &str,
    preview: Option<&crate::cli_tui::panes::browser::BrowserPreview>,
) -> String {
    match preview {
        Some(p) => {
            let mut parts = vec![p.name.clone()];
            if p.is_dir {
                parts.push("directory".to_string());
            } else {
                parts.push(format_size_compact(p.size));
            }
            if let Some(mime) = &p.mime_type {
                parts.push(mime.clone());
            }
            if let Some(perm) = &p.permissions {
                parts.push(perm.clone());
            }
            if p.is_symlink {
                match &p.link_target {
                    Some(target) => parts.push(format!("symlink -> {}", target)),
                    None => parts.push("symlink".to_string()),
                }
            }
            if let Some(modified) = &p.modified {
                parts.push(modified.chars().take(16).collect::<String>());
            }
            parts.join("  \u{00b7}  ")
        }
        None => format!("Metadata ready for {}.", path),
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TuiFocus {
    Users,
    Profiles,
    Actions,
    Browser,
    Transfers,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TuiIntent {
    Quit,
    ProfilesInteractive {
        user_name: String,
    },
    ProfileAction {
        user_name: String,
        profile_selector: String,
        action: TuiProfileAction,
    },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TuiProfileAction {
    ListRoot,
    Tree,
    DiskUsage,
    Quota,
}

#[derive(Debug, Clone, Copy)]
pub struct TuiActionItem {
    pub title: &'static str,
    pub description: &'static str,
    pub phase: &'static str,
    pub intent: TuiActionIntent,
}

#[derive(Debug, Clone, Copy)]
pub enum TuiActionIntent {
    ProfilesInteractive,
    Profile(TuiProfileAction),
    Planned,
}

/// Which file browser side is active when focus is on the (dual) browser area.
/// Phase 3: Local pane + Remote pane side-by-side.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub enum BrowserSide {
    #[default]
    Remote,
    Local,
}

pub const TUI_ACTION_ITEMS: &[TuiActionItem] = &[
    TuiActionItem {
        title: "Connect & browse",
        description: "Connect to the profile and open a live browser session (Enter from Profiles does this directly).",
        phase: "P1 ready",
        intent: TuiActionIntent::Profile(TuiProfileAction::ListRoot),
    },
    TuiActionItem {
        title: "Profiles navigator (leaves TUI)",
        description: "Open the existing profile navigator for the selected user (exits TUI).",
        phase: "P1 ready",
        intent: TuiActionIntent::ProfilesInteractive,
    },
    TuiActionItem {
        title: "Tree",
        description: "Show a shallow tree through cmd_tree.",
        phase: "P1 ready",
        intent: TuiActionIntent::Profile(TuiProfileAction::Tree),
    },
    TuiActionItem {
        title: "Quota",
        description: "Read storage quota through cmd_df.",
        phase: "P1 ready",
        intent: TuiActionIntent::Profile(TuiProfileAction::Quota),
    },
    TuiActionItem {
        title: "Disk usage",
        description: "Open the existing ncdu explorer for the selected profile.",
        phase: "P1 ready",
        intent: TuiActionIntent::Profile(TuiProfileAction::DiskUsage),
    },
    TuiActionItem {
        title: "Transfers",
        description:
            "Download (g) and upload (u) from the Browser; progress shows in the Transfers pane.",
        phase: "P2 live",
        intent: TuiActionIntent::Planned,
    },
    TuiActionItem {
        title: "Command palette",
        description: "Parse line-mode commands without re-implementing handlers.",
        phase: "P3",
        intent: TuiActionIntent::Planned,
    },
];

/// Port of the GUI's `src/utils/maskCredential.ts` (exact rules, no i18n).
/// Default use: masked display of profile usernames / auth ids (emails, S3 AKIA keys,
/// tokens) in the Profiles pane and Intent preview. Hosts stay in clear per spec.
/// Toggle 's' reveals raw for current session only (never persisted).
pub fn mask_credential(value: &str) -> String {
    if value.is_empty() {
        return value.to_string();
    }
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return value.to_string();
    }

    // HTTP(S) URL: not a secret; render host + path (e.g. for ImageKit URL-endpoint ids
    // that some providers store in the username field). Matches GUI behavior exactly.
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        // Minimal URL host+path without external crate: strip scheme, take up to first ? or #,
        // then host + trimmed path (no trailing /).
        let without_scheme = if let Some(rest) = trimmed
            .strip_prefix("https://")
            .or_else(|| trimmed.strip_prefix("http://"))
            .or_else(|| trimmed.strip_prefix("HTTPS://"))
            .or_else(|| trimmed.strip_prefix("HTTP://"))
        {
            rest
        } else {
            trimmed
        };
        let without_query = without_scheme
            .split(['?', '#'])
            .next()
            .unwrap_or(without_scheme);
        if let Some(slash) = without_query.find('/') {
            let host = &without_query[..slash];
            let mut path = &without_query[slash..];
            while path.ends_with('/') {
                path = &path[..path.len() - 1];
            }
            return if path.is_empty() {
                host.to_string()
            } else {
                format!("{}{}", host, path)
            };
        } else {
            return without_query.to_string();
        }
    }

    // S3 access key: ^AKIA[A-Z0-9]{16,}$ (case-insensitive per TS /i) -> first5...last4
    if trimmed.len() >= 20 && lower.starts_with("akia") {
        let rest = &trimmed[4..];
        if rest.chars().all(|c| c.is_ascii_alphanumeric()) {
            return format!("{}...{}", &trimmed[..5], &trimmed[trimmed.len() - 4..]);
        }
    }

    // Email: first 3 of local + *** + @domain (or all local if <3)
    if let Some(at_idx) = trimmed.find('@') {
        if at_idx > 0 {
            let local = &trimmed[..at_idx];
            let domain = &trimmed[at_idx..];
            let visible = std::cmp::min(3, local.len());
            return format!("{}***{}", &local[..visible], domain);
        }
    }

    // Short (<=3): fully ***
    if trimmed.len() <= 3 {
        return "***".to_string();
    }

    // Generic: first 3 + ***
    format!("{}***", &trimmed[..3])
}

impl Default for AppState {
    fn default() -> Self {
        Self::new(TuiContext::empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_tui::session::TuiSessionPhase;

    fn sample_context() -> TuiContext {
        TuiContext {
            users: vec![TuiUser {
                name: "default".to_string(),
                is_active: true,
                is_locked: false,
                is_admin: true,
                profile_count: 1,
                profiles: vec![TuiProfile {
                    selector: "1".to_string(),
                    name: "Production".to_string(),
                    protocol: "sftp".to_string(),
                    host: "example.com".to_string(),
                    username: "user".to_string(),
                    initial_path: "/".to_string(),
                    default_local_path: "/tmp".to_string(),
                    favorite: true,
                    ..Default::default()
                }],
            }],
            initial_user: 0,
            download_base: "/tmp/tui_cwd".to_string(),
        }
    }

    fn sample_identity() -> TuiSessionIdentity {
        TuiSessionIdentity {
            user_name: "default".to_string(),
            profile_selector: "1".to_string(),
            profile_name: "Production".to_string(),
            protocol: "sftp".to_string(),
            host: "example.com".to_string(),
        }
    }

    fn list_result(
        entries: Vec<crate::cli_tui::worker::TuiListEntry>,
    ) -> crate::cli_tui::worker::TuiListResult {
        let dirs = entries.iter().filter(|entry| entry.is_dir).count();
        let files = entries.len().saturating_sub(dirs);
        let total_bytes = entries
            .iter()
            .filter(|entry| !entry.is_dir)
            .map(|entry| entry.size)
            .sum();
        crate::cli_tui::worker::TuiListResult {
            summary: crate::cli_tui::worker::TuiListSummary {
                total: entries.len(),
                files,
                dirs,
                total_bytes,
                truncated: false,
                total_before_limit: entries.len(),
            },
            entries,
        }
    }

    fn stat_result(path: &str) -> crate::cli_tui::worker::TuiStatResult {
        crate::cli_tui::worker::TuiStatResult {
            name: path
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(path)
                .to_string(),
            path: path.to_string(),
            is_dir: false,
            size: 42,
            modified: Some("2026-06-09T10:00:00Z".to_string()),
            permissions: Some("-rw-r--r--".to_string()),
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: Some("text/plain".to_string()),
        }
    }

    #[test]
    fn locked_user_activation_routes_to_existing_unlock_flow() {
        let context = TuiContext {
            users: vec![TuiUser {
                name: "locked".to_string(),
                is_active: false,
                is_locked: true,
                is_admin: false,
                profile_count: 2,
                profiles: Vec::new(),
            }],
            initial_user: 0,
            download_base: "/tmp/tui_cwd".to_string(),
        };
        let mut app = AppState::new(context);
        app.apply_action(TuiAction::Activate);

        assert_eq!(
            app.take_intent(),
            Some(TuiIntent::ProfilesInteractive {
                user_name: "locked".to_string()
            })
        );
    }

    #[test]
    fn profile_action_carries_user_and_profile_selector() {
        let mut app = AppState::new(sample_context());
        app.focus = TuiFocus::Actions;
        app.selected_action = 0; // Connect & browse (ListRoot) is now index 0 after reorder
        app.apply_action(TuiAction::Activate);

        assert_eq!(
            app.take_intent(),
            Some(TuiIntent::ProfileAction {
                user_name: "default".to_string(),
                profile_selector: "1".to_string(),
                action: TuiProfileAction::ListRoot,
            })
        );
    }

    #[test]
    fn ready_action_menu_maps_every_command_to_the_expected_intent() {
        // After reorder: 0=Connect&browse (ListRoot), 1=Profiles navigator (None), 2=Tree, ...
        let ready_actions = [
            (0, Some(TuiProfileAction::ListRoot)),
            (1, None),
            (2, Some(TuiProfileAction::Tree)),
            (3, Some(TuiProfileAction::Quota)),
            (4, Some(TuiProfileAction::DiskUsage)),
        ];

        for (selected_action, expected_action) in ready_actions {
            let mut app = AppState::new(sample_context());
            app.focus = TuiFocus::Actions;
            app.selected_action = selected_action;
            app.apply_action(TuiAction::Activate);

            let expected = match expected_action {
                Some(action) => TuiIntent::ProfileAction {
                    user_name: "default".to_string(),
                    profile_selector: "1".to_string(),
                    action,
                },
                None => TuiIntent::ProfilesInteractive {
                    user_name: "default".to_string(),
                },
            };
            assert_eq!(app.take_intent(), Some(expected));
        }
    }

    #[test]
    fn planned_action_menu_items_do_not_exit_the_dashboard() {
        for (selected_action, item) in TUI_ACTION_ITEMS.iter().enumerate() {
            if !matches!(item.intent, TuiActionIntent::Planned) {
                continue;
            }

            let mut app = AppState::new(sample_context());
            app.focus = TuiFocus::Actions;
            app.selected_action = selected_action;
            app.apply_action(TuiAction::Activate);

            assert!(!app.should_quit);
            assert_eq!(app.take_intent(), None);
            assert!(app.status.contains(item.title));
        }
    }

    #[test]
    fn live_list_root_emits_worker_commands_without_exiting() {
        let mut app = AppState::new_live(sample_context());
        app.focus = TuiFocus::Actions;
        app.selected_action = 0; // Connect & browse now at 0

        let commands = app.apply_action(TuiAction::Activate);

        assert!(!app.should_quit);
        assert_eq!(app.take_intent(), None);
        assert_eq!(
            commands,
            vec![
                WorkerCommand::OpenSession {
                    identity: sample_identity(),
                    initial_cwd: "/".to_string(),
                },
                WorkerCommand::List {
                    path: "/".to_string(),
                },
                // Emits LocalList for the profile's default_local_path ("/tmp" in
                // the sample). Regression guard: clear() must not blank this path.
                WorkerCommand::LocalList {
                    path: "/tmp".to_string(),
                },
            ]
        );
        assert_eq!(app.session.phase, TuiSessionPhase::Connecting);
        assert_eq!(app.focus, TuiFocus::Browser);
    }

    #[test]
    fn browser_enter_on_directory_emits_read_only_list_command() {
        let mut app = AppState::new_live(sample_context());
        app.focus = TuiFocus::Actions;
        app.selected_action = 0; // Connect & browse
        app.apply_action(TuiAction::Activate);
        app.apply_worker_event(WorkerEvent::ListReady {
            identity: Some(sample_identity()),
            path: "/srv".to_string(),
            result: list_result(vec![
                crate::cli_tui::worker::TuiListEntry {
                    name: "docs".to_string(),
                    path: "/srv/docs".to_string(),
                    is_dir: true,
                    size: 0,
                    modified: None,
                },
                crate::cli_tui::worker::TuiListEntry {
                    name: "readme.txt".to_string(),
                    path: "/srv/readme.txt".to_string(),
                    is_dir: false,
                    size: 42,
                    modified: None,
                },
            ]),
        });

        let commands = app.apply_action(TuiAction::Activate);

        assert_eq!(
            commands,
            vec![WorkerCommand::List {
                path: "/srv/docs".to_string()
            }]
        );
        assert!(matches!(
            app.worker,
            WorkerEvent::Busy {
                operation: TuiWorkerOperation::List,
                ..
            }
        ));
    }

    #[test]
    fn browser_enter_on_file_emits_read_only_stat_command_and_applies_preview() {
        let mut app = AppState::new_live(sample_context());
        app.focus = TuiFocus::Actions;
        app.selected_action = 0; // Connect & browse
        app.apply_action(TuiAction::Activate);
        app.apply_worker_event(WorkerEvent::ListReady {
            identity: Some(sample_identity()),
            path: "/srv".to_string(),
            result: list_result(vec![
                crate::cli_tui::worker::TuiListEntry {
                    name: "docs".to_string(),
                    path: "/srv/docs".to_string(),
                    is_dir: true,
                    size: 0,
                    modified: None,
                },
                crate::cli_tui::worker::TuiListEntry {
                    name: "readme.txt".to_string(),
                    path: "/srv/readme.txt".to_string(),
                    is_dir: false,
                    size: 42,
                    modified: None,
                },
            ]),
        });
        app.apply_action(TuiAction::MoveDown);

        let commands = app.apply_action(TuiAction::Activate);

        assert_eq!(
            commands,
            vec![WorkerCommand::Stat {
                path: "/srv/readme.txt".to_string()
            }]
        );
        assert!(matches!(
            app.worker,
            WorkerEvent::Busy {
                operation: TuiWorkerOperation::Stat,
                ..
            }
        ));

        app.apply_worker_event(WorkerEvent::StatReady {
            identity: Some(sample_identity()),
            path: "/srv/readme.txt".to_string(),
            result: stat_result("/srv/readme.txt"),
        });

        assert_eq!(
            app.browser.preview.as_ref().map(|preview| (
                preview.path.as_str(),
                preview.size,
                preview.mime_type.as_deref(),
            )),
            Some(("/srv/readme.txt", 42, Some("text/plain")))
        );
    }

    #[test]
    fn stat_failure_keeps_the_live_session_connected() {
        let mut app = AppState::new_live(sample_context());
        app.apply_worker_event(WorkerEvent::ListReady {
            identity: Some(sample_identity()),
            path: "/srv".to_string(),
            result: list_result(Vec::new()),
        });

        app.apply_worker_event(WorkerEvent::Failed {
            operation: TuiWorkerOperation::Stat,
            identity: Some(sample_identity()),
            message: "stat failed: /srv/missing.txt not found".to_string(),
        });

        assert_eq!(app.session.phase, TuiSessionPhase::Connected);
        assert!(app.status.contains("stat failed"));
    }

    #[test]
    fn browser_selection_moves_on_entries_without_touching_profile_selection() {
        let mut app = AppState::new_live(sample_context());
        app.focus = TuiFocus::Browser;
        app.apply_worker_event(WorkerEvent::ListReady {
            identity: Some(sample_identity()),
            path: "/srv".to_string(),
            result: list_result(vec![
                crate::cli_tui::worker::TuiListEntry {
                    name: "docs".to_string(),
                    path: "/srv/docs".to_string(),
                    is_dir: true,
                    size: 0,
                    modified: None,
                },
                crate::cli_tui::worker::TuiListEntry {
                    name: "media".to_string(),
                    path: "/srv/media".to_string(),
                    is_dir: true,
                    size: 0,
                    modified: None,
                },
            ]),
        });

        app.apply_action(TuiAction::MoveDown);

        assert_eq!(app.browser.selected, 1);
        assert_eq!(app.selected_profile, 0);
    }

    #[test]
    fn browser_parent_navigation_lists_parent_and_stops_at_live_root() {
        let mut app = AppState::new_live(sample_context());
        app.focus = TuiFocus::Browser;
        app.apply_worker_event(WorkerEvent::ListReady {
            identity: Some(sample_identity()),
            path: "/srv".to_string(),
            result: list_result(Vec::new()),
        });
        app.apply_worker_event(WorkerEvent::ListReady {
            identity: Some(sample_identity()),
            path: "/srv/docs".to_string(),
            result: list_result(Vec::new()),
        });

        let commands = app.apply_action(TuiAction::MoveLeft);

        assert_eq!(
            commands,
            vec![WorkerCommand::List {
                path: "/srv".to_string()
            }]
        );

        app.apply_worker_event(WorkerEvent::ListReady {
            identity: Some(sample_identity()),
            path: "/srv".to_string(),
            result: list_result(Vec::new()),
        });

        assert!(app.apply_action(TuiAction::Parent).is_empty());
    }

    #[test]
    fn live_worker_ignores_events_for_previous_profile_selection() {
        let mut context = sample_context();
        context.users[0].profiles.push(TuiProfile {
            selector: "2".to_string(),
            name: "Archive".to_string(),
            protocol: "s3".to_string(),
            host: "s3.example.com".to_string(),
            username: "AKIAEXAMPLEKEY".to_string(),
            initial_path: "/archive".to_string(),
            default_local_path: "".to_string(),
            favorite: false,
            ..Default::default()
        });
        let mut app = AppState::new_live(context);
        let stale_identity = app.session.identity.clone().unwrap();

        app.focus = TuiFocus::Profiles;
        app.apply_action(TuiAction::MoveDown);
        assert_eq!(
            app.session
                .identity
                .as_ref()
                .map(|identity| identity.profile_selector.as_str()),
            Some("2")
        );

        app.apply_worker_event(WorkerEvent::SessionReady {
            identity: Some(stale_identity),
            cwd: "/".to_string(),
        });

        assert_eq!(app.session.phase, TuiSessionPhase::Disconnected);
        assert_eq!(
            app.session
                .identity
                .as_ref()
                .map(|identity| identity.profile_selector.as_str()),
            Some("2")
        );
    }

    #[test]
    fn session_preview_tracks_selected_profile_without_connecting() {
        let mut context = sample_context();
        context.users[0].profiles.push(TuiProfile {
            selector: "2".to_string(),
            name: "Archive".to_string(),
            protocol: "s3".to_string(),
            host: "s3.example.com".to_string(),
            username: "AKIAEXAMPLEKEY".to_string(),
            initial_path: "bucket/backups/".to_string(),
            default_local_path: "".to_string(),
            favorite: false,
            ..Default::default()
        });
        let mut app = AppState::new(context);

        app.focus = TuiFocus::Profiles;
        app.apply_action(TuiAction::MoveDown);

        assert_eq!(app.session.cwd, "/bucket/backups");
        assert_eq!(
            app.session.identity.as_ref().map(|identity| (
                identity.user_name.as_str(),
                identity.profile_selector.as_str(),
                identity.profile_name.as_str(),
                identity.protocol.as_str(),
            )),
            Some(("default", "2", "Archive", "s3"))
        );
        assert_eq!(app.session.phase, TuiSessionPhase::Disconnected);
    }

    #[test]
    fn locked_user_has_no_planned_session() {
        let context = TuiContext {
            users: vec![TuiUser {
                name: "locked".to_string(),
                is_active: false,
                is_locked: true,
                is_admin: false,
                profile_count: 1,
                profiles: vec![TuiProfile {
                    selector: "1".to_string(),
                    name: "Hidden".to_string(),
                    protocol: "sftp".to_string(),
                    host: "example.com".to_string(),
                    username: "user".to_string(),
                    initial_path: "/".to_string(),
                    default_local_path: "".to_string(),
                    favorite: false,
                    ..Default::default()
                }],
            }],
            initial_user: 0,
            download_base: "/tmp/tui_cwd".to_string(),
        };
        let app = AppState::new(context);

        assert_eq!(app.session, TuiSessionState::default());
    }

    /// Two users, the first with two profiles, for IntroHub navigation tests.
    fn introhub_context() -> TuiContext {
        let profile = |sel: &str, name: &str| TuiProfile {
            selector: sel.to_string(),
            name: name.to_string(),
            protocol: "sftp".to_string(),
            host: "example.com".to_string(),
            username: "user".to_string(),
            initial_path: "/".to_string(),
            default_local_path: String::new(),
            favorite: false,
            ..Default::default()
        };
        TuiContext {
            users: vec![
                TuiUser {
                    name: "ale".to_string(),
                    is_active: true,
                    is_locked: false,
                    is_admin: true,
                    profile_count: 2,
                    profiles: vec![profile("1", "Production"), profile("2", "Archive")],
                },
                TuiUser {
                    name: "root".to_string(),
                    is_active: false,
                    is_locked: false,
                    is_admin: false,
                    profile_count: 1,
                    profiles: vec![profile("1", "Backup")],
                },
            ],
            initial_user: 0,
            download_base: "/tmp/tui_cwd".to_string(),
        }
    }

    #[test]
    fn introhub_up_down_move_the_highlighted_profile() {
        let mut app = AppState::new_live(introhub_context());
        // Pre-connect IntroHub: Down highlights the next profile of the user.
        app.apply_action(TuiAction::MoveDown);
        assert_eq!(app.selected_profile, 1);
        assert_eq!(app.focus, TuiFocus::Profiles);
        app.apply_action(TuiAction::MoveUp);
        assert_eq!(app.selected_profile, 0);
    }

    #[test]
    fn introhub_left_right_switch_the_active_user() {
        let mut app = AppState::new_live(introhub_context());
        app.selected_profile = 1;
        // Right advances to the next user and resets the profile cursor.
        app.apply_action(TuiAction::MoveRight);
        assert_eq!(app.selected_user, 1);
        assert_eq!(app.selected_profile, 0);
        // Left at the first user is clamped (no wrap).
        app.apply_action(TuiAction::MoveLeft);
        assert_eq!(app.selected_user, 0);
        app.apply_action(TuiAction::MoveLeft);
        assert_eq!(app.selected_user, 0);
    }

    #[test]
    fn introhub_f_toggles_favorite_optimistically_and_asks_the_worker_to_persist() {
        let mut context = introhub_context();
        context.users[0].profiles[0].id = "srv-1".to_string();
        context.users[0].profiles[0].favorite = false;
        let mut app = AppState::new_live(context);

        let commands = app.apply_action(TuiAction::ToggleFavorite);

        // Display state flips immediately.
        assert!(app.context.users[0].profiles[0].favorite);
        // And the worker is asked to persist the change for that profile id.
        assert_eq!(
            commands,
            vec![WorkerCommand::ToggleFavorite {
                profile_id: "srv-1".to_string()
            }]
        );

        // Toggling again clears it.
        app.apply_action(TuiAction::ToggleFavorite);
        assert!(!app.context.users[0].profiles[0].favorite);
    }

    #[test]
    fn introhub_favorite_toggle_is_a_noop_without_a_saved_id() {
        let mut app = AppState::new_live(introhub_context()); // ids are empty
        let commands = app.apply_action(TuiAction::ToggleFavorite);
        assert!(commands.is_empty());
        assert!(!app.context.users[0].profiles[0].favorite);
    }

    #[test]
    fn introhub_enter_connects_the_selected_profile() {
        let mut app = AppState::new_live(introhub_context());
        app.apply_action(TuiAction::MoveRight); // user "root"
        let commands = app.apply_action(TuiAction::Activate);

        assert_eq!(app.focus, TuiFocus::Browser);
        assert!(matches!(
            app.session.phase,
            TuiSessionPhase::Connecting | TuiSessionPhase::Connected
        ));
        assert!(
            commands
                .iter()
                .any(|c| matches!(c, WorkerCommand::OpenSession { .. })),
            "Enter must open a session for the selected profile"
        );
        assert_eq!(
            app.session
                .identity
                .as_ref()
                .map(|identity| identity.user_name.as_str()),
            Some("root")
        );
    }

    /// A live app connected at `/srv` listing a directory (`docs/`) and a file
    /// (`readme.txt`), focused on the Browser with `docs` selected.
    fn connected_app_with_listing() -> AppState {
        let mut app = AppState::new_live(sample_context());
        app.focus = TuiFocus::Actions;
        app.selected_action = 0; // Connect & browse (was ListRoot at 1) now at 0 after polish reorder
        app.apply_action(TuiAction::Activate);
        app.apply_worker_event(WorkerEvent::ListReady {
            identity: Some(sample_identity()),
            path: "/srv".to_string(),
            result: list_result(vec![
                crate::cli_tui::worker::TuiListEntry {
                    name: "docs".to_string(),
                    path: "/srv/docs".to_string(),
                    is_dir: true,
                    size: 0,
                    modified: None,
                },
                crate::cli_tui::worker::TuiListEntry {
                    name: "readme.txt".to_string(),
                    path: "/srv/readme.txt".to_string(),
                    is_dir: false,
                    size: 42,
                    modified: None,
                },
            ]),
        });
        assert_eq!(app.session.phase, TuiSessionPhase::Connected);
        assert_eq!(app.focus, TuiFocus::Browser);
        app
    }

    #[test]
    fn mkdir_prompt_submit_emits_mkdir_under_the_current_directory() {
        let mut app = connected_app_with_listing();

        assert!(app.apply_action(TuiAction::NewDir).is_empty());
        assert!(app.overlay_active());

        for c in "newdir".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);

        assert_eq!(
            commands,
            vec![WorkerCommand::Mkdir {
                path: "/srv/newdir".to_string()
            }]
        );
        assert!(!app.overlay_active());
    }

    #[test]
    fn mkdir_rejects_empty_or_separator_names_without_closing_the_prompt() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::NewDir);

        let commands = app.handle_overlay_key(OverlayKey::Submit);
        assert!(commands.is_empty());
        assert!(app.overlay_active());
        assert!(app.status.contains("folder name"));
    }

    #[test]
    fn delete_confirmation_emits_recursive_remove_for_a_directory() {
        let mut app = connected_app_with_listing();

        assert!(app.apply_action(TuiAction::Delete).is_empty());
        assert!(app.overlay_active());

        let commands = app.handle_overlay_key(OverlayKey::Char('y'));
        assert_eq!(
            commands,
            vec![WorkerCommand::Remove {
                path: "/srv/docs".to_string(),
                recursive: true,
            }]
        );
        assert!(!app.overlay_active());
    }

    #[test]
    fn delete_cancel_leaves_the_remote_untouched() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::Delete);

        let commands = app.handle_overlay_key(OverlayKey::Char('n'));
        assert!(commands.is_empty());
        assert!(!app.overlay_active());
    }

    #[test]
    fn rename_submit_targets_a_sibling_path() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::MoveDown); // select readme.txt

        app.apply_action(TuiAction::Rename);
        app.handle_overlay_key(OverlayKey::Char('x'));
        let commands = app.handle_overlay_key(OverlayKey::Submit);

        assert_eq!(
            commands,
            vec![WorkerCommand::Rename {
                from: "/srv/readme.txt".to_string(),
                to: "/srv/readme.txtx".to_string(),
            }]
        );
    }

    #[test]
    fn download_submit_enqueues_a_transfer_and_focuses_the_pane() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::MoveDown); // select readme.txt

        app.apply_action(TuiAction::Download);
        let commands = app.handle_overlay_key(OverlayKey::Submit);

        assert_eq!(
            commands,
            vec![WorkerCommand::Download {
                id: 1,
                remote_path: "/srv/readme.txt".to_string(),
                local_path: "/tmp/tui_cwd/readme.txt".to_string(),
            }]
        );
        assert_eq!(app.focus, TuiFocus::Transfers);
        assert_eq!(app.transfers.items.len(), 1);
    }

    #[test]
    fn upload_submit_derives_the_remote_path_from_the_local_basename() {
        let mut app = connected_app_with_listing();

        app.apply_action(TuiAction::Upload);
        for c in "/tmp/data.bin".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);

        assert_eq!(
            commands,
            vec![WorkerCommand::Upload {
                id: 1,
                local_path: "/tmp/data.bin".to_string(),
                remote_path: "/srv/data.bin".to_string(),
            }]
        );
    }

    #[test]
    fn mutations_are_blocked_until_a_live_session_is_connected() {
        let mut app = AppState::new_live(sample_context());
        app.focus = TuiFocus::Browser;

        let commands = app.apply_action(TuiAction::NewDir);
        assert!(commands.is_empty());
        assert!(!app.overlay_active());
        assert!(app.status.contains("Connect first"));
    }

    #[test]
    fn successful_mutation_refreshes_the_current_listing() {
        let mut app = connected_app_with_listing();

        let commands = app.apply_worker_event(WorkerEvent::PathReady {
            operation: TuiWorkerOperation::Mkdir,
            path: "/srv/newdir".to_string(),
        });

        assert_eq!(
            commands,
            vec![WorkerCommand::List {
                path: "/srv".to_string()
            }]
        );
    }

    #[test]
    fn transfer_progress_and_completion_update_the_queue() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::MoveDown);
        app.apply_action(TuiAction::Download);
        app.handle_overlay_key(OverlayKey::Submit);

        app.apply_worker_event(WorkerEvent::TransferProgress {
            id: 1,
            transferred: 21,
            total: 42,
        });
        assert_eq!(app.transfers.items[0].transferred, 21);
        assert_eq!(app.transfers.items[0].total, 42);

        let commands = app.apply_worker_event(WorkerEvent::TransferDone {
            id: 1,
            message: "Downloaded /srv/readme.txt -> /tmp/tui_cwd/readme.txt".to_string(),
        });
        // A download never changes the remote listing, so no refresh is queued.
        assert!(commands.is_empty());
        assert_eq!(
            app.transfers.items[0].status,
            crate::cli_tui::panes::transfers::TransferStatus::Done
        );
    }

    #[test]
    fn upload_completion_refreshes_the_listing() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::Upload);
        for c in "/tmp/data.bin".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        app.handle_overlay_key(OverlayKey::Submit);

        let commands = app.apply_worker_event(WorkerEvent::TransferDone {
            id: 1,
            message: "Uploaded /tmp/data.bin -> /srv/data.bin".to_string(),
        });

        assert_eq!(
            commands,
            vec![WorkerCommand::List {
                path: "/srv".to_string()
            }]
        );
    }

    #[test]
    fn cancel_requests_a_worker_cancel_while_a_transfer_is_active() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::MoveDown);
        app.apply_action(TuiAction::Download);
        app.handle_overlay_key(OverlayKey::Submit);

        let commands = app.apply_action(TuiAction::CancelOp);
        assert_eq!(commands, vec![WorkerCommand::Cancel]);
    }

    #[test]
    fn delete_in_the_transfers_pane_drops_only_finished_entries() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::MoveDown);
        app.apply_action(TuiAction::Download);
        app.handle_overlay_key(OverlayKey::Submit);
        assert_eq!(app.focus, TuiFocus::Transfers);

        // Active transfer is protected from removal.
        app.apply_action(TuiAction::Delete);
        assert_eq!(app.transfers.items.len(), 1);

        app.apply_worker_event(WorkerEvent::TransferDone {
            id: 1,
            message: "done".to_string(),
        });
        app.apply_action(TuiAction::Delete);
        assert!(app.transfers.items.is_empty());
    }

    #[test]
    fn cancelling_a_transfer_keeps_the_session_connected() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::MoveDown);
        app.apply_action(TuiAction::Download);
        app.handle_overlay_key(OverlayKey::Submit);

        app.apply_worker_event(WorkerEvent::TransferProgress {
            id: 1,
            transferred: 25,
            total: 100,
        });
        let follow_up = app.apply_worker_event(WorkerEvent::TransferCancelled { id: 1 });

        // The item lands in the Cancelled terminal state with its partial ratio
        // preserved, and crucially the live session stays Connected so the next
        // navigation or transfer still works (regression: a transfer cancel used
        // to flip the whole session to Cancelled and brick every later action).
        assert!(follow_up.is_empty());
        assert_eq!(
            app.transfers.items[0].status,
            crate::cli_tui::panes::transfers::TransferStatus::Cancelled
        );
        assert_eq!(app.transfers.items[0].ratio(), 0.25);
        assert_eq!(app.session.phase, TuiSessionPhase::Connected);
        assert!(app.is_live_connected());
    }

    #[test]
    fn bare_cancel_event_keeps_the_session_connected() {
        let mut app = connected_app_with_listing();

        // A Cancel that reaches the worker with nothing in flight comes back as a
        // bare Cancelled event; it must not tear the session down.
        app.apply_worker_event(WorkerEvent::Cancelled {
            operation: TuiWorkerOperation::Cancel,
        });

        assert_eq!(app.session.phase, TuiSessionPhase::Connected);
        assert!(app.is_live_connected());
    }

    #[test]
    fn removing_a_finished_transfer_discards_its_partial() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::MoveDown); // select readme.txt
        app.apply_action(TuiAction::Download);
        app.handle_overlay_key(OverlayKey::Submit); // id 1, default uses sample download_base
        app.apply_worker_event(WorkerEvent::TransferDone {
            id: 1,
            message: "done".to_string(),
        });

        // `d` on a finished transfer removes the row AND asks the worker to drop
        // its `.aerotmp` leftover.
        let commands = app.apply_action(TuiAction::Delete);
        assert_eq!(
            commands,
            vec![WorkerCommand::DiscardPartial {
                local_path: "/tmp/tui_cwd/readme.txt".to_string()
            }]
        );
        assert!(app.transfers.items.is_empty());
    }

    #[test]
    fn clear_transfers_drops_all_finished_and_discards_each_partial() {
        let mut app = connected_app_with_listing();
        app.focus = TuiFocus::Transfers;
        let done = app.transfers.enqueue(
            TransferDirection::Download,
            "a.txt".to_string(),
            "/srv/a.txt".to_string(),
            "/tmp/a.txt".to_string(),
        );
        let cancelled = app.transfers.enqueue(
            TransferDirection::Download,
            "b.bin".to_string(),
            "/srv/b.bin".to_string(),
            "/tmp/b.bin".to_string(),
        );
        let active = app.transfers.enqueue(
            TransferDirection::Upload,
            "c.bin".to_string(),
            "/srv/c.bin".to_string(),
            "/tmp/c.bin".to_string(),
        );
        app.transfers.mark_done(done);
        app.transfers.mark_cancelled(cancelled);

        // `D` clears every finished row at once, discarding each partial; the
        // in-flight transfer is kept.
        let commands = app.apply_action(TuiAction::ClearTransfers);
        assert_eq!(
            commands,
            vec![
                WorkerCommand::DiscardPartial {
                    local_path: "/tmp/a.txt".to_string()
                },
                WorkerCommand::DiscardPartial {
                    local_path: "/tmp/b.bin".to_string()
                },
            ]
        );
        assert_eq!(app.transfers.items.len(), 1);
        assert_eq!(app.transfers.items[0].id, active);
    }

    // Regression (P3 dual-pane): a remote ListReady must populate `browser`
    // (not `local`) after connecting via the Profiles-Enter flow, and the
    // identity guard must accept it.
    #[test]
    fn remote_list_ready_populates_the_remote_pane_after_connect() {
        let mut app = AppState::new_live(sample_context());
        app.focus = TuiFocus::Profiles;
        app.selected_profile = 0;
        let cmds = app.apply_action(TuiAction::Activate);
        assert!(
            cmds.iter()
                .any(|c| matches!(c, WorkerCommand::OpenSession { .. })),
            "Enter on a profile must connect (OpenSession); got {:?}",
            cmds
        );
        assert!(
            cmds.iter().any(|c| matches!(c, WorkerCommand::List { .. })),
            "connect must request a remote List; got {:?}",
            cmds
        );

        app.apply_worker_event(WorkerEvent::SessionReady {
            identity: Some(sample_identity()),
            cwd: "/srv".to_string(),
        });
        app.apply_worker_event(WorkerEvent::ListReady {
            identity: Some(sample_identity()),
            path: "/srv".to_string(),
            result: list_result(vec![crate::cli_tui::worker::TuiListEntry {
                name: "readme.txt".to_string(),
                path: "/srv/readme.txt".to_string(),
                is_dir: false,
                size: 10,
                modified: None,
            }]),
        });

        assert!(app.is_live_connected(), "session should be Connected");
        assert_eq!(
            app.browser.entries.len(),
            1,
            "remote pane must show the listed file"
        );
        assert!(
            app.local.entries.is_empty(),
            "local pane must NOT receive the remote listing"
        );
    }

    // Regression: arrow movement in the Browser must move the *active* side.
    // Previously it always moved the remote pane, so the local highlight stayed
    // stuck on the first entry while the status line appeared to move.
    #[test]
    fn moving_in_browser_moves_only_the_active_side() {
        fn dir(name: &str) -> crate::cli_tui::worker::TuiListEntry {
            crate::cli_tui::worker::TuiListEntry {
                name: name.to_string(),
                path: format!("/{name}"),
                is_dir: true,
                size: 0,
                modified: None,
            }
        }
        let mut app = AppState::new_live(sample_context());
        app.browser
            .apply_list_result("/r".to_string(), list_result(vec![dir("a"), dir("b")]));
        app.local
            .apply_list_result("/l".to_string(), list_result(vec![dir("x"), dir("y")]));
        app.focus = TuiFocus::Browser;

        app.active_browser_side = BrowserSide::Local;
        app.apply_action(TuiAction::MoveDown);
        assert_eq!(app.local.selected, 1, "active local side must advance");
        assert_eq!(
            app.browser.selected, 0,
            "remote must not move when local is active"
        );

        app.active_browser_side = BrowserSide::Remote;
        app.apply_action(TuiAction::MoveDown);
        assert_eq!(app.browser.selected, 1, "active remote side must advance");
        assert_eq!(
            app.local.selected, 1,
            "local must not move when remote is active"
        );
    }
}
