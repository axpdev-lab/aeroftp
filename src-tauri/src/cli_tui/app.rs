use crate::cli_tui::{
    event::TuiAction,
    panes::{
        browser::BrowserPaneState, profiles::ProfilesPaneState, transfers::TransfersPaneState,
    },
    worker::WorkerEvent,
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
    pub worker: WorkerEvent,
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
            worker: WorkerEvent::Idle,
            intent: None,
        };
        state.sync_pane_state();
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

    pub fn pane_summary(&self) -> String {
        format!(
            "focus:{} browser:{} profiles:{} transfers:{} worker:{}",
            self.focus.label(),
            self.browser.selected,
            self.profiles.selected,
            self.transfers.selected,
            self.worker.label()
        )
    }

    pub fn apply_action(&mut self, action: TuiAction) {
        match action {
            TuiAction::Quit => self.finish(TuiIntent::Quit),
            TuiAction::MoveDown => self.move_selection(1),
            TuiAction::MoveUp => self.move_selection(-1),
            TuiAction::MoveLeft => self.focus_prev(),
            TuiAction::MoveRight => self.focus_next(),
            TuiAction::Activate => self.activate(),
            TuiAction::Noop => {}
        }
        self.sync_pane_state();
    }

    fn move_selection(&mut self, delta: isize) {
        let len = match self.focus {
            TuiFocus::Users => self.context.users.len(),
            TuiFocus::Profiles => self
                .selected_user()
                .map(|user| user.profiles.len())
                .unwrap_or_default(),
            TuiFocus::Actions => TUI_ACTION_ITEMS.len(),
        };
        if len == 0 {
            return;
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
    }

    fn focus_next(&mut self) {
        self.focus = match self.focus {
            TuiFocus::Users => TuiFocus::Profiles,
            TuiFocus::Profiles => TuiFocus::Actions,
            TuiFocus::Actions => TuiFocus::Users,
        };
        self.status = self.contextual_status();
    }

    fn focus_prev(&mut self) {
        self.focus = match self.focus {
            TuiFocus::Users => TuiFocus::Actions,
            TuiFocus::Profiles => TuiFocus::Users,
            TuiFocus::Actions => TuiFocus::Profiles,
        };
        self.status = self.contextual_status();
    }

    fn activate(&mut self) {
        let Some(user) = self.selected_user().cloned() else {
            self.status = "No users found in the local vault.".to_string();
            return;
        };

        if user.is_locked {
            self.finish(TuiIntent::ProfilesInteractive {
                user_name: user.name,
            });
            return;
        }

        match self.focus {
            TuiFocus::Users => {
                self.focus = TuiFocus::Profiles;
                self.status = self.contextual_status();
            }
            TuiFocus::Profiles => {
                if self.selected_profile().is_some() {
                    self.focus = TuiFocus::Actions;
                    self.status = self.contextual_status();
                } else {
                    self.status = format!("User '{}' has no visible profiles.", user.name);
                }
            }
            TuiFocus::Actions => {
                let action = self.selected_action();
                match action.intent {
                    TuiActionIntent::ProfilesInteractive => {
                        self.finish(TuiIntent::ProfilesInteractive {
                            user_name: user.name,
                        });
                    }
                    TuiActionIntent::Profile(action) => {
                        let Some(profile) = self.selected_profile().cloned() else {
                            self.status =
                                "Select a profile before running profile actions.".to_string();
                            return;
                        };
                        self.finish(TuiIntent::ProfileAction {
                            user_name: user.name,
                            profile_selector: profile.selector,
                            action,
                        });
                    }
                    TuiActionIntent::Planned => {
                        self.status = format!(
                            "{} is planned for {}. Preview: {}",
                            action.title, action.phase, action.command
                        );
                    }
                }
            }
        }
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
}
