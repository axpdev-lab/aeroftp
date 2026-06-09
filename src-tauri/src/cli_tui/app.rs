use crate::cli_tui::{
    event::TuiAction,
    panes::{
        browser::BrowserPaneState, profiles::ProfilesPaneState, transfers::TransfersPaneState,
    },
    session::{TuiSessionIdentity, TuiSessionPhase, TuiSessionState},
    worker::{TuiWorkerOperation, WorkerCommand, WorkerEvent},
};

#[derive(Debug, Clone)]
pub struct TuiContext {
    pub users: Vec<TuiUser>,
    pub initial_user: usize,
}

impl TuiContext {
    pub fn empty() -> Self {
        Self {
            users: Vec::new(),
            initial_user: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TuiUser {
    pub id: i64,
    pub name: String,
    pub is_active: bool,
    pub is_locked: bool,
    pub is_admin: bool,
    pub profile_count: usize,
    pub profiles: Vec<TuiProfile>,
}

#[derive(Debug, Clone)]
pub struct TuiProfile {
    pub selector: String,
    pub name: String,
    pub protocol: String,
    pub host: String,
    pub initial_path: String,
    pub favorite: bool,
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
    pub browser: BrowserPaneState,
    pub profiles: ProfilesPaneState,
    pub transfers: TransfersPaneState,
    pub session: TuiSessionState,
    pub worker: WorkerEvent,
    live_worker_enabled: bool,
    intent: Option<TuiIntent>,
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
            profiles: ProfilesPaneState::default(),
            transfers: TransfersPaneState::default(),
            session: TuiSessionState::default(),
            worker: WorkerEvent::Idle,
            live_worker_enabled: false,
            intent: None,
        };
        state.sync_pane_state();
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

    pub fn phase_label(&self) -> &'static str {
        if self.live_worker_enabled {
            "Phase 2"
        } else {
            "Phase 1"
        }
    }

    pub fn take_intent(&mut self) -> Option<TuiIntent> {
        self.intent.take()
    }

    pub fn pane_summary(&self) -> String {
        format!(
            "focus:{} browser:{} profiles:{} transfers:{} session:{} worker:{}",
            self.focus.label(),
            self.browser.selected,
            self.profiles.selected,
            self.transfers.selected,
            self.session.label(),
            self.worker.label()
        )
    }

    pub fn apply_action(&mut self, action: TuiAction) -> Vec<WorkerCommand> {
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
            TuiAction::Noop => Vec::new(),
        };
        self.sync_pane_state();
        commands
    }

    fn move_selection(&mut self, delta: isize) -> Vec<WorkerCommand> {
        let len = match self.focus {
            TuiFocus::Users => self.context.users.len(),
            TuiFocus::Profiles => self
                .selected_user()
                .map(|user| user.profiles.len())
                .unwrap_or_default(),
            TuiFocus::Actions => TUI_ACTION_ITEMS.len(),
        };
        if len == 0 {
            return Vec::new();
        }
        let target = match self.focus {
            TuiFocus::Users => &mut self.selected_user,
            TuiFocus::Profiles => &mut self.selected_profile,
            TuiFocus::Actions => &mut self.selected_action,
        };
        let next = (*target as isize + delta).clamp(0, len.saturating_sub(1) as isize);
        *target = next as usize;
        if matches!(self.focus, TuiFocus::Users) {
            self.selected_profile = 0;
        }
        self.status = self.contextual_status();
        Vec::new()
    }

    fn focus_next(&mut self) -> Vec<WorkerCommand> {
        self.focus = match self.focus {
            TuiFocus::Users => TuiFocus::Profiles,
            TuiFocus::Profiles => TuiFocus::Actions,
            TuiFocus::Actions => TuiFocus::Users,
        };
        self.status = self.contextual_status();
        Vec::new()
    }

    fn focus_prev(&mut self) -> Vec<WorkerCommand> {
        self.focus = match self.focus {
            TuiFocus::Users => TuiFocus::Actions,
            TuiFocus::Profiles => TuiFocus::Users,
            TuiFocus::Actions => TuiFocus::Profiles,
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
                if self.selected_profile().is_some() {
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
                            let identity = TuiSessionIdentity::from_selection(&user, &profile);
                            let mut session =
                                TuiSessionState::planned_from_selection(&user, &profile);
                            session.begin_connect();
                            self.session = session;
                            self.worker = WorkerEvent::Busy {
                                operation: TuiWorkerOperation::Connect,
                                identity: Some(identity.clone()),
                            };
                            self.status = format!(
                                "Connecting to '{}' for a live read-only listing.",
                                profile.name
                            );
                            return vec![
                                WorkerCommand::OpenSession {
                                    identity,
                                    initial_cwd: profile.initial_path,
                                },
                                WorkerCommand::List {
                                    path: "/".to_string(),
                                },
                            ];
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
                            "{} is planned for {}. Preview: {}",
                            action.title, action.phase, action.command
                        );
                        Vec::new()
                    }
                }
            }
        }
    }

    pub fn apply_worker_event(&mut self, event: WorkerEvent) {
        if let Some(identity) = event_identity(&event) {
            if self.session.identity.as_ref() != Some(identity) {
                return;
            }
        }

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
            }
            WorkerEvent::ListReady { path, result, .. } => {
                self.session.mark_connected(path);
                self.browser.apply_list_result(path.clone(), result.clone());
                self.status = format!(
                    "Listed {}: {} item(s), {} dir(s), {} file(s).",
                    path, result.summary.total, result.summary.dirs, result.summary.files
                );
            }
            WorkerEvent::Failed {
                operation, message, ..
            } => {
                self.session.mark_failed(message.clone());
                self.status = format!("{} failed: {}", operation.label(), message);
            }
            WorkerEvent::Cancelled { operation } => {
                self.session.mark_cancelled();
                self.status = format!("{} cancelled.", operation.label());
            }
        }
        self.worker = event;
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
                    format!(
                        "Profile '{}' selected. Right/Enter moves to actions.",
                        profile.name
                    )
                })
                .unwrap_or_else(|| "No profile available for this user yet.".to_string()),
            TuiFocus::Actions => {
                let action = self.selected_action();
                format!("Action '{}': {}", action.title, action.description)
            }
        }
    }

    fn sync_pane_state(&mut self) {
        self.browser.selected = self.selected_user;
        self.profiles.selected = self.selected_profile;
        self.transfers.selected = self.selected_action;
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
        WorkerEvent::SessionReady { identity, .. } | WorkerEvent::ListReady { identity, .. } => {
            Some(identity)
        }
        WorkerEvent::Idle | WorkerEvent::PathReady { .. } | WorkerEvent::Cancelled { .. } => None,
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TuiFocus {
    Users,
    Profiles,
    Actions,
}

impl TuiFocus {
    pub fn label(self) -> &'static str {
        match self {
            TuiFocus::Users => "users",
            TuiFocus::Profiles => "profiles",
            TuiFocus::Actions => "actions",
        }
    }
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
    pub command: &'static str,
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

pub const TUI_ACTION_ITEMS: &[TuiActionItem] = &[
    TuiActionItem {
        title: "Profiles navigator",
        command: "aeroftp-cli --user USER profiles -i",
        description: "Open the existing profile navigator for the selected user.",
        phase: "P1 ready",
        intent: TuiActionIntent::ProfilesInteractive,
    },
    TuiActionItem {
        title: "List root",
        command: "aeroftp-cli --user USER --profile N ls / -l",
        description: "List the selected profile root through cmd_ls.",
        phase: "P1 ready",
        intent: TuiActionIntent::Profile(TuiProfileAction::ListRoot),
    },
    TuiActionItem {
        title: "Tree",
        command: "aeroftp-cli --user USER --profile N tree / -d 2",
        description: "Show a shallow tree through cmd_tree.",
        phase: "P1 ready",
        intent: TuiActionIntent::Profile(TuiProfileAction::Tree),
    },
    TuiActionItem {
        title: "Quota",
        command: "aeroftp-cli --user USER --profile N df",
        description: "Read storage quota through cmd_df.",
        phase: "P1 ready",
        intent: TuiActionIntent::Profile(TuiProfileAction::Quota),
    },
    TuiActionItem {
        title: "Disk usage",
        command: "aeroftp-cli --user USER --profile N ncdu /",
        description: "Open the existing ncdu explorer for the selected profile.",
        phase: "P1 ready",
        intent: TuiActionIntent::Profile(TuiProfileAction::DiskUsage),
    },
    TuiActionItem {
        title: "Transfers",
        command: "aeroftp-cli get|put --profile N ...",
        description: "Live transfer queue with ratatui gauges fed by worker progress events.",
        phase: "P2/P3",
        intent: TuiActionIntent::Planned,
    },
    TuiActionItem {
        title: "Command palette",
        command: ": <any aeroftp-cli command>",
        description: "Parse line-mode commands without re-implementing handlers.",
        phase: "P3",
        intent: TuiActionIntent::Planned,
    },
];

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
                id: 1,
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
                    initial_path: "/".to_string(),
                    favorite: true,
                }],
            }],
            initial_user: 0,
        }
    }

    #[test]
    fn locked_user_activation_routes_to_existing_unlock_flow() {
        let context = TuiContext {
            users: vec![TuiUser {
                id: 1,
                name: "locked".to_string(),
                is_active: false,
                is_locked: true,
                is_admin: false,
                profile_count: 2,
                profiles: Vec::new(),
            }],
            initial_user: 0,
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
        app.selected_action = 1;
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
        let ready_actions = [
            (0, None),
            (1, Some(TuiProfileAction::ListRoot)),
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
        for selected_action in 0..TUI_ACTION_ITEMS.len() {
            if !matches!(
                TUI_ACTION_ITEMS[selected_action].intent,
                TuiActionIntent::Planned
            ) {
                continue;
            }

            let mut app = AppState::new(sample_context());
            app.focus = TuiFocus::Actions;
            app.selected_action = selected_action;
            app.apply_action(TuiAction::Activate);

            assert!(!app.should_quit);
            assert_eq!(app.take_intent(), None);
            assert!(app.status.contains("is planned for"));
        }
    }

    #[test]
    fn live_list_root_emits_worker_commands_without_exiting() {
        let mut app = AppState::new_live(sample_context());
        app.focus = TuiFocus::Actions;
        app.selected_action = 1;

        let commands = app.apply_action(TuiAction::Activate);

        assert!(!app.should_quit);
        assert_eq!(app.take_intent(), None);
        assert_eq!(
            commands,
            vec![
                WorkerCommand::OpenSession {
                    identity: TuiSessionIdentity {
                        user_name: "default".to_string(),
                        profile_selector: "1".to_string(),
                        profile_name: "Production".to_string(),
                        protocol: "sftp".to_string(),
                        host: "example.com".to_string(),
                    },
                    initial_cwd: "/".to_string(),
                },
                WorkerCommand::List {
                    path: "/".to_string(),
                },
            ]
        );
        assert_eq!(app.session.phase, TuiSessionPhase::Connecting);
    }

    #[test]
    fn live_worker_ignores_events_for_previous_profile_selection() {
        let mut context = sample_context();
        context.users[0].profiles.push(TuiProfile {
            selector: "2".to_string(),
            name: "Archive".to_string(),
            protocol: "s3".to_string(),
            host: "s3.example.com".to_string(),
            initial_path: "/archive".to_string(),
            favorite: false,
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
            identity: stale_identity,
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
            initial_path: "bucket/backups/".to_string(),
            favorite: false,
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
                id: 1,
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
                    initial_path: "/".to_string(),
                    favorite: false,
                }],
            }],
            initial_user: 0,
        };
        let app = AppState::new(context);

        assert_eq!(app.session, TuiSessionState::default());
    }
}
