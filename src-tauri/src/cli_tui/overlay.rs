//! Modal overlay state for the interactive TUI.
//!
//! Overlays are the only place the dashboard collects free-form input or a
//! yes/no decision before issuing a mutating worker command. They carry no
//! provider or vault knowledge: the enclosing [`crate::cli_tui::app::AppState`]
//! turns a committed overlay into the concrete
//! [`crate::cli_tui::worker::WorkerCommand`], keeping the "TUI is state, render
//! and input only" invariant intact.

use crate::cli_tui::worker::TuiSecret;

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub enum TuiOverlay {
    #[default]
    None,
    Prompt(PromptState),
    Confirm(ConfirmState),
    /// The named-group manager for the highlighted saved profile (#320): a menu
    /// to filter by a group, toggle the profile's membership, and create /
    /// rename / delete groups. The named generalisation of the `f` favourite.
    Groups(GroupsOverlayState),
    /// `:` command palette (B3): line editor dispatching to existing
    /// WorkerCommands (ls/cd/get/stat/mkdir/rm) against the held session only.
    /// Parser + dispatch live in AppState; overlay is pure input state.
    Palette(PaletteState),
    /// DiscoveryHub profile form (B4): create or edit a saved profile's
    /// metadata (and, optionally, its credential). A vertical list of labelled
    /// fields with one focused field; Tab/Up/Down move, Left/Right cycle the
    /// protocol, Enter submits, Esc cancels. The vault write happens in the
    /// worker via `SaveProfile`; the form only collects intent.
    ProfileForm(ProfileFormState),
}

/// Modal group manager state. Acts on the IntroHub's highlighted profile.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GroupsOverlayState {
    /// Saved-profile id the overlay acts on (membership toggle / new-group seed).
    /// Empty when the highlighted entry has no saved id (toggles are then inert).
    pub profile_id: String,
    /// Display name of that profile, for the overlay title.
    pub profile_name: String,
    /// Cursor: 0 selects the "All servers" row (clear filter); 1..=groups.len()
    /// indexes `groups[cursor - 1]`.
    pub cursor: usize,
    /// Every known group, with the highlighted profile's membership flag.
    pub groups: Vec<GroupsOverlayItem>,
}

impl GroupsOverlayState {
    /// Number of selectable rows: the "All servers" row plus one per group.
    pub fn row_count(&self) -> usize {
        self.groups.len() + 1
    }

    /// Move the cursor by `delta`, clamped to the selectable rows.
    pub fn move_cursor(&mut self, delta: isize) {
        let max = self.row_count().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
    }

    /// The group under the cursor, or `None` on the "All servers" row.
    pub fn selected_group(&self) -> Option<&GroupsOverlayItem> {
        self.cursor.checked_sub(1).and_then(|i| self.groups.get(i))
    }
}

/// One row of the group overlay: a group plus whether the acted-on profile
/// belongs to it.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GroupsOverlayItem {
    pub name: String,
    pub member_count: usize,
    pub is_member: bool,
}

/// Palette input state for `:` (B3). Dedicated variant (distinct submit
/// semantics from Prompt which returns a single value to the caller).
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct PaletteState {
    pub buffer: String,
    /// Last command echo + one-line result or usage hint (shown above the
    /// input line while the palette stays open on error).
    pub last_result: String,
}

#[allow(dead_code)]
impl PaletteState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        self.buffer.push(c);
    }

    pub fn backspace(&mut self) {
        self.buffer.pop();
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
    }
}

impl TuiOverlay {
    pub fn is_active(&self) -> bool {
        !matches!(self, TuiOverlay::None)
    }
}

/// Protocols offered by the DiscoveryHub form's cycling protocol field (B4).
/// Limited to the connection-oriented backends a plain host/port/credential
/// form can express; the OAuth/cloud providers need their own flow and are a
/// later increment (tracked in `todo.md`). Editing a profile whose protocol is
/// outside this list keeps the existing value (the cycle just starts here).
pub const TUI_FORM_PROTOCOLS: &[&str] = &["sftp", "ftp", "ftps", "webdav", "s3"];

/// Whether the DiscoveryHub form is creating a new profile or editing one.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ProfileFormMode {
    /// Create a new profile; the worker mints the id.
    Create,
    /// Edit the saved profile with this id.
    Edit { id: String },
}

/// The fields of the DiscoveryHub form, in tab order. `Password` is masked in
/// the render and never echoed; the rest are plain text.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ProfileFieldKind {
    Name,
    Protocol,
    Host,
    Port,
    Username,
    InitialPath,
    LocalPath,
    Password,
}

/// Tab order for the form fields.
pub const PROFILE_FIELD_ORDER: [ProfileFieldKind; 8] = [
    ProfileFieldKind::Name,
    ProfileFieldKind::Protocol,
    ProfileFieldKind::Host,
    ProfileFieldKind::Port,
    ProfileFieldKind::Username,
    ProfileFieldKind::InitialPath,
    ProfileFieldKind::LocalPath,
    ProfileFieldKind::Password,
];

impl ProfileFieldKind {
    pub fn label(self) -> &'static str {
        match self {
            ProfileFieldKind::Name => "Name",
            ProfileFieldKind::Protocol => "Protocol",
            ProfileFieldKind::Host => "Host",
            ProfileFieldKind::Port => "Port",
            ProfileFieldKind::Username => "Username",
            ProfileFieldKind::InitialPath => "Remote path",
            ProfileFieldKind::LocalPath => "Local path",
            ProfileFieldKind::Password => "Password",
        }
    }
}

/// State of the DiscoveryHub profile form (B4). Pure input state: navigation,
/// per-field editing, and a transient validation error. AppState turns a
/// submitted form into a `SaveProfile` worker command; the form never touches
/// the vault. The password lives in a [`TuiSecret`] so it is masked in `Debug`
/// and zeroized on drop.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProfileFormState {
    pub mode: ProfileFormMode,
    /// User partition the profile belongs to (the IntroHub's selected user).
    pub user_name: String,
    pub name: String,
    pub protocol: String,
    pub host: String,
    pub port: String,
    pub username: String,
    pub initial_path: String,
    pub local_path: String,
    pub password: TuiSecret,
    /// Set once the password field is edited, so an untouched edit-form does not
    /// overwrite an existing credential with an empty one.
    pub password_touched: bool,
    /// Index into [`PROFILE_FIELD_ORDER`].
    pub focus: usize,
    /// Last validation error, shown under the fields until the next submit.
    pub error: Option<String>,
}

impl ProfileFormState {
    /// An empty create-form for `user_name`, focused on the first field with the
    /// first cycling protocol pre-selected.
    pub fn new_create(user_name: String) -> Self {
        Self {
            mode: ProfileFormMode::Create,
            user_name,
            name: String::new(),
            protocol: TUI_FORM_PROTOCOLS[0].to_string(),
            host: String::new(),
            port: String::new(),
            username: String::new(),
            initial_path: String::new(),
            local_path: String::new(),
            password: TuiSecret::default(),
            password_touched: false,
            focus: 0,
            error: None,
        }
    }

    /// The field currently under the cursor.
    pub fn current_field(&self) -> ProfileFieldKind {
        PROFILE_FIELD_ORDER[self.focus.min(PROFILE_FIELD_ORDER.len() - 1)]
    }

    pub fn focus_next(&mut self) {
        self.focus = (self.focus + 1) % PROFILE_FIELD_ORDER.len();
    }

    pub fn focus_prev(&mut self) {
        self.focus = (self.focus + PROFILE_FIELD_ORDER.len() - 1) % PROFILE_FIELD_ORDER.len();
    }

    /// Append a character to the focused field. The port field accepts digits
    /// only; the password field records that it was touched.
    pub fn push_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        match self.current_field() {
            ProfileFieldKind::Name => self.name.push(c),
            // Protocol is normally cycled with Left/Right, but typing is also
            // allowed (then validated on submit).
            ProfileFieldKind::Protocol => self.protocol.push(c),
            ProfileFieldKind::Host => self.host.push(c),
            ProfileFieldKind::Port => {
                if c.is_ascii_digit() && self.port.len() < 5 {
                    self.port.push(c);
                }
            }
            ProfileFieldKind::Username => self.username.push(c),
            ProfileFieldKind::InitialPath => self.initial_path.push(c),
            ProfileFieldKind::LocalPath => self.local_path.push(c),
            ProfileFieldKind::Password => {
                self.password.push(c);
                self.password_touched = true;
            }
        }
    }

    /// Delete the last character of the focused field.
    pub fn backspace(&mut self) {
        match self.current_field() {
            ProfileFieldKind::Name => {
                self.name.pop();
            }
            ProfileFieldKind::Protocol => {
                self.protocol.pop();
            }
            ProfileFieldKind::Host => {
                self.host.pop();
            }
            ProfileFieldKind::Port => {
                self.port.pop();
            }
            ProfileFieldKind::Username => {
                self.username.pop();
            }
            ProfileFieldKind::InitialPath => {
                self.initial_path.pop();
            }
            ProfileFieldKind::LocalPath => {
                self.local_path.pop();
            }
            ProfileFieldKind::Password => {
                self.password.pop();
                self.password_touched = true;
            }
        }
    }

    /// Cycle the protocol field through [`TUI_FORM_PROTOCOLS`]. A no-op unless
    /// the protocol field is focused. `delta` is +1 (Right) or -1 (Left).
    pub fn cycle_protocol(&mut self, delta: isize) {
        if self.current_field() != ProfileFieldKind::Protocol {
            return;
        }
        let n = TUI_FORM_PROTOCOLS.len() as isize;
        let cur = TUI_FORM_PROTOCOLS
            .iter()
            .position(|p| p.eq_ignore_ascii_case(&self.protocol))
            .map(|i| i as isize)
            .unwrap_or(0);
        let next = (cur + delta).rem_euclid(n) as usize;
        self.protocol = TUI_FORM_PROTOCOLS[next].to_string();
    }

    /// The display string for a field; the password is rendered as bullets.
    pub fn field_display(&self, kind: ProfileFieldKind) -> String {
        match kind {
            ProfileFieldKind::Name => self.name.clone(),
            ProfileFieldKind::Protocol => self.protocol.clone(),
            ProfileFieldKind::Host => self.host.clone(),
            ProfileFieldKind::Port => self.port.clone(),
            ProfileFieldKind::Username => self.username.clone(),
            ProfileFieldKind::InitialPath => self.initial_path.clone(),
            ProfileFieldKind::LocalPath => self.local_path.clone(),
            ProfileFieldKind::Password => "\u{2022}".repeat(self.password.len()),
        }
    }
}

/// A single-line text prompt (create directory, rename, transfer paths).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PromptState {
    pub kind: PromptKind,
    pub title: String,
    pub hint: String,
    pub buffer: String,
}

impl PromptState {
    pub fn new(
        kind: PromptKind,
        title: impl Into<String>,
        hint: impl Into<String>,
        initial: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            title: title.into(),
            hint: hint.into(),
            buffer: initial.into(),
        }
    }

    /// Append a printable character, rejecting control characters and the path
    /// separators that would let a single-segment prompt escape its directory.
    pub fn push_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        if matches!(
            self.kind,
            PromptKind::Mkdir { .. } | PromptKind::Rename { .. }
        ) && matches!(c, '/' | '\\')
        {
            return;
        }
        self.buffer.push(c);
    }

    pub fn backspace(&mut self) {
        self.buffer.pop();
    }

    pub fn trimmed(&self) -> &str {
        self.buffer.trim()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PromptKind {
    /// Create a directory named by the buffer inside `parent`.
    Mkdir { parent: String },
    /// Rename `from` to a sibling named by the buffer.
    Rename { from: String },
    /// Download `remote` to the local path in the buffer.
    Download { remote: String },
    /// Upload the local path in the buffer into `remote_dir`.
    Upload { remote_dir: String },
    /// Name a group: create a new one seeded with `profile_id`, or rename the
    /// group named `rename_from` when present (#320). Buffer holds the name.
    GroupName {
        profile_id: String,
        rename_from: Option<String>,
    },
}

/// A yes/no confirmation guarding a destructive action.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ConfirmState {
    pub kind: ConfirmKind,
    pub message: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ConfirmKind {
    /// Delete `path`; `recursive` when the entry is a non-empty directory.
    Delete { path: String, recursive: bool },
    /// Delete a saved profile (B4 DiscoveryHub) from `user_name`'s partition.
    /// Carries the display name for the confirmation message and the id for the
    /// optimistic removal + worker `DeleteProfile`.
    DeleteProfile {
        user_name: String,
        profile_id: String,
        profile_name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mkdir_prompt_rejects_separators_and_control_chars() {
        let mut prompt = PromptState::new(
            PromptKind::Mkdir {
                parent: "/srv".to_string(),
            },
            "New folder",
            "name",
            String::new(),
        );
        for c in "ab/c\\d\n".chars() {
            prompt.push_char(c);
        }
        assert_eq!(prompt.buffer, "abcd");
    }

    #[test]
    fn download_prompt_keeps_local_separators() {
        let mut prompt = PromptState::new(
            PromptKind::Download {
                remote: "/srv/file.txt".to_string(),
            },
            "Download",
            "local path",
            String::new(),
        );
        for c in "./out/file.txt".chars() {
            prompt.push_char(c);
        }
        assert_eq!(prompt.buffer, "./out/file.txt");
    }

    #[test]
    fn backspace_and_trim_behave_like_a_line_editor() {
        let mut prompt = PromptState::new(
            PromptKind::Rename {
                from: "/srv/a.txt".to_string(),
            },
            "Rename",
            "new name",
            "a.txt".to_string(),
        );
        prompt.backspace();
        assert_eq!(prompt.buffer, "a.tx");
        prompt.buffer = "  spaced  ".to_string();
        assert_eq!(prompt.trimmed(), "spaced");
    }
}
