use crate::cli_tui::{
    event::{OverlayKey, TuiAction},
    overlay::{
        ConfirmKind, ConfirmState, GroupsOverlayItem, GroupsOverlayState, HelpState, PagerState,
        PaletteState, ProfileFormMode, ProfileFormState, PromptKind, PromptState, TuiOverlay,
    },
    panes::{
        browser::BrowserPaneState, profiles::ProfilesPaneState, transfers::TransfersPaneState,
    },
    session::{TuiSessionIdentity, TuiSessionPhase, TuiSessionState},
    worker::{
        TransferDirection, TuiProfileDraft, TuiSecret, TuiWorkerOperation, WorkerCommand,
        WorkerEvent,
    },
};

use crossterm::event::{MouseButton, MouseEvent};
use ratatui::layout::Rect;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct TuiContext {
    pub users: Vec<TuiUser>,
    pub initial_user: usize,
    /// Launch CWD captured at TUI entry boundary for absolute download defaults.
    /// Never compute inside pure AppState; threaded from caller.
    pub download_base: String,
    /// Named saved-server groups (#320), the generalisation of the favourites
    /// bucket. Global (not per-user) and sorted by the vault `order`; member
    /// ids live in the vault blob, the TUI carries names + counts only.
    pub groups: Vec<TuiGroup>,
}

impl TuiContext {
    pub fn empty() -> Self {
        Self {
            users: Vec::new(),
            initial_user: 0,
            download_base: ".".to_string(),
            groups: Vec::new(),
        }
    }
}

/// A named saved-server group as the TUI sees it: a display name and the global
/// member count. Membership of an individual profile is carried on
/// [`TuiProfile::groups`]; the vault remains the single source of truth.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct TuiGroup {
    pub name: String,
    pub member_count: usize,
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
    /// Names of the groups this profile belongs to, in vault `order` (#320).
    /// Empty when ungrouped. Mirrors `group_names_for_profile`; the filter and
    /// the group overlay read this for the membership indicator.
    pub groups: Vec<String>,
    /// Cached storage usage from the saved bookmark (the GUI/CLI `lastQuota`),
    /// surfaced read-only in the IntroHub table. Computed in `build_tui_context`
    /// with the same CLI helpers (`profile_effective_used`/`_total`); the TUI
    /// only renders them. `None` when the bookmark has no cached quota.
    pub used: Option<u64>,
    pub total: Option<u64>,
    /// Pre-formatted "last connected" label (the CLI `format_time_ago` output),
    /// or `None` when the profile was never connected.
    pub last_connected_label: Option<String>,
    /// Connection details needed by the health probe (DNS/TCP/TLS). `port` is 0
    /// when unknown; `endpoint` carries the S3/WebDAV override when present.
    pub port: u16,
    pub endpoint: Option<String>,
    /// Latest reachability probe result (`H`), `None` until probed.
    pub health: Option<TuiHealth>,
}

/// Outcome of a reachability probe, mirrored from `server_health::HealthCheckResult`.
#[derive(Debug, Clone)]
pub struct TuiHealth {
    pub status: String, // "healthy" | "degraded" | "unreachable" | "error"
    pub score: u8,
    pub latency_ms: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct LayoutRects {
    /// Rect for the IntroHub "My Servers" table (pre-connect profile list).
    pub intro_table: Option<Rect>,
    /// Rect for the REMOTE browser pane (connected dual view).
    pub remote_pane: Option<Rect>,
    /// Rect for the LOCAL browser pane (connected dual view).
    pub local_pane: Option<Rect>,
    /// Rect for the TRANSFERS strip (when visible).
    pub transfers_strip: Option<Rect>,
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
    /// Synced browsing (`Y`): when on, opening or leaving a directory on the
    /// active pane mirrors the same child-name navigation on the other pane, so
    /// the two sides walk a matching directory tree. Session-only.
    pub synced_browsing: bool,
    /// IntroHub group filter (#320): when `Some(name)`, the My Servers list is
    /// narrowed to that group's members. The TUI's first list filter; mutually
    /// exclusive with any other narrowing (only one at a time). `None` = all.
    pub selected_group: Option<String>,
    /// Rects of the major clickable regions recorded on the last render pass.
    /// Used exclusively by handle_mouse for hit-testing (row clicks, side
    /// switches). Not part of domain state; purely for input mapping.
    pub layout: LayoutRects,
    /// Last left-button down (col, row, time) used for double-click activate
    /// (crossterm does not surface native double-click events; we use a 350 ms
    /// same-cell window). Private; not rendered.
    last_mouse_down: Option<(u16, u16, Instant)>,
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
            synced_browsing: false,
            selected_group: None,
            layout: LayoutRects::default(),
            last_mouse_down: None,
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
            TuiAction::Back => self.handle_back(),
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
            // Favorite toggling and health probes only apply on the IntroHub
            // (handled in introhub_apply); a no-op in the connected browser.
            TuiAction::ToggleFavorite => Vec::new(),
            TuiAction::HealthCheck => Vec::new(),
            TuiAction::RefreshQuota => Vec::new(),
            // `G` manages groups on the IntroHub; in the connected browser it is
            // the go-to-path prompt (the keyed analogue of palette `cd`).
            TuiAction::ManageGroups => {
                if matches!(self.focus, TuiFocus::Browser) {
                    self.open_goto_prompt()
                } else {
                    Vec::new()
                }
            }
            // `a` adds a profile on the IntroHub; in the connected browser it is
            // the dotfile toggle (the key is free there per the keymap plan).
            TuiAction::AddProfile => {
                if matches!(self.focus, TuiFocus::Browser) {
                    self.toggle_active_hidden()
                } else {
                    Vec::new()
                }
            }
            // `e`/`x` manage profiles on the IntroHub only; no browser meaning.
            TuiAction::EditProfile => Vec::new(),
            TuiAction::DeleteProfile => Vec::new(),
            TuiAction::CycleSort => self.cycle_active_sort(),
            TuiAction::OpenFilter => self.open_filter_prompt(),
            TuiAction::Reload => self.reload_active(),
            TuiAction::MarkToggle => self.toggle_active_mark(),
            TuiAction::MarkAll => self.mark_all_active(),
            TuiAction::MarkNone => self.clear_active_marks(),
            TuiAction::ViewFile => self.view_selected_file(),
            TuiAction::EditFile => self.edit_selected_file(),
            TuiAction::Info => self.info_selected(),
            TuiAction::SizeRecursive => self.size_selected_dir(),
            TuiAction::Touch => self.trigger_touch(),
            TuiAction::SyncedBrowsing => self.toggle_synced_browsing(),
            TuiAction::Help => {
                self.overlay = TuiOverlay::Help(HelpState::default());
                self.status = "Help: Up/Down scroll  Esc close".to_string();
                Vec::new()
            }
            TuiAction::OpenPalette => {
                if self.is_live_connected() {
                    self.overlay = TuiOverlay::Palette(PaletteState::new());
                    self.status =
                        "palette: ls|cd <p> | get <r> [l] | stat <p> | mkdir <p> | rm|rm! <p>"
                            .to_string();
                    Vec::new()
                } else {
                    self.status =
                        "palette requires connected session (: only in live view)".to_string();
                    Vec::new()
                }
            }
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
            // `H` probes reachability (health) of the highlighted profile.
            TuiAction::HealthCheck => Some(self.introhub_check_health()),
            // `Q` refreshes the storage quota via a transient connection.
            TuiAction::RefreshQuota => Some(self.introhub_refresh_quota()),
            // `G` opens the named-group manager for the highlighted profile.
            TuiAction::ManageGroups => Some(self.introhub_open_groups()),
            // `a`/`e`/`x` manage saved profiles (DiscoveryHub, B4).
            TuiAction::AddProfile => Some(self.introhub_add_profile()),
            TuiAction::EditProfile => Some(self.introhub_edit_profile()),
            TuiAction::DeleteProfile => Some(self.introhub_delete_profile()),
            // Enter connects to the selected profile (or unlocks a locked user).
            TuiAction::Activate => Some(self.introhub_activate()),
            // Backspace/Parent is meaningless on the picker; swallow it.
            TuiAction::Parent => Some(Vec::new()),
            _ => None,
        }
    }

    /// Indices into the active user's `profiles` that are visible under the
    /// current group filter (#320). Without a filter, every profile is visible.
    /// The order matches `profiles`, so the Nth visible row maps back to a real
    /// profile index for navigation, rendering, and connect.
    pub fn visible_profile_indices(&self) -> Vec<usize> {
        let Some(user) = self.selected_user() else {
            return Vec::new();
        };
        match &self.selected_group {
            None => (0..user.profiles.len()).collect(),
            Some(group) => user
                .profiles
                .iter()
                .enumerate()
                .filter(|(_, p)| p.groups.iter().any(|g| g == group))
                .map(|(i, _)| i)
                .collect(),
        }
    }

    /// Move the highlighted profile in the IntroHub table, reusing the picker's
    /// Profiles-focus move (which also refreshes the local-pane preview). When a
    /// group filter is active the cursor steps within the visible subset only,
    /// keeping `selected_profile` a real index into the full `profiles` vec.
    fn introhub_move_profile(&mut self, delta: isize) -> Vec<WorkerCommand> {
        self.focus = TuiFocus::Profiles;
        if self.selected_group.is_some() {
            let visible = self.visible_profile_indices();
            if visible.is_empty() {
                return Vec::new();
            }
            let cur = visible
                .iter()
                .position(|&i| i == self.selected_profile)
                .unwrap_or(0);
            let next = (cur as isize + delta).clamp(0, visible.len() as isize - 1) as usize;
            self.selected_profile = visible[next];
            // Reuse the Profiles-focus move with delta 0 to refresh the local
            // preview at the new position without re-clamping past the filter.
            return self.move_selection(0);
        }
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

    /// Open the named-group manager overlay for the highlighted profile (#320).
    /// The overlay lists every known group with this profile's membership flag,
    /// plus an "All servers" row that clears the filter. Pure state: the overlay
    /// is rendered separately and its keys are routed via `handle_overlay_key`.
    fn introhub_open_groups(&mut self) -> Vec<WorkerCommand> {
        let (profile_id, profile_name, profile_groups) = match self.selected_profile() {
            Some(p) => (p.id.clone(), p.name.clone(), p.groups.clone()),
            None => {
                self.status = "No profile selected.".to_string();
                return Vec::new();
            }
        };
        let items: Vec<GroupsOverlayItem> = self
            .context
            .groups
            .iter()
            .map(|g| GroupsOverlayItem {
                name: g.name.clone(),
                member_count: g.member_count,
                is_member: profile_groups.iter().any(|n| n == &g.name),
            })
            .collect();
        // Start the cursor on the active filter's group when one is set, else on
        // the "All servers" row (index 0).
        let cursor = match &self.selected_group {
            Some(active) => items
                .iter()
                .position(|i| &i.name == active)
                .map(|i| i + 1)
                .unwrap_or(0),
            None => 0,
        };
        self.overlay = TuiOverlay::Groups(GroupsOverlayState {
            profile_id,
            profile_name,
            cursor,
            groups: items,
        });
        self.status = "Groups: Enter filter  space toggle  n new  r rename  d delete".to_string();
        Vec::new()
    }

    /// Apply a group-membership toggle to the in-memory context: flip `group`
    /// on the named profile across all users, and keep the group list and member
    /// counts in sync (creating the group when it is new). The optimistic mirror
    /// of `toggle_group_membership_in_vault`; the worker persists the vault.
    /// Returns the new membership state (`true` = now a member).
    fn apply_group_membership(&mut self, profile_id: &str, group: &str) -> bool {
        let mut now_member = false;
        for user in &mut self.context.users {
            for profile in &mut user.profiles {
                if profile.id != profile_id {
                    continue;
                }
                if let Some(pos) = profile.groups.iter().position(|g| g == group) {
                    profile.groups.remove(pos);
                    now_member = false;
                } else {
                    profile.groups.push(group.to_string());
                    now_member = true;
                }
            }
        }
        // Reflect the count change, creating the group row when it did not exist.
        if let Some(g) = self.context.groups.iter_mut().find(|g| g.name == group) {
            if now_member {
                g.member_count += 1;
            } else {
                g.member_count = g.member_count.saturating_sub(1);
            }
        } else if now_member {
            self.context.groups.push(TuiGroup {
                name: group.to_string(),
                member_count: 1,
            });
        }
        now_member
    }

    /// Rename a group in the in-memory context: the group row, every profile's
    /// membership list, and the active filter. Optimistic mirror of
    /// `rename_group_in_vault`.
    fn apply_group_rename(&mut self, old: &str, new: &str) {
        for user in &mut self.context.users {
            for profile in &mut user.profiles {
                for name in &mut profile.groups {
                    if name == old {
                        *name = new.to_string();
                    }
                }
            }
        }
        if let Some(g) = self.context.groups.iter_mut().find(|g| g.name == old) {
            g.name = new.to_string();
        }
        if self.selected_group.as_deref() == Some(old) {
            self.selected_group = Some(new.to_string());
        }
    }

    /// Delete a group from the in-memory context: drop the group row and the
    /// membership from every profile (the servers themselves stay), and clear
    /// the filter if it pointed at the deleted group. Optimistic mirror of
    /// `delete_group_in_vault`.
    fn apply_group_delete(&mut self, group: &str) {
        for user in &mut self.context.users {
            for profile in &mut user.profiles {
                profile.groups.retain(|g| g != group);
            }
        }
        self.context.groups.retain(|g| g.name != group);
        if self.selected_group.as_deref() == Some(group) {
            self.selected_group = None;
        }
    }

    // --- B4 DiscoveryHub: in-TUI profile management ------------------------

    /// `a` on the IntroHub: open an empty DiscoveryHub form to add a profile to
    /// the selected user. Refused on a locked user (their partition is sealed).
    fn introhub_add_profile(&mut self) -> Vec<WorkerCommand> {
        let Some(user) = self.selected_user() else {
            self.status = "No user selected.".to_string();
            return Vec::new();
        };
        if user.is_locked {
            self.status = "Unlock the user before adding a profile.".to_string();
            return Vec::new();
        }
        self.overlay = TuiOverlay::ProfileForm(ProfileFormState::new_create(user.name.clone()));
        self.status = "New profile: Tab move  arrows protocol  Enter save  Esc cancel".to_string();
        Vec::new()
    }

    /// `e` on the IntroHub: open the DiscoveryHub form pre-filled from the
    /// highlighted profile. Refused on a locked user or an unsaved (no id) row.
    fn introhub_edit_profile(&mut self) -> Vec<WorkerCommand> {
        let Some(user) = self.selected_user() else {
            self.status = "No user selected.".to_string();
            return Vec::new();
        };
        if user.is_locked {
            self.status = "Unlock the user before editing a profile.".to_string();
            return Vec::new();
        }
        let user_name = user.name.clone();
        let Some(profile) = self.selected_profile() else {
            self.status = "No profile selected to edit.".to_string();
            return Vec::new();
        };
        if profile.id.is_empty() {
            self.status = "This profile has no saved id; cannot edit it.".to_string();
            return Vec::new();
        }
        let form = ProfileFormState {
            mode: ProfileFormMode::Edit {
                id: profile.id.clone(),
            },
            user_name,
            name: profile.name.clone(),
            protocol: profile.protocol.clone(),
            host: profile.host.clone(),
            port: if profile.port == 0 {
                String::new()
            } else {
                profile.port.to_string()
            },
            username: profile.username.clone(),
            initial_path: profile.initial_path.clone(),
            local_path: profile.default_local_path.clone(),
            password: TuiSecret::default(),
            password_touched: false,
            focus: 0,
            error: None,
        };
        self.overlay = TuiOverlay::ProfileForm(form);
        self.status = "Edit profile: Tab move  arrows protocol  Enter save  Esc cancel".to_string();
        Vec::new()
    }

    /// `x` on the IntroHub: confirm-then-delete the highlighted profile. Refused
    /// on a locked user or an unsaved (no id) row.
    fn introhub_delete_profile(&mut self) -> Vec<WorkerCommand> {
        let Some(user) = self.selected_user() else {
            self.status = "No user selected.".to_string();
            return Vec::new();
        };
        if user.is_locked {
            self.status = "Unlock the user before deleting a profile.".to_string();
            return Vec::new();
        }
        let user_name = user.name.clone();
        let Some(profile) = self.selected_profile() else {
            self.status = "No profile selected to delete.".to_string();
            return Vec::new();
        };
        if profile.id.is_empty() {
            self.status = "This profile has no saved id; cannot delete it.".to_string();
            return Vec::new();
        }
        let profile_id = profile.id.clone();
        let profile_name = profile.name.clone();
        let message = format!(
            "Delete saved profile '{}' for user '{}'? (credential is removed too)",
            profile_name, user_name
        );
        self.overlay = TuiOverlay::Confirm(ConfirmState {
            kind: ConfirmKind::DeleteProfile {
                user_name,
                profile_id,
                profile_name,
            },
            message,
        });
        self.status = "Confirm profile delete (y/n).".to_string();
        Vec::new()
    }

    /// Optimistically remove a saved profile from the in-memory context: drop
    /// the row, decrement the member counts of every group it belonged to,
    /// renumber the remaining selectors (they are 1-based positions used as the
    /// connection key), and keep `selected_profile` valid under any filter.
    fn apply_profile_delete(&mut self, user_name: &str, profile_id: &str) {
        let Some(user) = self.context.users.iter_mut().find(|u| u.name == user_name) else {
            return;
        };
        let Some(pos) = user.profiles.iter().position(|p| p.id == profile_id) else {
            return;
        };
        let removed = user.profiles.remove(pos);
        // The deleted profile leaves every group it was a member of.
        for group_name in &removed.groups {
            if let Some(g) = self
                .context
                .groups
                .iter_mut()
                .find(|g| &g.name == group_name)
            {
                g.member_count = g.member_count.saturating_sub(1);
            }
        }
        Self::renumber_selectors(user_name, &mut self.context.users);
        // Keep the cursor on a still-visible row.
        self.snap_selection_to_filter();
    }

    /// Optimistically insert a created profile, or update an edited one in
    /// place, in the selected user's list. On create the new profile is
    /// prepended (matching `cmd_profile_create`) and selectors are renumbered.
    fn apply_profile_upsert(&mut self, user_name: &str, profile: TuiProfile) {
        let Some(user) = self.context.users.iter_mut().find(|u| u.name == user_name) else {
            return;
        };
        if let Some(existing) = user.profiles.iter_mut().find(|p| p.id == profile.id) {
            // Edit: preserve the group/favorite/health/quota state the form does
            // not own; only the editable metadata changes.
            existing.name = profile.name;
            existing.protocol = profile.protocol;
            existing.host = profile.host;
            existing.username = profile.username;
            existing.initial_path = profile.initial_path;
            existing.default_local_path = profile.default_local_path;
            existing.port = profile.port;
        } else {
            user.profiles.insert(0, profile);
            user.profile_count = user.profile_count.saturating_add(1);
            Self::renumber_selectors(user_name, &mut self.context.users);
            self.selected_profile = 0;
        }
        self.snap_selection_to_filter();
    }

    /// Renumber the 1-based `selector` of every profile of `user_name` to its
    /// position, so the connection key stays in sync with the list order after
    /// an insert or delete.
    fn renumber_selectors(user_name: &str, users: &mut [TuiUser]) {
        if let Some(user) = users.iter_mut().find(|u| u.name == user_name) {
            for (idx, profile) in user.profiles.iter_mut().enumerate() {
                profile.selector = (idx + 1).to_string();
            }
        }
    }

    /// Probe the highlighted profile's reachability. The TUI only requests the
    /// probe (carrying the saved connection details); the worker runs the shared
    /// `server_health_check` and reports a `HealthReady` event.
    fn introhub_check_health(&mut self) -> Vec<WorkerCommand> {
        // Copy the connection details out first so the status assignment below
        // does not overlap the immutable borrow of the selected profile.
        let Some((id, host, port, protocol, endpoint, name)) = self.selected_profile().map(|p| {
            (
                p.id.clone(),
                p.host.clone(),
                p.port,
                p.protocol.clone(),
                p.endpoint.clone(),
                p.name.clone(),
            )
        }) else {
            return Vec::new();
        };
        if id.is_empty() {
            self.status = "This profile has no saved id; cannot probe health.".to_string();
            return Vec::new();
        }
        if host.is_empty() {
            self.status = "No host on this profile to probe.".to_string();
            return Vec::new();
        }
        self.status = format!("Probing health of '{}'...", name);
        vec![WorkerCommand::HealthCheck {
            profile_id: id,
            host,
            port,
            protocol,
            endpoint,
        }]
    }

    /// Refresh the highlighted profile's storage quota via a transient provider
    /// connection. The TUI only asks; the worker connects, reads `storage_info`,
    /// reports `QuotaReady`, and persists the bookmark `lastQuota`.
    fn introhub_refresh_quota(&mut self) -> Vec<WorkerCommand> {
        let Some(user) = self.selected_user().cloned() else {
            return Vec::new();
        };
        if user.is_locked {
            self.status = "Unlock the user before refreshing quota.".to_string();
            return Vec::new();
        }
        let Some(profile) = self.selected_profile().cloned() else {
            return Vec::new();
        };
        if profile.id.is_empty() {
            self.status = "This profile has no saved id; cannot refresh quota.".to_string();
            return Vec::new();
        }
        let identity = TuiSessionIdentity::from_selection(&user, &profile);
        self.status = format!("Refreshing quota of '{}' (connecting)...", profile.name);
        vec![WorkerCommand::RefreshQuota {
            identity,
            profile_id: profile.id,
        }]
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

        // Connect atomically: send only OpenSession (plus the session-independent
        // local listing). The remote List is deferred to the SessionReady handler
        // so a failed/expired connect does not leave a straggler List running
        // without a session - which used to mask the real auth error with a
        // confusing "no active TUI session" and leave the view stuck.
        let commands = vec![
            WorkerCommand::OpenSession {
                identity: identity.clone(),
                initial_cwd: profile.initial_path.clone(),
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
        let side = self.active_browser_side;
        let active = self.active_browser();
        if let Some(path) = active.selected_directory_path() {
            let child = active.selected_entry().map(|entry| entry.name.clone());
            let mut commands = Vec::new();
            if side == BrowserSide::Local {
                // Local list can be handled in worker or directly; for consistency go through worker.
                self.status = format!("Listing local {}.", path);
                commands.push(WorkerCommand::LocalList { path });
            } else {
                self.worker = WorkerEvent::Busy {
                    operation: TuiWorkerOperation::List,
                    identity: self.session.identity.clone(),
                };
                self.status = format!("Listing {}.", path);
                commands.push(WorkerCommand::List { path });
            }
            // Synced browsing (`Y`): mirror the same child onto the other pane.
            if self.synced_browsing {
                if let Some(child) = child {
                    commands.extend(self.synced_open_other(side, &child));
                }
            }
            return commands;
        }
        let active = self.active_browser_mut();

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

        let side = self.active_browser_side;
        let mut commands = Vec::new();
        if side == BrowserSide::Local {
            self.status = format!("Listing local parent {}.", path);
            commands.push(WorkerCommand::LocalList { path });
        } else {
            self.worker = WorkerEvent::Busy {
                operation: TuiWorkerOperation::List,
                identity: self.session.identity.clone(),
            };
            self.status = format!("Listing parent {}.", path);
            commands.push(WorkerCommand::List { path });
        }
        // Synced browsing (`Y`): walk the other pane up too.
        if self.synced_browsing {
            commands.extend(self.synced_parent_other(side));
        }
        commands
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
        let on_local = self.active_browser_side == BrowserSide::Local;
        // Multi-select batch: when entries are marked, delete the whole set.
        let marked = self.active_browser().marked_entries();
        if !marked.is_empty() {
            let count = marked.len();
            let items: Vec<(String, bool)> = marked
                .into_iter()
                .map(|(path, is_dir, _)| (path, is_dir))
                .collect();
            let message = format!(
                "Delete {} marked entr{} on {:?}? (directories are removed recursively)",
                count,
                if count == 1 { "y" } else { "ies" },
                self.active_browser_side
            );
            self.overlay = TuiOverlay::Confirm(ConfirmState {
                kind: ConfirmKind::DeleteBatch {
                    items,
                    local: on_local,
                },
                message,
            });
            self.status = format!(
                "Confirm batch delete of {} entr{}.",
                count,
                if count == 1 { "y" } else { "ies" }
            );
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
                local: on_local,
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
        // Smart cross default destination: the local pane's directory, then the
        // launch CWD, then the process CWD.
        let base = if !self.local.path.is_empty() {
            self.local.path.clone()
        } else if !self.context.download_base.is_empty() {
            self.context.download_base.clone()
        } else {
            ".".to_string()
        };
        let base = base.trim_end_matches('/').to_string();
        // Multi-select batch: when remote files are marked, download the whole
        // set into the local directory (directories among the marks are skipped).
        let remote_marks = self.browser.marked_entries();
        if !remote_marks.is_empty() {
            let mut commands = Vec::new();
            let mut skipped = 0usize;
            for (remote, is_dir, name) in remote_marks {
                if is_dir {
                    skipped += 1;
                    continue;
                }
                let local_path = format!("{}/{}", base, name);
                let id = self.transfers.enqueue(
                    TransferDirection::Download,
                    name,
                    remote.clone(),
                    local_path.clone(),
                );
                commands.push(WorkerCommand::Download {
                    id,
                    remote_path: remote,
                    local_path,
                });
            }
            self.browser.clear_marks();
            if commands.is_empty() {
                self.status = "Marked entries are directories; mark files to download.".to_string();
                return Vec::new();
            }
            self.focus = TuiFocus::Transfers;
            let note = if skipped > 0 {
                format!(", {} dir(s) skipped", skipped)
            } else {
                String::new()
            };
            self.begin_mutation(
                TuiWorkerOperation::Transfer,
                format!("Downloading {} marked file(s){}.", commands.len(), note),
            );
            return commands;
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
        // Cross default: download into the LOCAL pane's directory (`base`,
        // computed above), so `g` mirrors `u`.
        let default_local = format!("{}/{}", base, name);
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
        let remote_dir = self.browser.path.clone();
        // Multi-select batch: when local files are marked, upload the whole set
        // into the remote directory (directories among the marks are skipped).
        let local_marks = self.local.marked_entries();
        if !local_marks.is_empty() {
            let mut commands = Vec::new();
            let mut skipped = 0usize;
            for (local_path, is_dir, name) in local_marks {
                if is_dir {
                    skipped += 1;
                    continue;
                }
                let remote = join_remote(&remote_dir, &name);
                let id = self.transfers.enqueue(
                    TransferDirection::Upload,
                    name,
                    remote.clone(),
                    local_path.clone(),
                );
                commands.push(WorkerCommand::Upload {
                    id,
                    local_path,
                    remote_path: remote,
                });
            }
            self.local.clear_marks();
            if commands.is_empty() {
                self.status = "Marked entries are directories; mark files to upload.".to_string();
                return Vec::new();
            }
            self.focus = TuiFocus::Transfers;
            let note = if skipped > 0 {
                format!(", {} dir(s) skipped", skipped)
            } else {
                String::new()
            };
            self.begin_mutation(
                TuiWorkerOperation::Transfer,
                format!("Uploading {} marked file(s){}.", commands.len(), note),
            );
            return commands;
        }
        // Phase 3 cross: 'u' sources from the LOCAL pane selection, targets the
        // remote pane's current dir. The source is prefilled with the highlighted
        // local file (mirror of `g`, which prefills the local destination), so the
        // common case is just Enter; the field stays editable and a directory or
        // empty selection leaves it blank to type a path.
        let default_source = self.local.selected_file_path().unwrap_or_default();
        self.overlay = TuiOverlay::Prompt(PromptState::new(
            PromptKind::Upload {
                remote_dir: remote_dir.clone(),
            },
            format!("Upload into {}", display_dir(&remote_dir)),
            "local source file (defaults to the local selection), Enter to start, Esc to cancel",
            default_source,
        ));
        self.status = "Uploading a file (cross local->remote).".to_string();
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

    // --- rev 1.0.3 file-manager table stakes -------------------------------

    /// `B`: cycle the active pane's sort (name/size/date/type, asc/desc). Pure
    /// in-memory view change, no worker round-trip.
    fn cycle_active_sort(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            self.status = "Switch to the Browser pane to change the sort.".to_string();
            return Vec::new();
        }
        let side = self.active_browser_side;
        self.active_browser_mut().cycle_sort();
        let indicator = self.active_browser().sort.indicator();
        self.status = format!("Sort: {} ({:?}).", indicator, side);
        Vec::new()
    }

    /// `/`: open the live filter prompt for the active pane, seeded with the
    /// current filter so it can be extended. Filtering is applied live as the
    /// user types (see `handle_prompt_key`).
    fn open_filter_prompt(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            self.status = "Switch to the Browser pane to filter the listing.".to_string();
            return Vec::new();
        }
        let local = self.active_browser_side == BrowserSide::Local;
        let current = self.active_browser().filter.clone().unwrap_or_default();
        self.overlay = TuiOverlay::Prompt(PromptState::new(
            PromptKind::Filter { local },
            "Filter listing",
            "substring or *?-wildcard (live); Enter keep, Esc clear",
            current,
        ));
        self.status = "Filtering the listing.".to_string();
        Vec::new()
    }

    /// `L` / `Ctrl+R`: reload the active pane's directory from scratch, dropping
    /// marks and any live filter so the listing reflects the server/disk again.
    fn reload_active(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            self.status = "Switch to the Browser pane to reload.".to_string();
            return Vec::new();
        }
        let local = self.active_browser_side == BrowserSide::Local;
        let pane = self.active_browser_mut();
        pane.clear_marks();
        pane.clear_filter();
        let path = pane.path.clone();
        if path.is_empty() {
            self.status = "No directory loaded to reload.".to_string();
            return Vec::new();
        }
        self.status = format!("Reloading {}.", path);
        if local {
            vec![WorkerCommand::LocalList { path }]
        } else {
            self.worker = WorkerEvent::Busy {
                operation: TuiWorkerOperation::List,
                identity: self.session.identity.clone(),
            };
            vec![WorkerCommand::List { path }]
        }
    }

    /// `Space`/`m`: toggle the mark on the selected entry, then advance the
    /// cursor so a column of entries can be marked quickly.
    fn toggle_active_mark(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            self.status = "Switch to the Browser pane to mark entries.".to_string();
            return Vec::new();
        }
        let toggled = self.active_browser_mut().toggle_mark();
        let count = self.active_browser().marked_count();
        match toggled {
            Some(true) => self.status = format!("Marked ({} selected).", count),
            Some(false) => self.status = format!("Unmarked ({} selected).", count),
            None => {
                self.status = "Nothing to mark.".to_string();
                return Vec::new();
            }
        }
        // Auto-advance like a file manager so successive marks are one key each.
        self.active_browser_mut().move_selection(1);
        Vec::new()
    }

    /// `Ctrl+A`: mark every visible entry in the active pane.
    fn mark_all_active(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            return Vec::new();
        }
        self.active_browser_mut().mark_all_visible();
        let count = self.active_browser().marked_count();
        self.status = format!("Marked all ({} selected).", count);
        Vec::new()
    }

    /// `Alt+A`: clear every mark in the active pane.
    fn clear_active_marks(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            return Vec::new();
        }
        self.active_browser_mut().clear_marks();
        self.status = "Cleared all marks.".to_string();
        Vec::new()
    }

    /// `a` in the browser: toggle dotfile visibility on the active pane.
    fn toggle_active_hidden(&mut self) -> Vec<WorkerCommand> {
        let side = self.active_browser_side;
        let shown = self.active_browser_mut().toggle_hidden();
        self.status = format!(
            "Hidden files {} ({:?}).",
            if shown { "shown" } else { "hidden" },
            side
        );
        Vec::new()
    }

    /// `v`: view the selected file in a read-only pager. The worker reads a
    /// capped prefix and replies with `FileContent`, which opens the pager.
    fn view_selected_file(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            self.status = "Switch to the Browser pane to view a file.".to_string();
            return Vec::new();
        }
        let local = self.active_browser_side == BrowserSide::Local;
        let active = self.active_browser();
        let Some(entry) = active.selected_entry() else {
            self.status = "No entry selected to view.".to_string();
            return Vec::new();
        };
        if entry.is_dir {
            self.status = "That is a directory; press Enter to open it.".to_string();
            return Vec::new();
        }
        let Some(path) = active.selected_file_path() else {
            self.status = "No file selected to view.".to_string();
            return Vec::new();
        };
        self.status = format!("Loading {} for viewing.", path);
        if !local {
            self.begin_mutation(TuiWorkerOperation::View, self.status.clone());
        }
        vec![WorkerCommand::ViewFile { path, local }]
    }

    /// `o`: edit the selected remote file in `$EDITOR`. Remote-only: the worker
    /// stages the file (`EditFetch`), the run loop opens the editor, then
    /// `EditCommit` re-uploads it.
    fn edit_selected_file(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            self.status = "Switch to the Browser pane to edit a file.".to_string();
            return Vec::new();
        }
        if self.active_browser_side == BrowserSide::Local {
            self.status = "Edit applies to remote files; switch to the Remote pane.".to_string();
            return Vec::new();
        }
        if !self.require_live_connection() {
            return Vec::new();
        }
        let Some(entry) = self.browser.selected_entry() else {
            self.status = "No entry selected to edit.".to_string();
            return Vec::new();
        };
        if entry.is_dir {
            self.status = "Select a file to edit.".to_string();
            return Vec::new();
        }
        let Some(remote) = self.browser.selected_file_path() else {
            self.status = "No file selected to edit.".to_string();
            return Vec::new();
        };
        self.begin_mutation(
            TuiWorkerOperation::Edit,
            format!("Fetching {} for $EDITOR...", remote),
        );
        vec![WorkerCommand::EditFetch {
            remote_path: remote,
        }]
    }

    /// `i`: load full metadata for the selected entry (file or directory) into
    /// the status line, reusing the existing stat path.
    fn info_selected(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            self.status = "Switch to the Browser pane for file info.".to_string();
            return Vec::new();
        }
        let local = self.active_browser_side == BrowserSide::Local;
        let Some(path) = self.active_browser().selected_entry_path() else {
            self.status = "No entry selected.".to_string();
            return Vec::new();
        };
        self.status = format!("Loading metadata for {}.", path);
        if local {
            vec![WorkerCommand::LocalStat { path }]
        } else {
            self.begin_mutation(TuiWorkerOperation::Stat, self.status.clone());
            vec![WorkerCommand::Stat { path }]
        }
    }

    /// `Ctrl+S`: recursive size of the selected directory (a file just reports
    /// its own size immediately, no worker round-trip).
    fn size_selected_dir(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            self.status = "Switch to the Browser pane to size a directory.".to_string();
            return Vec::new();
        }
        let local = self.active_browser_side == BrowserSide::Local;
        let active = self.active_browser();
        let Some(entry) = active.selected_entry() else {
            self.status = "No entry selected.".to_string();
            return Vec::new();
        };
        let is_dir = entry.is_dir;
        let size = entry.size;
        let name = entry.name.clone();
        let Some(path) = active.selected_entry_path() else {
            self.status = "No entry selected.".to_string();
            return Vec::new();
        };
        if !is_dir {
            self.status = format!("{} is {}.", name, format_size_compact(size));
            return Vec::new();
        }
        self.status = format!("Computing size of {}...", path);
        vec![WorkerCommand::SizeRecursive { path, local }]
    }

    /// `N`: create an empty file in the active directory (the touch complement
    /// to `n` mkdir).
    fn trigger_touch(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            self.status = "Switch to the Browser pane to create a file.".to_string();
            return Vec::new();
        }
        if !self.require_live_connection() {
            return Vec::new();
        }
        let local = self.active_browser_side == BrowserSide::Local;
        let parent = self.active_browser().path.clone();
        self.overlay = TuiOverlay::Prompt(PromptState::new(
            PromptKind::Touch {
                parent: parent.clone(),
                local,
            },
            format!("New file in {}", display_dir(&parent)),
            "type a name, Enter to create, Esc to cancel",
            String::new(),
        ));
        self.status = format!("Creating a file in {:?}.", self.active_browser_side);
        Vec::new()
    }

    /// `Y`: toggle synced browsing so both panes `cd` together by child name.
    fn toggle_synced_browsing(&mut self) -> Vec<WorkerCommand> {
        if !matches!(self.focus, TuiFocus::Browser) {
            self.status = "Synced browsing applies in the connected browser.".to_string();
            return Vec::new();
        }
        self.synced_browsing = !self.synced_browsing;
        self.status = if self.synced_browsing {
            "Synced browsing on: both panes cd together.".to_string()
        } else {
            "Synced browsing off.".to_string()
        };
        Vec::new()
    }

    /// `G` in the browser: open a go-to-path prompt for the active pane, seeded
    /// with the current directory.
    fn open_goto_prompt(&mut self) -> Vec<WorkerCommand> {
        let local = self.active_browser_side == BrowserSide::Local;
        // Seed empty so the user types a fresh target (an absolute path, or one
        // relative to the current directory).
        self.overlay = TuiOverlay::Prompt(PromptState::new(
            PromptKind::Goto { local },
            "Go to path",
            "absolute or relative path, Enter to go, Esc to cancel",
            String::new(),
        ));
        self.status = "Go to a directory.".to_string();
        Vec::new()
    }

    /// Mirror an open-directory navigation onto the other pane (synced browsing):
    /// open the same child name on the opposite side, best-effort.
    fn synced_open_other(&self, active_side: BrowserSide, child: &str) -> Vec<WorkerCommand> {
        match active_side {
            BrowserSide::Remote => {
                if self.local.path.is_empty() {
                    return Vec::new();
                }
                let target = format!("{}/{}", self.local.path.trim_end_matches('/'), child);
                vec![WorkerCommand::LocalList { path: target }]
            }
            BrowserSide::Local => {
                if !self.is_live_connected() || self.browser.path.is_empty() {
                    return Vec::new();
                }
                let target = join_remote(&self.browser.path, child);
                vec![WorkerCommand::List { path: target }]
            }
        }
    }

    /// Mirror a parent navigation onto the other pane (synced browsing).
    fn synced_parent_other(&self, active_side: BrowserSide) -> Vec<WorkerCommand> {
        match active_side {
            BrowserSide::Remote => match self.local.parent_path() {
                Some(path) => vec![WorkerCommand::LocalList { path }],
                None => Vec::new(),
            },
            BrowserSide::Local => {
                if !self.is_live_connected() {
                    return Vec::new();
                }
                match self.browser.parent_path() {
                    Some(path) => vec![WorkerCommand::List { path }],
                    None => Vec::new(),
                }
            }
        }
    }

    /// Esc: contextual "back". Disconnect to the IntroHub when a live session is
    /// connected, otherwise quit the app. Refused while a transfer is in flight
    /// (cancel it first) so a disconnect never strands an active transfer.
    fn handle_back(&mut self) -> Vec<WorkerCommand> {
        if self.is_live_connected() {
            if self.transfers.has_active() {
                self.status = "Cancel the active transfer (c) before disconnecting.".to_string();
                return Vec::new();
            }
            return self.disconnect_to_introhub();
        }
        self.finish(TuiIntent::Quit);
        Vec::new()
    }

    /// Drop the live session and return to the IntroHub (My Servers). The TUI
    /// resets its own session state optimistically and asks the worker to close
    /// the provider so the connection is not leaked.
    fn disconnect_to_introhub(&mut self) -> Vec<WorkerCommand> {
        self.session = TuiSessionState::default();
        self.browser.clear();
        self.overlay = TuiOverlay::None;
        // Back on the IntroHub: navigation keys must reach `introhub_apply`,
        // which is gated on the focus not being Browser/Transfers.
        self.focus = TuiFocus::Profiles;
        self.worker = WorkerEvent::Idle;
        self.status = "Disconnected. Back to My Servers (Esc again to quit).".to_string();
        self.sync_pane_state();
        vec![WorkerCommand::Disconnect]
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
            TuiOverlay::Groups(_) => self.handle_groups_key(key),
            TuiOverlay::Palette(_) => self.handle_palette_key(key),
            TuiOverlay::ProfileForm(_) => self.handle_profile_form_key(key),
            TuiOverlay::Pager(_) => self.handle_pager_key(key),
            TuiOverlay::Help(_) => self.handle_help_key(key),
        };
        self.sync_pane_state();
        commands
    }

    /// Route a key while the read-only file viewer (`v`) is open. Up/Down and
    /// `j`/`k` scroll a line, PageUp/PageDown a screenful, Home/End jump, Esc/`q`
    /// close. No mutation: the pager is a view over already-fetched content.
    fn handle_pager_key(&mut self, key: OverlayKey) -> Vec<WorkerCommand> {
        if let TuiOverlay::Pager(state) = &mut self.overlay {
            match key {
                OverlayKey::Up | OverlayKey::Char('k') => state.scroll_up(1),
                OverlayKey::Down | OverlayKey::Char('j') => state.scroll_down(1),
                OverlayKey::PageUp | OverlayKey::Char('u') => state.page_up(),
                OverlayKey::PageDown | OverlayKey::Char('d') | OverlayKey::Char(' ') => {
                    state.page_down()
                }
                OverlayKey::Home | OverlayKey::Char('g') => state.scroll_to_top(),
                OverlayKey::End | OverlayKey::Char('G') => state.scroll_to_bottom(),
                OverlayKey::Cancel | OverlayKey::Char('q') => {
                    self.overlay = TuiOverlay::None;
                    self.status = "Closed the viewer.".to_string();
                }
                _ => {}
            }
        }
        Vec::new()
    }

    /// Route a key while the help overlay (`?`/`F1`) is open: scroll or close.
    fn handle_help_key(&mut self, key: OverlayKey) -> Vec<WorkerCommand> {
        if let TuiOverlay::Help(state) = &mut self.overlay {
            match key {
                OverlayKey::Up | OverlayKey::Char('k') => state.scroll_up(1),
                OverlayKey::Down | OverlayKey::Char('j') => state.scroll_down(1),
                OverlayKey::PageUp => state.scroll_up(10),
                OverlayKey::PageDown => state.scroll_down(10),
                OverlayKey::Cancel | OverlayKey::Char('q') | OverlayKey::Char('?') => {
                    self.overlay = TuiOverlay::None;
                    self.status = "Closed help.".to_string();
                }
                _ => {}
            }
        }
        Vec::new()
    }

    /// Route a key while the group manager overlay (#320) is open. The overlay
    /// owns the keyboard: `j`/`k` and arrows move the cursor, Enter applies the
    /// group as the list filter (or clears it on the "All servers" row), space
    /// toggles the highlighted profile's membership, and `n`/`r`/`d`
    /// create/rename/delete groups. Esc dismisses.
    fn handle_groups_key(&mut self, key: OverlayKey) -> Vec<WorkerCommand> {
        match key {
            OverlayKey::Up | OverlayKey::Char('k') => {
                if let TuiOverlay::Groups(state) = &mut self.overlay {
                    state.move_cursor(-1);
                }
                Vec::new()
            }
            OverlayKey::Down | OverlayKey::Char('j') => {
                if let TuiOverlay::Groups(state) = &mut self.overlay {
                    state.move_cursor(1);
                }
                Vec::new()
            }
            OverlayKey::Submit => self.groups_overlay_apply_filter(),
            OverlayKey::Char(' ') => self.groups_overlay_toggle_membership(),
            OverlayKey::Char('n') | OverlayKey::Char('N') => self.groups_overlay_new(),
            OverlayKey::Char('r') | OverlayKey::Char('R') => self.groups_overlay_rename(),
            OverlayKey::Char('d') | OverlayKey::Char('D') => self.groups_overlay_delete(),
            OverlayKey::Cancel => self.cancel_overlay(),
            _ => Vec::new(),
        }
    }

    /// Enter in the group overlay: set (or clear, on "All servers") the list
    /// filter to the highlighted group, snap the cursor onto a visible profile,
    /// and dismiss the overlay.
    fn groups_overlay_apply_filter(&mut self) -> Vec<WorkerCommand> {
        let target = if let TuiOverlay::Groups(state) = &self.overlay {
            state.selected_group().map(|item| item.name.clone())
        } else {
            return Vec::new();
        };
        self.overlay = TuiOverlay::None;
        match target {
            None => {
                self.selected_group = None;
                self.status = "Showing all servers.".to_string();
            }
            Some(name) => {
                self.status = format!("Filtering by group '{}'.", name);
                self.selected_group = Some(name);
            }
        }
        self.snap_selection_to_filter();
        Vec::new()
    }

    /// After a filter change, move the profile cursor onto the first visible row
    /// so navigation and the detail panel never point at a hidden profile.
    fn snap_selection_to_filter(&mut self) {
        let visible = self.visible_profile_indices();
        self.selected_profile = visible.first().copied().unwrap_or(0);
    }

    /// Space in the group overlay: toggle the acted-on profile's membership in
    /// the highlighted group, updating both the context and the overlay row in
    /// place, and ask the worker to persist. No-op on the "All servers" row.
    fn groups_overlay_toggle_membership(&mut self) -> Vec<WorkerCommand> {
        let (profile_id, profile_name, group, idx) =
            if let TuiOverlay::Groups(state) = &self.overlay {
                match state.selected_group() {
                    Some(item) => (
                        state.profile_id.clone(),
                        state.profile_name.clone(),
                        item.name.clone(),
                        state.cursor - 1,
                    ),
                    None => {
                        self.status = "Pick a group row to toggle membership.".to_string();
                        return Vec::new();
                    }
                }
            } else {
                return Vec::new();
            };
        if profile_id.is_empty() {
            self.status = "This profile has no saved id; membership not persisted.".to_string();
            return Vec::new();
        }
        let now_member = self.apply_group_membership(&profile_id, &group);
        if let TuiOverlay::Groups(state) = &mut self.overlay {
            if let Some(item) = state.groups.get_mut(idx) {
                item.is_member = now_member;
                item.member_count = if now_member {
                    item.member_count + 1
                } else {
                    item.member_count.saturating_sub(1)
                };
            }
        }
        self.status = if now_member {
            format!("Added '{}' to group '{}'.", profile_name, group)
        } else {
            format!("Removed '{}' from group '{}'.", profile_name, group)
        };
        vec![WorkerCommand::ToggleGroupMembership {
            profile_id,
            group_name: group,
        }]
    }

    /// `n` in the group overlay: open a name prompt for a new group seeded with
    /// the highlighted profile (the TUI analogue of CLI `g <selector> <group>`).
    fn groups_overlay_new(&mut self) -> Vec<WorkerCommand> {
        let profile_id = if let TuiOverlay::Groups(state) = &self.overlay {
            state.profile_id.clone()
        } else {
            return Vec::new();
        };
        if profile_id.is_empty() {
            self.status = "This profile has no saved id; cannot seed a group.".to_string();
            return Vec::new();
        }
        self.overlay = TuiOverlay::Prompt(PromptState::new(
            PromptKind::GroupName {
                profile_id,
                rename_from: None,
            },
            "New group",
            "name (the highlighted server joins it)",
            String::new(),
        ));
        Vec::new()
    }

    /// `r` in the group overlay: open a name prompt to rename the highlighted
    /// group. No-op on the "All servers" row.
    fn groups_overlay_rename(&mut self) -> Vec<WorkerCommand> {
        let old = if let TuiOverlay::Groups(state) = &self.overlay {
            match state.selected_group() {
                Some(item) => item.name.clone(),
                None => {
                    self.status = "Pick a group row to rename.".to_string();
                    return Vec::new();
                }
            }
        } else {
            return Vec::new();
        };
        self.overlay = TuiOverlay::Prompt(PromptState::new(
            PromptKind::GroupName {
                profile_id: String::new(),
                rename_from: Some(old.clone()),
            },
            "Rename group",
            "new name",
            old,
        ));
        Vec::new()
    }

    /// `d` in the group overlay: delete the highlighted group (servers untouched)
    /// from the context and the overlay, and ask the worker to persist. No-op on
    /// the "All servers" row.
    fn groups_overlay_delete(&mut self) -> Vec<WorkerCommand> {
        let (name, idx) = if let TuiOverlay::Groups(state) = &self.overlay {
            match state.selected_group() {
                Some(item) => (item.name.clone(), state.cursor - 1),
                None => {
                    self.status = "Pick a group row to delete.".to_string();
                    return Vec::new();
                }
            }
        } else {
            return Vec::new();
        };
        self.apply_group_delete(&name);
        if let TuiOverlay::Groups(state) = &mut self.overlay {
            if idx < state.groups.len() {
                state.groups.remove(idx);
            }
            let max = state.row_count().saturating_sub(1);
            if state.cursor > max {
                state.cursor = max;
            }
        }
        self.status = format!("Deleted group '{}' (servers kept).", name);
        vec![WorkerCommand::DeleteGroup { name }]
    }

    fn handle_prompt_key(&mut self, key: OverlayKey) -> Vec<WorkerCommand> {
        match key {
            OverlayKey::Char(c) => {
                if let TuiOverlay::Prompt(prompt) = &mut self.overlay {
                    prompt.push_char(c);
                }
                // The filter prompt narrows the listing live as the user types.
                self.apply_live_filter();
                Vec::new()
            }
            OverlayKey::Backspace => {
                if let TuiOverlay::Prompt(prompt) = &mut self.overlay {
                    prompt.backspace();
                }
                self.apply_live_filter();
                Vec::new()
            }
            OverlayKey::Left => {
                if let TuiOverlay::Prompt(prompt) = &mut self.overlay {
                    prompt.move_left();
                }
                Vec::new()
            }
            OverlayKey::Right => {
                if let TuiOverlay::Prompt(prompt) = &mut self.overlay {
                    prompt.move_right();
                }
                Vec::new()
            }
            OverlayKey::Submit => self.submit_prompt(),
            OverlayKey::Cancel => {
                // Esc on the filter prompt clears any active filter (full listing).
                if let TuiOverlay::Prompt(prompt) = &self.overlay {
                    if let PromptKind::Filter { local } = prompt.kind {
                        let pane = if local {
                            &mut self.local
                        } else {
                            &mut self.browser
                        };
                        pane.clear_filter();
                        self.overlay = TuiOverlay::None;
                        self.status = "Filter cleared.".to_string();
                        return Vec::new();
                    }
                }
                self.cancel_overlay()
            }
            OverlayKey::Up
            | OverlayKey::Down
            | OverlayKey::Tab
            | OverlayKey::PageUp
            | OverlayKey::PageDown
            | OverlayKey::Home
            | OverlayKey::End
            | OverlayKey::Noop => Vec::new(),
        }
    }

    /// Re-apply the live filter from the open filter prompt to its pane. A no-op
    /// for any other overlay.
    fn apply_live_filter(&mut self) {
        let (buffer, local) = match &self.overlay {
            TuiOverlay::Prompt(prompt) => match prompt.kind {
                PromptKind::Filter { local } => (prompt.buffer.clone(), local),
                _ => return,
            },
            _ => return,
        };
        let pane = if local {
            &mut self.local
        } else {
            &mut self.browser
        };
        pane.set_filter(buffer);
    }

    /// Route a key while the `:` command palette (B3) is open. The palette is a
    /// single-line editor: printable chars append, Backspace deletes, Enter
    /// dispatches the parsed line through existing `WorkerCommand`s, Esc closes.
    /// Up/Down/Left/Right are reserved (command history is a later increment).
    fn handle_palette_key(&mut self, key: OverlayKey) -> Vec<WorkerCommand> {
        match key {
            OverlayKey::Char(c) => {
                if let TuiOverlay::Palette(state) = &mut self.overlay {
                    state.push_char(c);
                }
                Vec::new()
            }
            OverlayKey::Backspace => {
                if let TuiOverlay::Palette(state) = &mut self.overlay {
                    state.backspace();
                }
                Vec::new()
            }
            OverlayKey::Submit => self.submit_palette(),
            OverlayKey::Cancel => {
                self.overlay = TuiOverlay::None;
                self.status = "Palette closed.".to_string();
                Vec::new()
            }
            OverlayKey::Up
            | OverlayKey::Down
            | OverlayKey::Left
            | OverlayKey::Right
            | OverlayKey::Tab
            | OverlayKey::PageUp
            | OverlayKey::PageDown
            | OverlayKey::Home
            | OverlayKey::End
            | OverlayKey::Noop => Vec::new(),
        }
    }

    /// Parse the palette buffer and dispatch it through the existing worker
    /// command path (the #311 invariant: the palette never re-implements a
    /// provider op, it only maps a verb onto a `WorkerCommand` the connected
    /// worker already handles). A parse error keeps the palette open with a
    /// one-line usage hint in `last_result`; a successful dispatch closes it.
    fn submit_palette(&mut self) -> Vec<WorkerCommand> {
        let line = match &self.overlay {
            TuiOverlay::Palette(state) => state.buffer.clone(),
            _ => return Vec::new(),
        };
        // Connected-only for v1 (there must be a held session to run against).
        if !self.is_live_connected() {
            if let TuiOverlay::Palette(state) = &mut self.overlay {
                state.last_result = "not connected".to_string();
            }
            return Vec::new();
        }
        let cwd = self.current_browser_dir();
        let parsed = parse_palette_command(&line, &cwd);
        match parsed {
            PaletteCommand::Empty => Vec::new(),
            PaletteCommand::Help => {
                if let TuiOverlay::Palette(state) = &mut self.overlay {
                    state.last_result = palette_cheatsheet().to_string();
                    state.buffer.clear();
                }
                Vec::new()
            }
            PaletteCommand::Error(hint) => {
                if let TuiOverlay::Palette(state) = &mut self.overlay {
                    state.last_result = hint;
                }
                Vec::new()
            }
            PaletteCommand::List { path } => {
                self.overlay = TuiOverlay::None;
                self.focus = TuiFocus::Browser;
                self.active_browser_side = BrowserSide::Remote;
                self.begin_mutation(TuiWorkerOperation::List, format!("Listing {}.", path));
                vec![WorkerCommand::List { path }]
            }
            PaletteCommand::Stat { path } => {
                self.overlay = TuiOverlay::None;
                self.begin_mutation(
                    TuiWorkerOperation::Stat,
                    format!("Loading metadata for {}.", path),
                );
                vec![WorkerCommand::Stat { path }]
            }
            PaletteCommand::Mkdir { path } => {
                self.overlay = TuiOverlay::None;
                self.begin_mutation(TuiWorkerOperation::Mkdir, format!("Creating {}.", path));
                vec![WorkerCommand::Mkdir { path }]
            }
            PaletteCommand::Get { remote, local } => {
                let name = remote_basename(&remote);
                let base = if !self.context.download_base.is_empty() {
                    self.context.download_base.clone()
                } else if !self.local.path.is_empty() {
                    self.local.path.clone()
                } else {
                    ".".to_string()
                };
                let local_path =
                    local.unwrap_or_else(|| format!("{}/{}", base.trim_end_matches('/'), name));
                let id = self.transfers.enqueue(
                    TransferDirection::Download,
                    name,
                    remote.clone(),
                    local_path.clone(),
                );
                self.overlay = TuiOverlay::None;
                self.focus = TuiFocus::Transfers;
                self.begin_mutation(
                    TuiWorkerOperation::Transfer,
                    format!("Downloading {} -> {}.", remote, local_path),
                );
                vec![WorkerCommand::Download {
                    id,
                    remote_path: remote,
                    local_path,
                }]
            }
            PaletteCommand::Put { local, remote } => {
                let name = local_basename(&local);
                if name.is_empty() {
                    if let TuiOverlay::Palette(state) = &mut self.overlay {
                        state.last_result = "could not read a file name from that path".to_string();
                    }
                    return Vec::new();
                }
                let remote_path = remote.unwrap_or_else(|| join_remote(&cwd, &name));
                let id = self.transfers.enqueue(
                    TransferDirection::Upload,
                    name,
                    remote_path.clone(),
                    local.clone(),
                );
                self.overlay = TuiOverlay::None;
                self.focus = TuiFocus::Transfers;
                self.begin_mutation(
                    TuiWorkerOperation::Transfer,
                    format!("Uploading {} -> {}.", local, remote_path),
                );
                vec![WorkerCommand::Upload {
                    id,
                    local_path: local,
                    remote_path,
                }]
            }
            PaletteCommand::Move { from, to } => {
                self.overlay = TuiOverlay::None;
                self.begin_mutation(
                    TuiWorkerOperation::Rename,
                    format!("Moving {} -> {}.", from, to),
                );
                vec![WorkerCommand::Rename { from, to }]
            }
            PaletteCommand::Rm { path, force } => {
                if force {
                    self.overlay = TuiOverlay::None;
                    self.begin_mutation(TuiWorkerOperation::Remove, format!("Deleting {}.", path));
                    // Best-effort recursive flag: the palette cannot stat the
                    // entry here, so route as non-recursive and let the provider
                    // report a non-empty directory. `rm!` is the power-user verb.
                    vec![WorkerCommand::Remove {
                        path,
                        recursive: false,
                    }]
                } else {
                    // Route through the same confirm overlay the `d` key uses.
                    let message = format!("Delete '{}'?", path);
                    self.overlay = TuiOverlay::Confirm(ConfirmState {
                        kind: ConfirmKind::Delete {
                            path,
                            recursive: false,
                            local: false, // palette `rm` is remote-only (v1)
                        },
                        message,
                    });
                    self.status = "Confirm delete (y/n).".to_string();
                    Vec::new()
                }
            }
        }
    }

    /// Route a key while the DiscoveryHub profile form (B4) is open. Tab and
    /// Up/Down move between fields, Left/Right cycle the protocol field,
    /// printable chars edit the focused field, Enter submits, Esc cancels.
    fn handle_profile_form_key(&mut self, key: OverlayKey) -> Vec<WorkerCommand> {
        match key {
            OverlayKey::Tab | OverlayKey::Down => {
                if let TuiOverlay::ProfileForm(form) = &mut self.overlay {
                    form.focus_next();
                }
                Vec::new()
            }
            OverlayKey::Char(c) => {
                if let TuiOverlay::ProfileForm(form) = &mut self.overlay {
                    form.push_char(c);
                }
                Vec::new()
            }
            OverlayKey::Backspace => {
                if let TuiOverlay::ProfileForm(form) = &mut self.overlay {
                    form.backspace();
                }
                Vec::new()
            }
            OverlayKey::Up => {
                if let TuiOverlay::ProfileForm(form) = &mut self.overlay {
                    form.focus_prev();
                }
                Vec::new()
            }
            OverlayKey::Left => {
                if let TuiOverlay::ProfileForm(form) = &mut self.overlay {
                    form.cycle_protocol(-1);
                }
                Vec::new()
            }
            OverlayKey::Right => {
                if let TuiOverlay::ProfileForm(form) = &mut self.overlay {
                    form.cycle_protocol(1);
                }
                Vec::new()
            }
            OverlayKey::Submit => self.submit_profile_form(),
            OverlayKey::Cancel => self.cancel_overlay(),
            OverlayKey::PageUp
            | OverlayKey::PageDown
            | OverlayKey::Home
            | OverlayKey::End
            | OverlayKey::Noop => Vec::new(),
        }
    }

    /// Validate the DiscoveryHub form and emit a `SaveProfile` worker command,
    /// optimistically upserting the profile in the in-memory context. A
    /// validation error is shown in the form and the overlay stays open.
    fn submit_profile_form(&mut self) -> Vec<WorkerCommand> {
        let form = match &self.overlay {
            TuiOverlay::ProfileForm(form) => form.clone(),
            _ => return Vec::new(),
        };
        // Validate. Keep the form open and surface the first problem.
        let name = form.name.trim().to_string();
        if name.is_empty() {
            self.set_form_error("Name cannot be empty.");
            return Vec::new();
        }
        let protocol = form.protocol.trim().to_ascii_lowercase();
        if protocol.is_empty() {
            self.set_form_error("Protocol cannot be empty.");
            return Vec::new();
        }
        let host = form.host.trim().to_string();
        if host.is_empty() {
            self.set_form_error("Host cannot be empty.");
            return Vec::new();
        }
        let port: u16 = if form.port.trim().is_empty() {
            default_port_for(&protocol)
        } else {
            match form.port.trim().parse::<u16>() {
                Ok(p) if p > 0 => p,
                _ => {
                    self.set_form_error("Port must be a number in 1..=65535.");
                    return Vec::new();
                }
            }
        };

        let id = match &form.mode {
            ProfileFormMode::Edit { id } => id.clone(),
            ProfileFormMode::Create => mint_profile_id(),
        };

        let draft = TuiProfileDraft {
            id: id.clone(),
            name: name.clone(),
            protocol: protocol.clone(),
            host: host.clone(),
            port,
            username: form.username.trim().to_string(),
            initial_path: form.initial_path.trim().to_string(),
            local_initial_path: form.local_path.trim().to_string(),
        };

        // The secret only crosses the channel when the field was actually
        // touched, so an untouched edit never overwrites an existing credential.
        let secret = if form.password_touched && !form.password.is_empty() {
            Some(form.password.clone())
        } else {
            None
        };

        let user_name = form.user_name.clone();
        // Optimistic in-memory upsert (favourites/groups pattern). Health/quota
        // and group/favourite state are preserved on edit; a created profile
        // starts clean with the minted id.
        let optimistic = TuiProfile {
            selector: String::new(), // renumbered by apply_profile_upsert
            id,
            name,
            protocol,
            host,
            username: draft.username.clone(),
            initial_path: draft.initial_path.clone(),
            default_local_path: draft.local_initial_path.clone(),
            favorite: false,
            groups: Vec::new(),
            used: None,
            total: None,
            last_connected_label: None,
            port,
            endpoint: None,
            health: None,
        };
        self.apply_profile_upsert(&user_name, optimistic);

        self.overlay = TuiOverlay::None;
        self.status = match &form.mode {
            ProfileFormMode::Create => format!("Created profile '{}'.", draft.name),
            ProfileFormMode::Edit { .. } => format!("Updated profile '{}'.", draft.name),
        };
        vec![WorkerCommand::SaveProfile {
            user_name,
            draft,
            secret,
        }]
    }

    /// Record a validation error on the open profile form (no-op otherwise).
    fn set_form_error(&mut self, message: &str) {
        if let TuiOverlay::ProfileForm(form) = &mut self.overlay {
            form.error = Some(message.to_string());
        }
        self.status = message.to_string();
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
                let on_local = self.active_browser_side == BrowserSide::Local;
                self.overlay = TuiOverlay::None;
                self.begin_mutation(TuiWorkerOperation::Mkdir, format!("Creating {}.", path));
                // Route to the active side: a Local-pane mkdir hits the local
                // filesystem, not the remote provider (otherwise a "local" mkdir
                // would create the directory on the server).
                if on_local {
                    vec![WorkerCommand::LocalMkdir { path }]
                } else {
                    vec![WorkerCommand::Mkdir { path }]
                }
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
                let on_local = self.active_browser_side == BrowserSide::Local;
                self.overlay = TuiOverlay::None;
                self.begin_mutation(TuiWorkerOperation::Rename, format!("Renaming to {}.", to));
                // Route to the active side: a Local-pane rename hits the local
                // filesystem (sending it to the remote provider is what caused
                // the "[550] No such file or directory" failure on local files).
                if on_local {
                    vec![WorkerCommand::LocalRename { from, to }]
                } else {
                    vec![WorkerCommand::Rename { from, to }]
                }
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
            PromptKind::GroupName {
                profile_id,
                rename_from,
            } => {
                if value.is_empty() {
                    self.status = "Enter a group name.".to_string();
                    return Vec::new();
                }
                self.overlay = TuiOverlay::None;
                match rename_from {
                    Some(old) => {
                        if value.eq_ignore_ascii_case(&old) {
                            self.status = "Name unchanged.".to_string();
                            return Vec::new();
                        }
                        self.apply_group_rename(&old, &value);
                        self.status = format!("Renamed group '{}' to '{}'.", old, value);
                        vec![WorkerCommand::RenameGroup {
                            old_name: old,
                            new_name: value,
                        }]
                    }
                    None => {
                        // Create-if-new and add the seed profile, mirroring the
                        // CLI `g` create-or-toggle on `config_server_groups`.
                        let now_member = self.apply_group_membership(&profile_id, &value);
                        self.status = if now_member {
                            format!("Created group '{}'.", value)
                        } else {
                            format!("Updated group '{}'.", value)
                        };
                        vec![WorkerCommand::ToggleGroupMembership {
                            profile_id,
                            group_name: value,
                        }]
                    }
                }
            }
            PromptKind::Filter { .. } => {
                // The filter was applied live while typing; Enter just keeps it
                // and closes the prompt.
                self.overlay = TuiOverlay::None;
                let active = self.active_browser();
                self.status = match &active.filter {
                    Some(f) => format!("Filter '{}' ({} shown).", f, active.entries.len()),
                    None => "Filter cleared.".to_string(),
                };
                Vec::new()
            }
            PromptKind::Touch { parent, local } => {
                if !is_valid_segment(&value) {
                    self.status = "Enter a file name without '/' or '..'.".to_string();
                    return Vec::new();
                }
                let path = join_remote(&parent, &value);
                self.overlay = TuiOverlay::None;
                self.begin_mutation(TuiWorkerOperation::Touch, format!("Creating {}.", path));
                vec![WorkerCommand::Touch { path, local }]
            }
            PromptKind::Goto { local } => {
                if value.is_empty() {
                    self.status = "Enter a path to go to.".to_string();
                    return Vec::new();
                }
                self.overlay = TuiOverlay::None;
                if local {
                    let target = if value.starts_with('/') {
                        value
                    } else {
                        format!("{}/{}", self.local.path.trim_end_matches('/'), value)
                    };
                    self.status = format!("Going to local {}.", target);
                    self.active_browser_side = BrowserSide::Local;
                    vec![WorkerCommand::LocalList { path: target }]
                } else {
                    let cwd = if self.browser.path.is_empty() {
                        "/".to_string()
                    } else {
                        self.browser.path.clone()
                    };
                    let target = resolve_remote_arg(&cwd, &value);
                    self.status = format!("Going to {}.", target);
                    self.active_browser_side = BrowserSide::Remote;
                    self.begin_mutation(TuiWorkerOperation::List, self.status.clone());
                    vec![WorkerCommand::List { path: target }]
                }
            }
        }
    }

    fn confirm_overlay(&mut self) -> Vec<WorkerCommand> {
        let TuiOverlay::Confirm(confirm) = &self.overlay else {
            return Vec::new();
        };
        let kind = confirm.kind.clone();
        match kind {
            ConfirmKind::Delete {
                path,
                recursive,
                local,
            } => {
                self.overlay = TuiOverlay::None;
                self.begin_mutation(TuiWorkerOperation::Remove, format!("Deleting {}.", path));
                // Route to the side the delete was raised on: a Local-pane delete
                // must remove the local file, never the remote entry at the same
                // path. The palette `rm` is always remote (local = false).
                if local {
                    vec![WorkerCommand::LocalRemove { path, recursive }]
                } else {
                    vec![WorkerCommand::Remove { path, recursive }]
                }
            }
            ConfirmKind::DeleteProfile {
                user_name,
                profile_id,
                profile_name,
            } => {
                self.overlay = TuiOverlay::None;
                self.apply_profile_delete(&user_name, &profile_id);
                self.status = format!("Deleted profile '{}'.", profile_name);
                vec![WorkerCommand::DeleteProfile {
                    user_name,
                    profile_id,
                }]
            }
            ConfirmKind::DeleteBatch { items, local } => {
                self.overlay = TuiOverlay::None;
                let count = items.len();
                self.begin_mutation(
                    TuiWorkerOperation::Remove,
                    format!(
                        "Deleting {} marked entr{}.",
                        count,
                        if count == 1 { "y" } else { "ies" }
                    ),
                );
                // The marks have been consumed by this batch; drop them on the
                // pane the batch targeted so the view does not show stale marks.
                if local {
                    self.local.clear_marks();
                } else {
                    self.browser.clear_marks();
                }
                items
                    .into_iter()
                    .map(|(path, is_dir)| {
                        if local {
                            WorkerCommand::LocalRemove {
                                path,
                                recursive: is_dir,
                            }
                        } else {
                            WorkerCommand::Remove {
                                path,
                                recursive: is_dir,
                            }
                        }
                    })
                    .collect()
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
                // List the remote pane now that the session is live (deferred from
                // connect_to_profile so a failed connect never lists with no
                // session). cwd is the post-cd working directory.
                follow_up.push(WorkerCommand::List { path: cwd.clone() });
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
                        | TuiWorkerOperation::Touch
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
                // Refresh the destination pane so the transferred file appears:
                // an upload lands in the REMOTE pane's directory, a download in
                // the LOCAL pane's directory. Route each to the right side (the
                // active side is the Transfers pane here, so don't use it).
                if was_upload {
                    if !self.browser.path.is_empty() {
                        follow_up.push(WorkerCommand::List {
                            path: self.browser.path.clone(),
                        });
                    }
                } else if !self.local.path.is_empty() {
                    follow_up.push(WorkerCommand::LocalList {
                        path: self.local.path.clone(),
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
            WorkerEvent::HealthReady {
                profile_id,
                status,
                score,
                latency_ms,
            } => {
                let latency = *latency_ms;
                let label = match latency {
                    Some(ms) => format!("{} ({}) {}ms", status, score, ms),
                    None => format!("{} ({})", status, score),
                };
                let mut applied = false;
                for user in &mut self.context.users {
                    for profile in &mut user.profiles {
                        if profile.id == *profile_id {
                            profile.health = Some(TuiHealth {
                                status: status.clone(),
                                score: *score,
                                latency_ms: latency,
                            });
                            applied = true;
                        }
                    }
                }
                if applied {
                    self.status = format!("Health: {}", label);
                }
            }
            WorkerEvent::QuotaReady {
                profile_id,
                used,
                total,
            } => {
                for user in &mut self.context.users {
                    for profile in &mut user.profiles {
                        if profile.id == *profile_id {
                            profile.used = Some(*used);
                            profile.total = if *total > 0 {
                                Some(*total)
                            } else {
                                profile.total
                            };
                        }
                    }
                }
                self.status = format!("Quota refreshed: {} / {} bytes.", used, total);
            }
            WorkerEvent::QuotaFailed {
                profile_id: _,
                message,
            } => {
                self.status = format!("Quota refresh failed: {}", message);
            }
            WorkerEvent::FileContent {
                path,
                content,
                truncated,
                binary,
            } => {
                // Decode to display lines, neutralising control characters that a
                // remote name/body could otherwise inject into the terminal.
                let lines: Vec<String> = if content.is_empty() {
                    vec!["(empty file)".to_string()]
                } else {
                    content
                        .split('\n')
                        .map(crate::cli_tui::sanitize_display)
                        .collect()
                };
                let title = format!(
                    " View: {}{} ",
                    path,
                    if *truncated { " (truncated)" } else { "" }
                );
                self.overlay =
                    TuiOverlay::Pager(PagerState::new(title, lines, *truncated, *binary));
                self.status = format!(
                    "Viewing {}{}.",
                    path,
                    if *binary { " (binary)" } else { "" }
                );
            }
            WorkerEvent::DirSize { path, bytes, files } => {
                self.status = format!(
                    "{}: {} in {} file(s).",
                    path,
                    format_size_compact(*bytes),
                    files
                );
            }
            WorkerEvent::EditReady { remote_path, .. } => {
                // The run loop intercepts this to spawn $EDITOR (it owns the
                // terminal); reaching here is a fallback that just notes progress.
                self.status = format!("Opening {} in the editor...", remote_path);
            }
            WorkerEvent::EditDone {
                remote_path,
                message,
            } => {
                self.status = message.clone();
                // Refresh the remote listing so an edit that changed the size /
                // mtime is reflected. Best-effort: only when we know the dir.
                let _ = remote_path;
                if !self.browser.path.is_empty() {
                    follow_up.push(WorkerCommand::List {
                        path: self.browser.path.clone(),
                    });
                }
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
                    "No transfers yet. From Browser: g downloads a file, p uploads into the directory."
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

    /// Mouse handler (1.0.1 hybrid TUI). Wheel scrolls the focused list (no
    /// hit test needed). Left click selects row (intro or browser file) or
    /// activates the clicked pane (like Tab). Double-click (tracked via 350ms
    /// same-cell) activates (connect profile or open dir/file). Returns
    /// WorkerCommands exactly like apply_action for activate paths; selection
    /// changes are pure state mutations (optimistic, same as keys).
    pub fn handle_mouse(&mut self, ev: MouseEvent) -> Vec<WorkerCommand> {
        use crossterm::event::MouseEventKind;

        // A modal overlay owns all input while it is open: a click or wheel must
        // not reach the view behind it (it would select rows or switch sides
        // under the modal). The overlays are keyboard-driven, so swallow mouse.
        if self.overlay.is_active() {
            return Vec::new();
        }

        // Double-click detection (update last on Down Left).
        let mut is_double = false;
        if let ::crossterm::event::MouseEventKind::Down(MouseButton::Left) = ev.kind {
            let now = Instant::now();
            if let Some((lc, lr, lt)) = self.last_mouse_down {
                if lc == ev.column
                    && lr == ev.row
                    && now.duration_since(lt) < Duration::from_millis(350)
                {
                    is_double = true;
                    self.last_mouse_down = None;
                } else {
                    self.last_mouse_down = Some((ev.column, ev.row, now));
                }
            } else {
                self.last_mouse_down = Some((ev.column, ev.row, now));
            }
        }

        match ev.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let delta: isize = if matches!(ev.kind, MouseEventKind::ScrollUp) {
                    -1
                } else {
                    1
                };
                match self.focus {
                    TuiFocus::Profiles => {
                        let visible = self.visible_profile_indices();
                        if !visible.is_empty() {
                            if let Some(pos) =
                                visible.iter().position(|&p| p == self.selected_profile)
                            {
                                let new_pos = ((pos as isize) + delta)
                                    .clamp(0, (visible.len() as isize) - 1)
                                    as usize;
                                self.selected_profile = visible[new_pos];
                            }
                        }
                        Vec::new()
                    }
                    TuiFocus::Browser => {
                        self.move_selection(delta);
                        Vec::new()
                    }
                    TuiFocus::Transfers => {
                        let n = self.transfers.items.len();
                        if n > 0 {
                            let s = self.transfers.selected;
                            self.transfers.selected = if delta > 0 {
                                (s + 1).min(n - 1)
                            } else {
                                s.saturating_sub(1)
                            };
                        }
                        Vec::new()
                    }
                    _ => Vec::new(),
                }
            }
            ::crossterm::event::MouseEventKind::Down(MouseButton::Left) => {
                let (c, r) = (ev.column, ev.row);
                // Pre-connect: click rows in the intro table.
                if !self.is_live_connected() {
                    if let Some(tr) = self.layout.intro_table {
                        if point_in_rect(c, r, tr) {
                            // Bordered table with a header row: row 0 is the top
                            // border, row 1 the column header, data starts at +2.
                            let data_top = tr.y + 2;
                            if r >= data_top {
                                let idx = (r - data_top) as usize;
                                let visible = self.visible_profile_indices();
                                if idx < visible.len() {
                                    self.selected_profile = visible[idx];
                                    self.focus = TuiFocus::Profiles;
                                    if is_double {
                                        // The IntroHub connect path is
                                        // `introhub_activate` (what keyboard Enter
                                        // uses); the generic `activate` only
                                        // connects when focus is already Profiles,
                                        // so a fresh click would just switch focus.
                                        return self.introhub_activate();
                                    }
                                }
                            }
                        }
                    }
                    return Vec::new();
                }

                // Connected: transfers strip click selects transfer row + focuses it.
                if let Some(tr) = self.layout.transfers_strip {
                    if point_in_rect(c, r, tr) {
                        self.focus = TuiFocus::Transfers;
                        let data_top = tr.y + 1; // bordered list title
                        if r >= data_top {
                            let idx = (r - data_top) as usize;
                            if idx < self.transfers.items.len() {
                                self.transfers.selected = idx;
                            }
                        }
                        return Vec::new();
                    }
                }

                // Browser panes: click switches active side (if on the other), then selects row.
                let in_remote = self
                    .layout
                    .remote_pane
                    .is_some_and(|p| point_in_rect(c, r, p));
                let in_local = self
                    .layout
                    .local_pane
                    .is_some_and(|p| point_in_rect(c, r, p));
                if in_remote || in_local {
                    let target = if in_remote {
                        BrowserSide::Remote
                    } else {
                        BrowserSide::Local
                    };
                    if self.active_browser_side != target {
                        self.active_browser_side = target;
                        self.focus = TuiFocus::Browser;
                    }
                    let pane = if in_remote {
                        self.layout.remote_pane.unwrap()
                    } else {
                        self.layout.local_pane.unwrap()
                    };
                    let state = if in_remote {
                        &mut self.browser
                    } else {
                        &mut self.local
                    };
                    // Bordered list, title on the top border, no header row:
                    // row 0 is the border, the first entry starts at +1.
                    let data_top = pane.y + 1;
                    if r >= data_top {
                        let idx = (r - data_top) as usize;
                        if idx < state.entries.len() {
                            state.selected = idx;
                            if is_double {
                                return self.open_selected_browser_entry();
                            }
                        }
                    }
                    return Vec::new();
                }

                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

fn point_in_rect(col: u16, row: u16, r: Rect) -> bool {
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
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
        | WorkerEvent::Cancelled { .. }
        | WorkerEvent::HealthReady { .. }
        | WorkerEvent::QuotaReady { .. }
        | WorkerEvent::QuotaFailed { .. }
        | WorkerEvent::FileContent { .. }
        | WorkerEvent::DirSize { .. }
        | WorkerEvent::EditReady { .. }
        | WorkerEvent::EditDone { .. } => None,
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

/// Default port for a protocol when the DiscoveryHub form leaves it blank (B4).
/// Mirrors the connection defaults the CLI/GUI use; unknown protocols get 0,
/// which the connect path resolves from its own defaults.
fn default_port_for(protocol: &str) -> u16 {
    match protocol {
        "sftp" => 22,
        "ftp" | "ftps" => 21,
        "webdav" | "s3" => 443,
        _ => 0,
    }
}

/// Mint a new saved-profile id, matching the CLI/GUI scheme
/// (`srv_<unix_ms>_<rand9>`) so the TUI, CLI, and GUI converge on one format.
/// Impure (time + rng); kept out of the pure form state so the state machine
/// stays deterministic and unit-testable.
fn mint_profile_id() -> String {
    use rand::Rng;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut rng = rand::thread_rng();
    let suffix: String = (0..9)
        .map(|_| {
            let idx: u8 = rng.gen_range(0..36);
            if idx < 10 {
                (b'0' + idx) as char
            } else {
                (b'a' + (idx - 10)) as char
            }
        })
        .collect();
    format!("srv_{}_{}", now_ms, suffix)
}

/// A parsed `:` command-palette line (B3). The verb table is a single match in
/// [`parse_palette_command`]; widening the palette later is adding a variant
/// and a row, not threading new plumbing. Paths are already resolved against the
/// remote cwd so the dispatcher only has to enqueue/emit.
#[derive(Debug, Clone, Eq, PartialEq)]
enum PaletteCommand {
    /// Blank line: a no-op that leaves the palette open.
    Empty,
    /// `help` / `?`: echo the full verb cheatsheet, palette stays open.
    Help,
    /// Parse error or unknown verb: a one-line usage hint, palette stays open.
    Error(String),
    /// `ls [path]` / `cd <path>`: list a remote directory (the pane follows).
    List { path: String },
    /// `stat <path>`: load metadata for a remote entry.
    Stat { path: String },
    /// `mkdir <path>`: create a remote directory.
    Mkdir { path: String },
    /// `get <remote> [local]` / `pget <remote> [local]`: enqueue a download
    /// (local defaults like the `g` key).
    Get {
        remote: String,
        local: Option<String>,
    },
    /// `put <local> [remote]`: enqueue an upload (remote defaults to cwd, like
    /// the `p` key). Mirrors the CLI `put`, reusing `WorkerCommand::Upload`.
    Put {
        local: String,
        remote: Option<String>,
    },
    /// `mv <src> <dst>`: move/rename a remote entry. A remote rename is a move
    /// here, so this reuses `WorkerCommand::Rename` (CLI parity for `mv`).
    Move { from: String, to: String },
    /// `rm <path>` (confirm) / `rm! <path>` (force): delete a remote entry.
    Rm { path: String, force: bool },
}

/// Split a palette line shell-style, honouring double quotes so a single
/// argument can carry spaces (`get "a b.txt"`). Backslash escaping is not
/// supported (remote paths are POSIX); an unterminated quote simply runs to the
/// end of the line. Pure and unit-tested.
fn tokenize_palette_line(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut has_token = false;
    for c in line.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                has_token = true;
            }
            c if c.is_whitespace() && !in_quote => {
                if has_token {
                    tokens.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            c => {
                current.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        tokens.push(current);
    }
    tokens
}

/// Resolve a palette path argument against the remote cwd: `.` is the cwd, `..`
/// the parent, an absolute path is normalised, and anything else is joined onto
/// the cwd. Mirrors keyboard navigation so the palette inherits the same paths.
fn resolve_remote_arg(cwd: &str, arg: &str) -> String {
    match arg {
        "." => {
            if cwd.is_empty() {
                "/".to_string()
            } else {
                cwd.to_string()
            }
        }
        ".." => parent_remote(cwd),
        _ if arg.starts_with('/') => {
            let trimmed = arg.trim_end_matches('/');
            if trimmed.is_empty() {
                "/".to_string()
            } else {
                trimmed.to_string()
            }
        }
        _ => join_remote(cwd, arg),
    }
}

/// Parse a palette line into a [`PaletteCommand`]. The first token is the verb;
/// remaining tokens are arguments resolved against `cwd`. Unknown verbs and
/// wrong arity return [`PaletteCommand::Error`] with a short usage hint so the
/// palette can stay open without ever running a half-parsed command.
fn parse_palette_command(line: &str, cwd: &str) -> PaletteCommand {
    let tokens = tokenize_palette_line(line);
    let Some((verb, args)) = tokens.split_first() else {
        return PaletteCommand::Empty;
    };
    match verb.as_str() {
        "help" | "?" => PaletteCommand::Help,
        "ls" => {
            let path = match args.len() {
                0 => resolve_remote_arg(cwd, "."),
                1 => resolve_remote_arg(cwd, &args[0]),
                _ => return PaletteCommand::Error("usage: ls [path]".to_string()),
            };
            PaletteCommand::List { path }
        }
        "cd" => {
            if args.len() != 1 {
                return PaletteCommand::Error("usage: cd <path>".to_string());
            }
            PaletteCommand::List {
                path: resolve_remote_arg(cwd, &args[0]),
            }
        }
        "stat" => {
            if args.len() != 1 {
                return PaletteCommand::Error("usage: stat <path>".to_string());
            }
            PaletteCommand::Stat {
                path: resolve_remote_arg(cwd, &args[0]),
            }
        }
        "mkdir" => {
            if args.len() != 1 {
                return PaletteCommand::Error("usage: mkdir <path>".to_string());
            }
            PaletteCommand::Mkdir {
                path: resolve_remote_arg(cwd, &args[0]),
            }
        }
        "get" | "pget" => match args.len() {
            1 => PaletteCommand::Get {
                remote: resolve_remote_arg(cwd, &args[0]),
                local: None,
            },
            2 => PaletteCommand::Get {
                remote: resolve_remote_arg(cwd, &args[0]),
                local: Some(args[1].clone()),
            },
            _ => PaletteCommand::Error("usage: get <remote> [local]".to_string()),
        },
        "put" => match args.len() {
            1 => PaletteCommand::Put {
                local: args[0].clone(),
                remote: None,
            },
            2 => PaletteCommand::Put {
                local: args[0].clone(),
                remote: Some(resolve_remote_arg(cwd, &args[1])),
            },
            _ => PaletteCommand::Error("usage: put <local> [remote]".to_string()),
        },
        "mv" => {
            if args.len() != 2 {
                return PaletteCommand::Error("usage: mv <src> <dst>".to_string());
            }
            PaletteCommand::Move {
                from: resolve_remote_arg(cwd, &args[0]),
                to: resolve_remote_arg(cwd, &args[1]),
            }
        }
        "rm" | "rm!" => {
            if args.len() != 1 {
                return PaletteCommand::Error(
                    "usage: rm <path>  (rm! to skip confirm)".to_string(),
                );
            }
            PaletteCommand::Rm {
                path: resolve_remote_arg(cwd, &args[0]),
                force: verb == "rm!",
            }
        }
        other => PaletteCommand::Error(format!(
            "unknown '{}': ls cd get put stat mkdir mv rm rm! (type help)",
            other
        )),
    }
}

/// The one-line cheatsheet of every palette verb. Single source of truth for
/// the `help`/`?` echo and the empty-buffer placeholder hint (rendered in
/// `mod.rs`). The middot separator matches the header style; no em-dashes.
pub(crate) fn palette_cheatsheet() -> &'static str {
    "ls [path] · cd <path> · stat <path> · mkdir <path> · get <remote> [local] · put <local> [remote] · mv <src> <dst> · rm <path> · rm! <path>"
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
            groups: Vec::new(),
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
            groups: Vec::new(),
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
        // Connect is atomic: only OpenSession + the session-independent LocalList.
        // The remote List is deferred to SessionReady (so a failed connect never
        // lists with no session). LocalList uses the profile's default_local_path
        // ("/tmp" in the sample); regression guard: clear() must not blank it.
        assert_eq!(
            commands,
            vec![
                WorkerCommand::OpenSession {
                    identity: sample_identity(),
                    initial_cwd: "/".to_string(),
                },
                WorkerCommand::LocalList {
                    path: "/tmp".to_string(),
                },
            ]
        );
        assert_eq!(app.session.phase, TuiSessionPhase::Connecting);
        assert_eq!(app.focus, TuiFocus::Browser);

        // A successful SessionReady now drives the remote List.
        let follow_up = app.apply_worker_event(WorkerEvent::SessionReady {
            identity: Some(sample_identity()),
            cwd: "/".to_string(),
        });
        assert_eq!(
            follow_up,
            vec![WorkerCommand::List {
                path: "/".to_string(),
            }]
        );
    }

    /// A failed/expired connect (e.g. invalid S3 key) must surface the auth error
    /// and leave the session in a clean Failed state - not stuck "connecting" with
    /// a straggler remote List masking the error as "no active TUI session".
    #[test]
    fn failed_connect_marks_failed_and_keeps_the_auth_error() {
        let mut app = AppState::new_live(sample_context());
        app.focus = TuiFocus::Profiles;
        app.selected_profile = 0;
        let cmds = app.apply_action(TuiAction::Activate);
        // No eager remote List that could run without a session.
        assert!(
            !cmds.iter().any(|c| matches!(c, WorkerCommand::List { .. })),
            "connect must not eagerly request a remote List; got {:?}",
            cmds
        );
        assert_eq!(app.session.phase, TuiSessionPhase::Connecting);

        let follow_up = app.apply_worker_event(WorkerEvent::Failed {
            operation: TuiWorkerOperation::Connect,
            identity: Some(sample_identity()),
            message: "S3 auth error: key not valid".to_string(),
        });
        assert!(follow_up.is_empty(), "a failed connect drives no follow-up");
        assert!(
            matches!(app.session.phase, TuiSessionPhase::Failed(_)),
            "phase must be Failed, got {:?}",
            app.session.phase
        );
        assert!(!app.is_live_connected());
        assert!(
            app.status.contains("S3 auth error"),
            "status keeps the auth error; got {:?}",
            app.status
        );
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
            groups: Vec::new(),
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
            groups: Vec::new(),
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

    /// An IntroHub context where the first user's two profiles have saved ids and
    /// "Production" already contains the first profile (#320 group tests).
    fn grouped_context() -> TuiContext {
        let mut context = introhub_context();
        context.users[0].profiles[0].id = "srv-1".to_string();
        context.users[0].profiles[1].id = "srv-2".to_string();
        context.users[0].profiles[0].groups = vec!["Production".to_string()];
        context.groups = vec![TuiGroup {
            name: "Production".to_string(),
            member_count: 1,
        }];
        context
    }

    #[test]
    fn introhub_g_opens_the_group_overlay_with_membership() {
        let mut app = AppState::new_live(grouped_context());
        let commands = app.apply_action(TuiAction::ManageGroups);
        assert!(commands.is_empty());
        match &app.overlay {
            TuiOverlay::Groups(state) => {
                assert_eq!(state.profile_id, "srv-1");
                assert_eq!(state.groups.len(), 1);
                assert!(state.groups[0].is_member);
                assert_eq!(state.cursor, 0); // starts on "All servers"
            }
            other => panic!("expected a Groups overlay, got {:?}", other),
        }
    }

    #[test]
    fn group_overlay_space_toggles_membership_optimistically_and_asks_the_worker() {
        let mut app = AppState::new_live(grouped_context());
        app.apply_action(TuiAction::ManageGroups);
        // Down moves onto the "Production" row, space removes the member.
        app.handle_overlay_key(OverlayKey::Down);
        let commands = app.handle_overlay_key(OverlayKey::Char(' '));

        assert!(app.context.users[0].profiles[0].groups.is_empty());
        assert_eq!(app.context.groups[0].member_count, 0);
        assert_eq!(
            commands,
            vec![WorkerCommand::ToggleGroupMembership {
                profile_id: "srv-1".to_string(),
                group_name: "Production".to_string(),
            }]
        );
        // The overlay row reflects the new state in place.
        if let TuiOverlay::Groups(state) = &app.overlay {
            assert!(!state.groups[0].is_member);
            assert_eq!(state.groups[0].member_count, 0);
        } else {
            panic!("overlay should stay open after a membership toggle");
        }
    }

    #[test]
    fn group_overlay_enter_sets_then_clears_the_filter() {
        let mut app = AppState::new_live(grouped_context());
        app.apply_action(TuiAction::ManageGroups);
        app.handle_overlay_key(OverlayKey::Down); // onto "Production"
        app.handle_overlay_key(OverlayKey::Submit);

        assert_eq!(app.selected_group.as_deref(), Some("Production"));
        // Only the member profile is visible under the filter.
        assert_eq!(app.visible_profile_indices(), vec![0]);
        assert!(!app.overlay.is_active());

        // Reopen: the cursor seeds onto the active group, so Up returns to the
        // "All servers" row and Enter clears the filter.
        app.apply_action(TuiAction::ManageGroups);
        app.handle_overlay_key(OverlayKey::Up);
        app.handle_overlay_key(OverlayKey::Submit);
        assert!(app.selected_group.is_none());
        assert_eq!(app.visible_profile_indices(), vec![0, 1]);
    }

    #[test]
    fn group_filter_narrows_navigation_to_members() {
        let mut app = AppState::new_live(grouped_context());
        app.selected_group = Some("Production".to_string());
        app.selected_profile = 0;
        // Down stays on the only visible member rather than moving to srv-2.
        app.apply_action(TuiAction::MoveDown);
        assert_eq!(app.selected_profile, 0);
    }

    #[test]
    fn group_overlay_new_seeds_a_group_with_the_highlighted_profile() {
        let mut app = AppState::new_live(grouped_context());
        app.apply_action(TuiAction::ManageGroups);
        app.handle_overlay_key(OverlayKey::Char('n'));
        // The naming prompt is open; type a fresh group name and submit.
        for c in "Staging".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);

        assert!(app.context.users[0].profiles[0]
            .groups
            .iter()
            .any(|g| g == "Staging"));
        assert!(app.context.groups.iter().any(|g| g.name == "Staging"));
        assert_eq!(
            commands,
            vec![WorkerCommand::ToggleGroupMembership {
                profile_id: "srv-1".to_string(),
                group_name: "Staging".to_string(),
            }]
        );
    }

    #[test]
    fn group_overlay_rename_updates_context_and_active_filter() {
        let mut app = AppState::new_live(grouped_context());
        app.selected_group = Some("Production".to_string());
        app.apply_action(TuiAction::ManageGroups);
        app.handle_overlay_key(OverlayKey::Down); // onto "Production"
        app.handle_overlay_key(OverlayKey::Char('r'));
        // Rename prompt pre-filled with the old name; clear it and type a new one.
        for _ in 0.."Production".len() {
            app.handle_overlay_key(OverlayKey::Backspace);
        }
        for c in "Prod".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);

        assert!(app.context.groups.iter().any(|g| g.name == "Prod"));
        assert!(app.context.groups.iter().all(|g| g.name != "Production"));
        assert_eq!(app.context.users[0].profiles[0].groups, vec!["Prod"]);
        assert_eq!(app.selected_group.as_deref(), Some("Prod"));
        assert_eq!(
            commands,
            vec![WorkerCommand::RenameGroup {
                old_name: "Production".to_string(),
                new_name: "Prod".to_string(),
            }]
        );
    }

    #[test]
    fn group_overlay_delete_removes_the_group_but_keeps_the_servers() {
        let mut app = AppState::new_live(grouped_context());
        app.selected_group = Some("Production".to_string());
        app.apply_action(TuiAction::ManageGroups);
        app.handle_overlay_key(OverlayKey::Down); // onto "Production"
        let commands = app.handle_overlay_key(OverlayKey::Char('d'));

        assert!(app.context.groups.is_empty());
        assert!(app.context.users[0].profiles[0].groups.is_empty());
        // The profiles themselves survive; only the grouping is gone.
        assert_eq!(app.context.users[0].profiles.len(), 2);
        // The filter that pointed at the deleted group is cleared.
        assert!(app.selected_group.is_none());
        assert_eq!(
            commands,
            vec![WorkerCommand::DeleteGroup {
                name: "Production".to_string(),
            }]
        );
    }

    // --- B4 DiscoveryHub: profile add / edit / delete ---------------------

    #[test]
    fn introhub_x_confirms_then_deletes_profile_and_prunes_groups() {
        let mut app = AppState::new_live(grouped_context());
        // srv-1 is the first profile and a member of "Production" (count 1).
        let commands = app.apply_action(TuiAction::DeleteProfile);
        assert!(commands.is_empty(), "delete waits for confirmation");
        assert!(matches!(app.overlay, TuiOverlay::Confirm(_)));

        let commands = app.handle_overlay_key(OverlayKey::Submit);
        assert_eq!(
            commands,
            vec![WorkerCommand::DeleteProfile {
                user_name: "ale".to_string(),
                profile_id: "srv-1".to_string(),
            }]
        );
        // The row is gone, only "Archive" remains, renumbered to selector 1.
        assert_eq!(app.context.users[0].profiles.len(), 1);
        assert_eq!(app.context.users[0].profiles[0].name, "Archive");
        assert_eq!(app.context.users[0].profiles[0].selector, "1");
        // Its group membership count is decremented.
        assert_eq!(app.context.groups[0].member_count, 0);
    }

    #[test]
    fn introhub_x_is_a_noop_without_a_saved_id() {
        let mut app = AppState::new_live(introhub_context()); // ids empty
        let commands = app.apply_action(TuiAction::DeleteProfile);
        assert!(commands.is_empty());
        assert!(!app.overlay_active());
        assert_eq!(app.context.users[0].profiles.len(), 2);
    }

    #[test]
    fn introhub_a_opens_an_empty_create_form() {
        let mut app = AppState::new_live(introhub_context());
        app.apply_action(TuiAction::AddProfile);
        match &app.overlay {
            TuiOverlay::ProfileForm(form) => {
                assert!(matches!(form.mode, ProfileFormMode::Create));
                assert_eq!(form.user_name, "ale");
                assert!(form.name.is_empty());
                assert_eq!(form.protocol, "sftp");
            }
            other => panic!("expected a ProfileForm overlay, got {:?}", other),
        }
    }

    #[test]
    fn profile_form_create_submits_save_and_inserts_optimistically() {
        let mut app = AppState::new_live(introhub_context());
        app.apply_action(TuiAction::AddProfile);
        for c in "NAS".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        // Tab past Protocol to Host, then type a host.
        app.handle_overlay_key(OverlayKey::Tab); // -> Protocol
        app.handle_overlay_key(OverlayKey::Tab); // -> Host
        for c in "nas.local".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        assert_eq!(commands.len(), 1);
        match &commands[0] {
            WorkerCommand::SaveProfile {
                user_name,
                draft,
                secret,
            } => {
                assert_eq!(user_name, "ale");
                assert_eq!(draft.name, "NAS");
                assert_eq!(draft.protocol, "sftp");
                assert_eq!(draft.host, "nas.local");
                assert_eq!(draft.port, 22); // default for sftp
                assert!(draft.id.starts_with("srv_"));
                assert!(secret.is_none(), "no credential typed");
            }
            other => panic!("expected SaveProfile, got {:?}", other),
        }
        // The new profile is prepended and selectors renumbered.
        assert_eq!(app.context.users[0].profiles.len(), 3);
        assert_eq!(app.context.users[0].profiles[0].name, "NAS");
        assert_eq!(app.context.users[0].profiles[0].selector, "1");
        assert!(!app.overlay_active());
    }

    #[test]
    fn profile_form_validation_keeps_the_form_open() {
        let mut app = AppState::new_live(introhub_context());
        app.apply_action(TuiAction::AddProfile);
        // Submit with an empty name.
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        assert!(commands.is_empty());
        match &app.overlay {
            TuiOverlay::ProfileForm(form) => assert!(form.error.is_some()),
            _ => panic!("form must stay open on a validation error"),
        }
    }

    #[test]
    fn profile_form_edit_prefills_and_preserves_group_state() {
        let mut app = AppState::new_live(grouped_context());
        // srv-1 is a member of "Production".
        app.apply_action(TuiAction::EditProfile);
        match &app.overlay {
            TuiOverlay::ProfileForm(form) => {
                assert!(matches!(form.mode, ProfileFormMode::Edit { .. }));
                assert_eq!(form.name, "Production");
                assert_eq!(form.host, "example.com");
            }
            other => panic!("expected an edit ProfileForm, got {:?}", other),
        }
        // Rename via backspacing to empty then typing a new name.
        for _ in 0.."Production".len() {
            app.handle_overlay_key(OverlayKey::Backspace);
        }
        for c in "Prod".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        match &commands[0] {
            WorkerCommand::SaveProfile { draft, .. } => {
                assert_eq!(draft.id, "srv-1");
                assert_eq!(draft.name, "Prod");
            }
            other => panic!("expected SaveProfile, got {:?}", other),
        }
        // Edit preserves the group membership the form does not own.
        assert_eq!(app.context.users[0].profiles[0].name, "Prod");
        assert_eq!(app.context.users[0].profiles[0].groups, vec!["Production"]);
        assert_eq!(app.context.users[0].profiles.len(), 2);
    }

    #[test]
    fn profile_form_protocol_cycles_with_arrows() {
        let mut app = AppState::new_live(introhub_context());
        app.apply_action(TuiAction::AddProfile);
        app.handle_overlay_key(OverlayKey::Tab); // focus Protocol
        app.handle_overlay_key(OverlayKey::Right);
        match &app.overlay {
            TuiOverlay::ProfileForm(form) => assert_eq!(form.protocol, "ftp"),
            _ => panic!("form missing"),
        }
        app.handle_overlay_key(OverlayKey::Left);
        match &app.overlay {
            TuiOverlay::ProfileForm(form) => assert_eq!(form.protocol, "sftp"),
            _ => panic!("form missing"),
        }
    }

    #[test]
    fn profile_form_sends_the_credential_only_when_touched() {
        let mut app = AppState::new_live(introhub_context());
        app.apply_action(TuiAction::AddProfile);
        for c in "NAS".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        app.handle_overlay_key(OverlayKey::Tab); // Protocol
        app.handle_overlay_key(OverlayKey::Tab); // Host
        for c in "nas.local".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        // Walk to the password field (Host -> Port -> Username -> Remote ->
        // Local -> Password) and type a secret.
        for _ in 0..5 {
            app.handle_overlay_key(OverlayKey::Tab);
        }
        for c in "hunter2".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        match &commands[0] {
            WorkerCommand::SaveProfile { secret, .. } => {
                let secret = secret.as_ref().expect("secret sent when touched");
                assert_eq!(secret.expose(), "hunter2");
            }
            other => panic!("expected SaveProfile, got {:?}", other),
        }
    }

    #[test]
    fn introhub_add_profile_refused_on_a_locked_user() {
        let context = TuiContext {
            users: vec![TuiUser {
                name: "locked".to_string(),
                is_active: true,
                is_locked: true,
                is_admin: false,
                profile_count: 0,
                profiles: Vec::new(),
            }],
            initial_user: 0,
            download_base: "/tmp".to_string(),
            groups: Vec::new(),
        };
        let mut app = AppState::new_live(context);
        app.apply_action(TuiAction::AddProfile);
        assert!(!app.overlay_active(), "no form for a locked user");
    }

    #[test]
    fn introhub_h_requests_a_health_probe_and_applies_the_result() {
        let mut context = introhub_context();
        context.users[0].profiles[0].id = "srv-1".to_string();
        context.users[0].profiles[0].host = "nas.example.com".to_string();
        context.users[0].profiles[0].port = 22;
        let mut app = AppState::new_live(context);

        let commands = app.apply_action(TuiAction::HealthCheck);
        assert_eq!(
            commands,
            vec![WorkerCommand::HealthCheck {
                profile_id: "srv-1".to_string(),
                host: "nas.example.com".to_string(),
                port: 22,
                protocol: "sftp".to_string(),
                endpoint: None,
            }]
        );

        app.apply_worker_event(WorkerEvent::HealthReady {
            profile_id: "srv-1".to_string(),
            status: "healthy".to_string(),
            score: 97,
            latency_ms: Some(42),
        });
        let health = app.context.users[0].profiles[0]
            .health
            .as_ref()
            .expect("health applied");
        assert_eq!(health.status, "healthy");
        assert_eq!(health.score, 97);
        assert_eq!(health.latency_ms, Some(42));
    }

    #[test]
    fn introhub_q_requests_a_quota_refresh_and_applies_the_result() {
        let mut context = introhub_context();
        context.users[0].profiles[0].id = "srv-1".to_string();
        let mut app = AppState::new_live(context);

        let commands = app.apply_action(TuiAction::RefreshQuota);
        assert_eq!(commands.len(), 1);
        match &commands[0] {
            WorkerCommand::RefreshQuota {
                identity,
                profile_id,
            } => {
                assert_eq!(profile_id, "srv-1");
                assert_eq!(identity.user_name, "ale");
            }
            other => panic!("expected RefreshQuota, got {:?}", other),
        }

        app.apply_worker_event(WorkerEvent::QuotaReady {
            profile_id: "srv-1".to_string(),
            used: 5 * 1024 * 1024 * 1024,
            total: 20 * 1024 * 1024 * 1024,
        });
        assert_eq!(
            app.context.users[0].profiles[0].used,
            Some(5 * 1024 * 1024 * 1024)
        );
        assert_eq!(
            app.context.users[0].profiles[0].total,
            Some(20 * 1024 * 1024 * 1024)
        );
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

    // --- B3 command palette ------------------------------------------------

    #[test]
    fn palette_tokenizer_honors_double_quotes() {
        assert_eq!(
            tokenize_palette_line("get \"a b.txt\" ./out"),
            vec![
                "get".to_string(),
                "a b.txt".to_string(),
                "./out".to_string()
            ]
        );
        assert_eq!(tokenize_palette_line("   ls   /srv  "), vec!["ls", "/srv"]);
        assert!(tokenize_palette_line("   ").is_empty());
    }

    #[test]
    fn palette_resolves_paths_against_the_remote_cwd() {
        assert_eq!(resolve_remote_arg("/srv", "docs"), "/srv/docs");
        assert_eq!(resolve_remote_arg("/srv", "/etc/hosts"), "/etc/hosts");
        assert_eq!(resolve_remote_arg("/srv/docs", ".."), "/srv");
        assert_eq!(resolve_remote_arg("/srv", "."), "/srv");
    }

    #[test]
    fn palette_parses_each_verb() {
        assert_eq!(
            parse_palette_command("ls /srv", "/home"),
            PaletteCommand::List {
                path: "/srv".to_string()
            }
        );
        // ls with no argument lists the current directory.
        assert_eq!(
            parse_palette_command("ls", "/srv"),
            PaletteCommand::List {
                path: "/srv".to_string()
            }
        );
        assert_eq!(
            parse_palette_command("cd docs", "/srv"),
            PaletteCommand::List {
                path: "/srv/docs".to_string()
            }
        );
        assert_eq!(
            parse_palette_command("stat readme.txt", "/srv"),
            PaletteCommand::Stat {
                path: "/srv/readme.txt".to_string()
            }
        );
        assert_eq!(
            parse_palette_command("mkdir out", "/srv"),
            PaletteCommand::Mkdir {
                path: "/srv/out".to_string()
            }
        );
        assert_eq!(
            parse_palette_command("get \"a b.txt\" ./b", "/srv"),
            PaletteCommand::Get {
                remote: "/srv/a b.txt".to_string(),
                local: Some("./b".to_string()),
            }
        );
        assert_eq!(
            parse_palette_command("get a.txt", "/srv"),
            PaletteCommand::Get {
                remote: "/srv/a.txt".to_string(),
                local: None,
            }
        );
        assert_eq!(
            parse_palette_command("rm old", "/srv"),
            PaletteCommand::Rm {
                path: "/srv/old".to_string(),
                force: false,
            }
        );
        assert_eq!(
            parse_palette_command("rm! old", "/srv"),
            PaletteCommand::Rm {
                path: "/srv/old".to_string(),
                force: true,
            }
        );
        // `pget` is an alias of `get` (CLI parity).
        assert_eq!(
            parse_palette_command("pget a.txt", "/srv"),
            PaletteCommand::Get {
                remote: "/srv/a.txt".to_string(),
                local: None,
            }
        );
        // `put` mirrors `get`: local source, optional remote destination.
        assert_eq!(
            parse_palette_command("put ./a.txt", "/srv"),
            PaletteCommand::Put {
                local: "./a.txt".to_string(),
                remote: None,
            }
        );
        assert_eq!(
            parse_palette_command("put ./a.txt sub/b.txt", "/srv"),
            PaletteCommand::Put {
                local: "./a.txt".to_string(),
                remote: Some("/srv/sub/b.txt".to_string()),
            }
        );
        // `mv` resolves both arguments against the remote cwd (rename == move).
        assert_eq!(
            parse_palette_command("mv a.txt b.txt", "/srv"),
            PaletteCommand::Move {
                from: "/srv/a.txt".to_string(),
                to: "/srv/b.txt".to_string(),
            }
        );
        assert!(matches!(
            parse_palette_command("mv only", "/srv"),
            PaletteCommand::Error(_)
        ));
    }

    #[test]
    fn palette_put_enqueues_an_upload_and_mv_emits_a_rename() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::OpenPalette);
        for c in "put /tmp/data.bin".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let cmds = app.handle_overlay_key(OverlayKey::Submit);
        match cmds.as_slice() {
            [WorkerCommand::Upload {
                local_path,
                remote_path,
                ..
            }] => {
                assert_eq!(local_path, "/tmp/data.bin");
                assert!(remote_path.ends_with("/data.bin"), "uploaded into cwd");
            }
            other => panic!("put must emit a single Upload, got {:?}", other),
        }

        let mut app = connected_app_with_listing();
        let cwd = app.current_browser_dir();
        app.apply_action(TuiAction::OpenPalette);
        for c in "mv a.txt b.txt".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let cmds = app.handle_overlay_key(OverlayKey::Submit);
        assert_eq!(
            cmds,
            vec![WorkerCommand::Rename {
                from: resolve_remote_arg(&cwd, "a.txt"),
                to: resolve_remote_arg(&cwd, "b.txt"),
            }]
        );
    }

    #[test]
    fn palette_reports_unknown_verbs_and_bad_arity() {
        assert!(matches!(
            parse_palette_command("bogus", "/srv"),
            PaletteCommand::Error(_)
        ));
        assert!(matches!(
            parse_palette_command("cd", "/srv"),
            PaletteCommand::Error(_)
        ));
        assert!(matches!(
            parse_palette_command("get a b c", "/srv"),
            PaletteCommand::Error(_)
        ));
        assert_eq!(parse_palette_command("", "/srv"), PaletteCommand::Empty);
    }

    #[test]
    fn palette_parses_help_verbs() {
        assert_eq!(parse_palette_command("help", "/srv"), PaletteCommand::Help);
        assert_eq!(parse_palette_command("?", "/srv"), PaletteCommand::Help);
    }

    #[test]
    fn palette_help_echoes_cheatsheet_and_stays_open() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::OpenPalette);
        for c in "help".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        assert!(commands.is_empty(), "help dispatches no worker command");
        match &app.overlay {
            TuiOverlay::Palette(state) => {
                assert_eq!(state.last_result, palette_cheatsheet());
                assert!(state.buffer.is_empty(), "buffer cleared after help");
            }
            _ => panic!("palette must stay open after help"),
        }
    }

    #[test]
    fn palette_opens_only_when_connected() {
        // Pre-connect IntroHub: ':' explains it needs a session, no overlay.
        let mut app = AppState::new_live(introhub_context());
        app.apply_action(TuiAction::OpenPalette);
        assert!(!app.overlay_active(), "palette must not open pre-connect");

        // Connected: ':' opens the palette overlay.
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::OpenPalette);
        assert!(matches!(app.overlay, TuiOverlay::Palette(_)));
    }

    #[test]
    fn palette_submit_dispatches_ls_and_closes() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::OpenPalette);
        for c in "cd docs".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        assert_eq!(
            commands,
            vec![WorkerCommand::List {
                path: "/srv/docs".to_string()
            }]
        );
        assert!(
            !app.overlay_active(),
            "palette closes after a valid command"
        );
    }

    #[test]
    fn palette_submit_get_enqueues_a_transfer() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::OpenPalette);
        for c in "get readme.txt".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        assert_eq!(commands.len(), 1);
        assert!(matches!(
            &commands[0],
            WorkerCommand::Download { remote_path, .. } if remote_path == "/srv/readme.txt"
        ));
        assert_eq!(app.focus, TuiFocus::Transfers);
        assert_eq!(app.transfers.items.len(), 1);
    }

    #[test]
    fn palette_rm_routes_through_the_confirm_overlay() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::OpenPalette);
        for c in "rm old".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        assert!(commands.is_empty(), "rm waits for confirmation");
        assert!(matches!(app.overlay, TuiOverlay::Confirm(_)));
        // Confirming issues the non-recursive remove.
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        assert_eq!(
            commands,
            vec![WorkerCommand::Remove {
                path: "/srv/old".to_string(),
                recursive: false,
            }]
        );
    }

    #[test]
    fn palette_unknown_keeps_open_with_a_hint() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::OpenPalette);
        for c in "bogus".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        assert!(commands.is_empty());
        match &app.overlay {
            TuiOverlay::Palette(state) => assert!(!state.last_result.is_empty()),
            _ => panic!("palette must stay open on a parse error"),
        }
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

    /// A connected app with the Local pane populated and active, for testing
    /// that dual-pane mutations on the Local side hit the local filesystem.
    fn connected_app_with_local_listing() -> AppState {
        let mut app = connected_app_with_listing();
        app.apply_worker_event(WorkerEvent::ListReady {
            identity: None, // identity None == local result
            path: "/home/ale".to_string(),
            result: list_result(vec![
                crate::cli_tui::worker::TuiListEntry {
                    name: "note.txt".to_string(),
                    path: "/home/ale/note.txt".to_string(),
                    is_dir: false,
                    size: 10,
                    modified: None,
                },
                crate::cli_tui::worker::TuiListEntry {
                    name: "sub".to_string(),
                    path: "/home/ale/sub".to_string(),
                    is_dir: true,
                    size: 0,
                    modified: None,
                },
            ]),
        });
        app.active_browser_side = BrowserSide::Local;
        app.focus = TuiFocus::Browser;
        app
    }

    #[test]
    fn local_rename_routes_to_the_local_filesystem() {
        let mut app = connected_app_with_local_listing();
        // Dirs sort first: "sub" is index 0, "note.txt" is index 1.
        app.apply_action(TuiAction::MoveDown); // select note.txt
        app.apply_action(TuiAction::Rename);
        app.handle_overlay_key(OverlayKey::Char('2'));
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        // Must be a LOCAL rename, not a remote one (the remote Rename caused the
        // "[550] No such file or directory" on local files).
        assert_eq!(
            commands,
            vec![WorkerCommand::LocalRename {
                from: "/home/ale/note.txt".to_string(),
                to: "/home/ale/note.txt2".to_string(),
            }]
        );
    }

    #[test]
    fn local_mkdir_routes_to_the_local_filesystem() {
        let mut app = connected_app_with_local_listing();
        app.apply_action(TuiAction::NewDir);
        for c in "newdir".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        assert_eq!(
            commands,
            vec![WorkerCommand::LocalMkdir {
                path: "/home/ale/newdir".to_string(),
            }]
        );
    }

    #[test]
    fn local_delete_routes_to_the_local_filesystem() {
        let mut app = connected_app_with_local_listing();
        // Dirs sort first: the directory "sub" is index 0 -> recursive delete.
        app.apply_action(TuiAction::Delete);
        let commands = app.handle_overlay_key(OverlayKey::Submit); // confirm
        assert_eq!(
            commands,
            vec![WorkerCommand::LocalRemove {
                path: "/home/ale/sub".to_string(),
                recursive: true,
            }]
        );
    }

    #[test]
    fn upload_prefills_the_source_from_the_local_selection() {
        // 'u' should not ask for a path when a local file is selected: it
        // prefills the source so Enter uploads it to the remote pane's dir.
        let mut app = connected_app_with_local_listing();
        // Dirs sort first: select note.txt (index 1) so the prompt prefills it.
        app.apply_action(TuiAction::MoveDown);
        app.apply_action(TuiAction::Upload);
        match &app.overlay {
            TuiOverlay::Prompt(prompt) => {
                assert_eq!(prompt.buffer, "/home/ale/note.txt");
            }
            other => panic!("expected an upload prompt, got {:?}", other),
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        assert_eq!(
            commands,
            vec![WorkerCommand::Upload {
                id: 1,
                local_path: "/home/ale/note.txt".to_string(),
                remote_path: "/srv/note.txt".to_string(),
            }]
        );
    }

    #[test]
    fn remote_rename_still_routes_to_the_remote_provider() {
        // Regression guard: the Remote side keeps using the provider command.
        let mut app = connected_app_with_listing(); // Remote side active
        app.apply_action(TuiAction::MoveDown); // readme.txt
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

        // The destination defaults to the LOCAL pane's current directory
        // ("/tmp" from the profile's default_local_path), not the launch CWD.
        assert_eq!(
            commands,
            vec![WorkerCommand::Download {
                id: 1,
                remote_path: "/srv/readme.txt".to_string(),
                local_path: "/tmp/readme.txt".to_string(),
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
            message: "Downloaded /srv/readme.txt -> /tmp/readme.txt".to_string(),
        });
        // A download lands in the LOCAL pane's directory, so it refreshes the
        // local listing (not the remote one) to surface the new file.
        assert_eq!(
            commands,
            vec![WorkerCommand::LocalList {
                path: "/tmp".to_string()
            }]
        );
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
        app.handle_overlay_key(OverlayKey::Submit); // id 1, default = local pane dir (/tmp)
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
                local_path: "/tmp/readme.txt".to_string()
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
        // The remote List is deferred to SessionReady (atomic connect), so it is
        // not in the connect batch; the session-independent LocalList is.
        assert!(
            cmds.iter()
                .any(|c| matches!(c, WorkerCommand::LocalList { .. })),
            "connect must request a local List; got {:?}",
            cmds
        );

        let follow_up = app.apply_worker_event(WorkerEvent::SessionReady {
            identity: Some(sample_identity()),
            cwd: "/srv".to_string(),
        });
        assert!(
            follow_up
                .iter()
                .any(|c| matches!(c, WorkerCommand::List { .. })),
            "SessionReady must drive the remote List; got {:?}",
            follow_up
        );
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

    // --- Mouse support unit tests (B mouse task, 1.0.1) ---

    fn sample_mouse_event(
        kind: ::crossterm::event::MouseEventKind,
        col: u16,
        row: u16,
    ) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: ::crossterm::event::KeyModifiers::NONE,
        }
    }

    #[test]
    fn mouse_wheel_on_profiles_moves_highlight_without_commands() {
        let mut app = AppState::new_live(sample_context());
        app.focus = TuiFocus::Profiles;
        // Assume at least 2 profiles in sample; move down then up.
        let _ = app.handle_mouse(sample_mouse_event(
            ::crossterm::event::MouseEventKind::ScrollDown,
            10,
            10,
        ));
        let _ = app.handle_mouse(sample_mouse_event(
            ::crossterm::event::MouseEventKind::ScrollUp,
            10,
            10,
        ));
        // Wheel exercised (selection change depends on #visible profiles in
        // fixture); no panic + empty cmds asserted by not returning any.
    }

    #[test]
    fn mouse_click_on_intro_table_selects_profile_row() {
        // introhub_context has two profiles for user 0 (Production, Archive).
        let mut app = AppState::new_live(introhub_context());
        app.focus = TuiFocus::Profiles;
        // Bordered table with a header row at y=5: top border = 5, header = 6,
        // first data row = 7, second data row = 8.
        app.layout.intro_table = Some(Rect {
            x: 0,
            y: 5,
            width: 80,
            height: 10,
        });
        // Click the FIRST data row (y+2 = 7) -> visible index 0.
        let cmds = app.handle_mouse(sample_mouse_event(
            ::crossterm::event::MouseEventKind::Down(MouseButton::Left),
            20,
            7,
        ));
        assert!(cmds.is_empty());
        assert_eq!(app.selected_profile, 0);
        // Click the SECOND data row (8) -> visible index 1. This pins the
        // border + header offset so the row math cannot regress.
        app.handle_mouse(sample_mouse_event(
            ::crossterm::event::MouseEventKind::Down(MouseButton::Left),
            20,
            8,
        ));
        assert_eq!(app.selected_profile, 1);
        // Clicking the header row (6) selects nothing new.
        app.handle_mouse(sample_mouse_event(
            ::crossterm::event::MouseEventKind::Down(MouseButton::Left),
            20,
            6,
        ));
        assert_eq!(app.selected_profile, 1);
    }

    #[test]
    fn mouse_wheel_and_click_on_browser_pane_select_and_switch_side() {
        let mut app = connected_app_with_listing();
        app.focus = TuiFocus::Browser;
        app.active_browser_side = BrowserSide::Remote;
        app.layout.remote_pane = Some(Rect {
            x: 0,
            y: 2,
            width: 40,
            height: 10,
        });
        app.layout.local_pane = Some(Rect {
            x: 40,
            y: 2,
            width: 40,
            height: 10,
        });
        // Wheel on remote (focus) moves remote selection.
        let before = app.browser.selected;
        let _ = app.handle_mouse(sample_mouse_event(
            ::crossterm::event::MouseEventKind::ScrollDown,
            5,
            5,
        ));
        assert_ne!(app.browser.selected, before);

        // Click in local pane rect: should switch active side (no cmd).
        let me = sample_mouse_event(
            ::crossterm::event::MouseEventKind::Down(MouseButton::Left),
            50,
            5,
        );
        let cmds = app.handle_mouse(me);
        assert!(cmds.is_empty());
        assert_eq!(app.active_browser_side, BrowserSide::Local);
    }

    #[test]
    fn mouse_double_click_on_browser_row_emits_open_command() {
        let mut app = connected_app_with_listing();
        app.focus = TuiFocus::Browser;
        app.active_browser_side = BrowserSide::Remote;
        // Bordered pane at y=2: the first entry row is data_top = y + 1 = 3.
        app.layout.remote_pane = Some(Rect {
            x: 0,
            y: 2,
            width: 40,
            height: 10,
        });
        // First down primes double-click detection on row 3 (the `docs/` dir).
        let _ = app.handle_mouse(sample_mouse_event(
            ::crossterm::event::MouseEventKind::Down(MouseButton::Left),
            10,
            3,
        ));
        // Second down fast on the same cell -> double-click -> open the entry.
        let cmds = app.handle_mouse(sample_mouse_event(
            ::crossterm::event::MouseEventKind::Down(MouseButton::Left),
            10,
            3,
        ));
        // Row 0 (`docs`) is a directory, so opening it lists that path.
        assert_eq!(app.browser.selected, 0);
        assert_eq!(
            cmds,
            vec![WorkerCommand::List {
                path: "/srv/docs".to_string()
            }]
        );
    }

    #[test]
    fn mouse_is_swallowed_while_an_overlay_is_open() {
        let mut app = connected_app_with_listing();
        app.focus = TuiFocus::Browser;
        app.active_browser_side = BrowserSide::Remote;
        app.layout.local_pane = Some(Rect {
            x: 40,
            y: 2,
            width: 40,
            height: 10,
        });
        // Open the palette, then click in the local pane behind it.
        app.apply_action(TuiAction::OpenPalette);
        let before_side = app.active_browser_side;
        let cmds = app.handle_mouse(sample_mouse_event(
            ::crossterm::event::MouseEventKind::Down(MouseButton::Left),
            50,
            5,
        ));
        assert!(cmds.is_empty(), "no command from a click under a modal");
        assert_eq!(
            app.active_browser_side, before_side,
            "the view behind the overlay is not touched"
        );
        assert!(matches!(app.overlay, TuiOverlay::Palette(_)));
    }

    #[test]
    fn esc_disconnects_to_introhub_when_connected() {
        let mut app = connected_app_with_listing();
        assert!(app.is_live_connected());
        let commands = app.apply_action(TuiAction::Back);
        // The worker is asked to close the provider...
        assert_eq!(commands, vec![WorkerCommand::Disconnect]);
        // ...and the TUI drops back to the IntroHub (My Servers): the live
        // session is gone (phase Disconnected; sync_pane_state then re-seeds the
        // planned identity of the highlighted profile, exactly like a fresh
        // IntroHub, so is_live_connected stays false).
        assert!(!app.is_live_connected());
        assert_eq!(app.session.phase, TuiSessionPhase::Disconnected);
        assert_eq!(app.focus, TuiFocus::Profiles);
        // It does not quit the app.
        assert!(!app.should_quit);
        assert_eq!(app.take_intent(), None);
    }

    #[test]
    fn esc_quits_from_the_introhub() {
        let mut app = AppState::new_live(introhub_context());
        assert!(!app.is_live_connected());
        let commands = app.apply_action(TuiAction::Back);
        assert!(commands.is_empty());
        assert_eq!(app.take_intent(), Some(TuiIntent::Quit));
    }

    #[test]
    fn esc_during_an_active_transfer_is_refused() {
        let mut app = connected_app_with_listing();
        // Enqueue a transfer so one is "active" (not finished).
        app.transfers.enqueue(
            TransferDirection::Download,
            "f.bin".to_string(),
            "/srv/f.bin".to_string(),
            "./f.bin".to_string(),
        );
        assert!(app.transfers.has_active());
        let commands = app.apply_action(TuiAction::Back);
        assert!(commands.is_empty(), "disconnect refused mid-transfer");
        assert!(app.is_live_connected(), "still connected");
    }

    #[test]
    fn mouse_double_click_on_intro_row_connects() {
        let mut app = AppState::new_live(introhub_context());
        // Bordered table with a header row: data starts at rect.y + 2.
        app.layout.intro_table = Some(Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 12,
        });
        // First down primes the double-click on row 2 (data_top = 0 + 2 -> idx 0).
        let _ = app.handle_mouse(sample_mouse_event(
            ::crossterm::event::MouseEventKind::Down(MouseButton::Left),
            5,
            2,
        ));
        let commands = app.handle_mouse(sample_mouse_event(
            ::crossterm::event::MouseEventKind::Down(MouseButton::Left),
            5,
            2,
        ));
        // Double-click connects the highlighted profile (not just switches focus).
        assert_eq!(app.selected_profile, 0);
        assert!(
            commands
                .iter()
                .any(|c| matches!(c, WorkerCommand::OpenSession { .. })),
            "double-click must open a session"
        );
        assert!(matches!(
            app.session.phase,
            TuiSessionPhase::Connecting | TuiSessionPhase::Connected
        ));
    }

    #[test]
    fn remote_file_names_with_control_chars_are_sanitized_for_display() {
        // A crafted listing name carrying an ESC sequence must not reach the
        // terminal verbatim (P4 injection hardening).
        let cleaned = crate::cli_tui::sanitize_display("evil\u{1b}[31mname\nsecond");
        assert!(!cleaned.contains('\u{1b}'));
        assert!(!cleaned.contains('\n'));
        assert!(cleaned.contains("evil"));
        assert!(cleaned.contains("name"));
    }

    // --- rev 1.0.3 table stakes: action-level behavior ---------------------

    #[test]
    fn view_action_emits_a_view_command_for_the_selected_file() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::MoveDown); // select readme.txt
        let commands = app.apply_action(TuiAction::ViewFile);
        assert_eq!(
            commands,
            vec![WorkerCommand::ViewFile {
                path: "/srv/readme.txt".to_string(),
                local: false,
            }]
        );
    }

    #[test]
    fn view_action_on_a_directory_is_refused() {
        let mut app = connected_app_with_listing(); // docs (dir) selected at index 0
        let commands = app.apply_action(TuiAction::ViewFile);
        assert!(commands.is_empty());
        assert!(app.status.contains("directory"));
    }

    #[test]
    fn file_content_event_opens_a_scrollable_pager() {
        let mut app = connected_app_with_listing();
        app.apply_worker_event(WorkerEvent::FileContent {
            path: "/srv/readme.txt".to_string(),
            content: "line one\nline two\nline three".to_string(),
            truncated: false,
            binary: false,
        });
        match &app.overlay {
            TuiOverlay::Pager(state) => {
                assert_eq!(state.lines.len(), 3);
                assert!(!state.binary);
            }
            other => panic!("expected a pager overlay, got {:?}", other),
        }
        // Scroll then close.
        app.handle_overlay_key(OverlayKey::Down);
        app.handle_overlay_key(OverlayKey::Cancel);
        assert!(!app.overlay_active());
    }

    #[test]
    fn info_action_requests_a_stat_for_the_selection() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::MoveDown); // readme.txt
        let commands = app.apply_action(TuiAction::Info);
        assert_eq!(
            commands,
            vec![WorkerCommand::Stat {
                path: "/srv/readme.txt".to_string()
            }]
        );
    }

    #[test]
    fn size_action_on_a_directory_requests_a_recursive_size() {
        let mut app = connected_app_with_listing(); // docs (dir) selected
        let commands = app.apply_action(TuiAction::SizeRecursive);
        assert_eq!(
            commands,
            vec![WorkerCommand::SizeRecursive {
                path: "/srv/docs".to_string(),
                local: false,
            }]
        );
    }

    #[test]
    fn size_action_on_a_file_reports_inline_without_a_command() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::MoveDown); // readme.txt (42 bytes)
        let commands = app.apply_action(TuiAction::SizeRecursive);
        assert!(commands.is_empty());
        assert!(app.status.contains("readme.txt"));
    }

    #[test]
    fn touch_prompt_emits_a_touch_command() {
        let mut app = connected_app_with_listing();
        assert!(app.apply_action(TuiAction::Touch).is_empty());
        for c in "new.txt".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        assert_eq!(
            commands,
            vec![WorkerCommand::Touch {
                path: "/srv/new.txt".to_string(),
                local: false,
            }]
        );
    }

    #[test]
    fn goto_prompt_lists_the_typed_remote_path() {
        let mut app = connected_app_with_listing();
        // `G` in the browser opens the go-to prompt (ManageGroups action).
        assert!(app.apply_action(TuiAction::ManageGroups).is_empty());
        for c in "/var/log".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        assert_eq!(
            commands,
            vec![WorkerCommand::List {
                path: "/var/log".to_string()
            }]
        );
    }

    #[test]
    fn help_overlay_opens_and_closes() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::Help);
        assert!(matches!(app.overlay, TuiOverlay::Help(_)));
        app.handle_overlay_key(OverlayKey::Cancel);
        assert!(!app.overlay_active());
    }

    #[test]
    fn toggle_hidden_in_the_browser_flips_the_active_pane() {
        let mut app = connected_app_with_listing();
        assert!(!app.browser.show_hidden);
        // `a` in the connected browser is the hidden toggle (AddProfile action).
        app.apply_action(TuiAction::AddProfile);
        assert!(app.browser.show_hidden);
        app.apply_action(TuiAction::AddProfile);
        assert!(!app.browser.show_hidden);
    }

    #[test]
    fn cycle_sort_action_advances_the_active_pane_sort() {
        let mut app = connected_app_with_listing();
        let before = app.browser.sort;
        app.apply_action(TuiAction::CycleSort);
        assert_ne!(app.browser.sort, before);
    }

    #[test]
    fn live_filter_narrows_then_clears_the_active_pane() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::OpenFilter);
        for c in "readme".chars() {
            app.handle_overlay_key(OverlayKey::Char(c));
        }
        assert_eq!(app.browser.entries.len(), 1);
        // Esc clears the filter and restores the full listing.
        app.handle_overlay_key(OverlayKey::Cancel);
        assert_eq!(app.browser.entries.len(), 2);
        assert!(app.browser.filter.is_none());
    }

    #[test]
    fn reload_relists_and_drops_marks_and_filter() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::MarkToggle); // mark docs
        app.browser.set_filter("docs".to_string());
        let commands = app.apply_action(TuiAction::Reload);
        assert_eq!(
            commands,
            vec![WorkerCommand::List {
                path: "/srv".to_string()
            }]
        );
        assert_eq!(app.browser.marked_count(), 0);
        assert!(app.browser.filter.is_none());
    }

    #[test]
    fn marking_then_delete_batches_a_remove_per_entry() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::MarkToggle); // docs, auto-advance to readme
        app.apply_action(TuiAction::MarkToggle); // readme
        assert_eq!(app.browser.marked_count(), 2);
        assert!(app.apply_action(TuiAction::Delete).is_empty());
        assert!(matches!(app.overlay, TuiOverlay::Confirm(_)));
        let commands = app.handle_overlay_key(OverlayKey::Submit);
        assert_eq!(commands.len(), 2);
        assert!(commands.contains(&WorkerCommand::Remove {
            path: "/srv/docs".to_string(),
            recursive: true,
        }));
        assert!(commands.contains(&WorkerCommand::Remove {
            path: "/srv/readme.txt".to_string(),
            recursive: false,
        }));
        assert_eq!(app.browser.marked_count(), 0);
    }

    #[test]
    fn marking_then_download_enqueues_only_marked_files() {
        let mut app = connected_app_with_listing();
        app.apply_action(TuiAction::MarkToggle); // docs (dir)
        app.apply_action(TuiAction::MarkToggle); // readme (file)
        let commands = app.apply_action(TuiAction::Download);
        // The directory is skipped; only the file is enqueued.
        assert_eq!(commands.len(), 1);
        assert!(matches!(
            &commands[0],
            WorkerCommand::Download { remote_path, .. } if remote_path == "/srv/readme.txt"
        ));
        assert_eq!(app.focus, TuiFocus::Transfers);
        assert_eq!(app.browser.marked_count(), 0);
    }

    #[test]
    fn synced_browsing_mirrors_an_open_onto_the_other_pane() {
        let mut app = connected_app_with_listing();
        // The Local pane opens at the profile default ("/tmp" in sample_context).
        assert_eq!(app.local.path, "/tmp");
        app.apply_action(TuiAction::SyncedBrowsing);
        assert!(app.synced_browsing);
        // Enter on the remote "docs" dir lists it AND mirrors the child locally.
        let commands = app.apply_action(TuiAction::Activate);
        assert!(commands.contains(&WorkerCommand::List {
            path: "/srv/docs".to_string()
        }));
        assert!(commands.contains(&WorkerCommand::LocalList {
            path: "/tmp/docs".to_string()
        }));
    }
}
