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
    /// Read-only file viewer (`v`): a scrollable pager over a downloaded/streamed
    /// file, binary-safe. Pure view state; the bytes are read by the worker and
    /// handed over as already-decoded lines.
    Pager(PagerState),
    /// Full key-reference help overlay (`?`/`F1`): a scrollable list of every
    /// key binding. Static content; pure scroll state.
    Help(HelpState),
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

/// Read-only pager state for the file viewer (`v`). The worker decodes the file
/// into display lines (binary files become a short notice); the overlay only
/// tracks the scroll offset. `viewport` is the last-rendered visible height, kept
/// so paging keys move by a screenful.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct PagerState {
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
    pub truncated: bool,
    pub binary: bool,
    pub viewport: usize,
}

impl PagerState {
    pub fn new(title: String, lines: Vec<String>, truncated: bool, binary: bool) -> Self {
        Self {
            title,
            lines,
            scroll: 0,
            truncated,
            binary,
            viewport: 1,
        }
    }

    /// Largest valid scroll offset given the current viewport, so the last line
    /// can reach the bottom without scrolling past the end.
    fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.viewport.max(1))
    }

    pub fn scroll_down(&mut self, delta: usize) {
        self.scroll = (self.scroll + delta).min(self.max_scroll());
    }

    pub fn scroll_up(&mut self, delta: usize) {
        self.scroll = self.scroll.saturating_sub(delta);
    }

    pub fn page_down(&mut self) {
        self.scroll_down(self.viewport.max(1));
    }

    pub fn page_up(&mut self) {
        self.scroll_up(self.viewport.max(1));
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll = self.max_scroll();
    }
}

/// Help overlay state (`?`/`F1`): a scrollable static key reference. Like the
/// pager it only carries a scroll offset and the last-rendered viewport height.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct HelpState {
    pub scroll: usize,
    pub viewport: usize,
}

impl HelpState {
    fn max_scroll(&self) -> usize {
        HELP_LINES.len().saturating_sub(self.viewport.max(1))
    }

    pub fn scroll_down(&mut self, delta: usize) {
        self.scroll = (self.scroll + delta).min(self.max_scroll());
    }

    pub fn scroll_up(&mut self, delta: usize) {
        self.scroll = self.scroll.saturating_sub(delta);
    }
}

/// The full key reference shown by the help overlay. Single source of truth so
/// the overlay and any future docs stay in sync. No emojis / em-dashes.
pub const HELP_LINES: &[&str] = &[
    "AeroFTP TUI - key reference",
    "",
    "IntroHub (pre-connect)",
    "  Up/Down      move the highlighted profile",
    "  Left/Right   switch user",
    "  Enter        connect to the profile",
    "  f            toggle favorite",
    "  G            manage groups (filter / membership)",
    "  a / e / x    add / edit / delete a profile",
    "  H            health probe        Q  refresh quota",
    "  s            reveal credentials (session only)",
    "",
    "Connected browser",
    "  Tab          switch active pane (Local / Remote)",
    "  Up/Down      move selection      Enter  open dir / stat file",
    "  Bksp         parent directory",
    "  g / p        download / upload",
    "  n / N        new folder / new empty file",
    "  r            rename             d  delete",
    "  v            view file          o  edit in $EDITOR",
    "  i            file info          Ctrl+S  recursive size",
    "  B            cycle sort         /  filter listing",
    "  a            toggle hidden      L / Ctrl+R  reload",
    "  Space / m    mark entry         Ctrl+A / Alt+A  mark all / none",
    "  G            go to path         Y  synced browsing",
    "  :            command palette    c  cancel   D  clear transfers",
    "  Esc          disconnect to IntroHub",
    "",
    "General",
    "  ?  / F1      this help          q  quit",
];

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

    /// Protocol-aware label: the generic Host/Username/Password fields map onto
    /// protocol-specific concepts (S3 access keys, WebDAV URL), so the form shows
    /// the right name instead of a misleading generic one. The stored value is the
    /// same; only the label changes (option A - the rich per-protocol form with
    /// bucket/region is the B roadmap).
    pub fn label_for(self, protocol: &str) -> &'static str {
        match (self, protocol) {
            (ProfileFieldKind::Host, "s3") => "Endpoint",
            (ProfileFieldKind::Username, "s3") => "Access Key",
            (ProfileFieldKind::Password, "s3") => "Secret Key",
            (ProfileFieldKind::Host, "webdav") => "URL",
            _ => self.label(),
        }
    }
}

/// A one-line note for protocols whose full configuration cannot be expressed in
/// this simple form, shown under the fields so the form is honest about its
/// scope (option A). The generic fields still edit safely (a rename preserves the
/// stored advanced options); the GUI remains the place for the rest.
pub fn protocol_form_note(protocol: &str) -> Option<&'static str> {
    if protocol == "s3" {
        return Some("Advanced fields (bucket, region, endpoint) are set in the GUI.");
    }
    // Anything outside the basic, form-editable set (FTP/FTPS/SFTP/WebDAV/S3) is
    // an OAuth/token protocol that cannot be created from a plain form.
    if !TUI_FORM_PROTOCOLS.contains(&protocol) {
        return Some("OAuth/token protocol: create or fully edit it in the GUI.");
    }
    None
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

/// A single-line text prompt (create directory, rename, transfer paths). A
/// minimal line editor: a `cursor` (char index, 0..=len) supports inserting and
/// deleting in the middle of the buffer, with Left/Right to move it.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PromptState {
    pub kind: PromptKind,
    pub title: String,
    pub hint: String,
    pub buffer: String,
    /// Insertion point as a char index into `buffer` (0..=char count).
    pub cursor: usize,
}

impl PromptState {
    pub fn new(
        kind: PromptKind,
        title: impl Into<String>,
        hint: impl Into<String>,
        initial: impl Into<String>,
    ) -> Self {
        let buffer = initial.into();
        let cursor = buffer.chars().count();
        Self {
            kind,
            title: title.into(),
            hint: hint.into(),
            buffer,
            cursor,
        }
    }

    fn len_chars(&self) -> usize {
        self.buffer.chars().count()
    }

    /// Byte offset of char index `cursor`, or the buffer length when at the end.
    fn byte_at(&self, cursor: usize) -> usize {
        self.buffer
            .char_indices()
            .nth(cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.buffer.len())
    }

    /// Insert a printable character at the cursor, rejecting control characters
    /// and the path separators that would let a single-segment prompt escape its
    /// directory. Advances the cursor past the inserted character.
    pub fn push_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        if matches!(
            self.kind,
            PromptKind::Mkdir { .. } | PromptKind::Rename { .. } | PromptKind::Touch { .. }
        ) && matches!(c, '/' | '\\')
        {
            return;
        }
        self.cursor = self.cursor.min(self.len_chars());
        let byte = self.byte_at(self.cursor);
        self.buffer.insert(byte, c);
        self.cursor += 1;
    }

    /// Delete the character before the cursor (Backspace).
    pub fn backspace(&mut self) {
        self.cursor = self.cursor.min(self.len_chars());
        if self.cursor == 0 {
            return;
        }
        let byte = self.byte_at(self.cursor - 1);
        self.buffer.remove(byte);
        self.cursor -= 1;
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.min(self.len_chars()).saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len_chars());
    }

    /// Split the buffer for rendering a block cursor: `(before, at, after)` where
    /// `at` is the single character under the cursor (a space when at the end).
    pub fn cursor_split(&self) -> (String, String, String) {
        let chars: Vec<char> = self.buffer.chars().collect();
        let cursor = self.cursor.min(chars.len());
        let before: String = chars[..cursor].iter().collect();
        let at: String = chars
            .get(cursor)
            .map(|c| c.to_string())
            .unwrap_or_else(|| " ".to_string());
        let after: String = if cursor < chars.len() {
            chars[cursor + 1..].iter().collect()
        } else {
            String::new()
        };
        (before, at, after)
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
    /// Live filter (`/`): the buffer narrows the active pane's listing as a
    /// case-insensitive substring or `*`/`?` wildcard. `local` records which pane
    /// the filter was raised on so submit/clear act on the right side.
    Filter { local: bool },
    /// Create an empty file (`N`) named by the buffer inside `parent`. `local`
    /// routes the touch to the local filesystem instead of the remote provider.
    Touch { parent: String, local: bool },
    /// Go to a path (`G` in the browser): jump the active pane to the directory
    /// in the buffer. `local` selects the pane; the path is resolved by AppState.
    Goto { local: bool },
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
    /// `local` routes the delete to the local filesystem (Local pane) instead of
    /// the remote provider - carried explicitly because a delete can be raised
    /// from either the active pane or the (always-remote) palette `rm`.
    Delete {
        path: String,
        recursive: bool,
        local: bool,
    },
    /// Delete a saved profile (B4 DiscoveryHub) from `user_name`'s partition.
    /// Carries the display name for the confirmation message and the id for the
    /// optimistic removal + worker `DeleteProfile`.
    DeleteProfile {
        user_name: String,
        profile_id: String,
        profile_name: String,
    },
    /// Delete every marked entry in a pane (multi-select batch). Each item is
    /// `(path, is_dir)`; `local` routes the whole batch to the local filesystem
    /// or the remote provider.
    DeleteBatch {
        items: Vec<(String, bool)>,
        local: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s3_form_uses_protocol_aware_labels_and_a_note() {
        // S3 relabels the generic fields to S3 concepts; the value is unchanged.
        assert_eq!(ProfileFieldKind::Host.label_for("s3"), "Endpoint");
        assert_eq!(ProfileFieldKind::Username.label_for("s3"), "Access Key");
        assert_eq!(ProfileFieldKind::Password.label_for("s3"), "Secret Key");
        // A basic protocol keeps the generic labels.
        assert_eq!(ProfileFieldKind::Host.label_for("ftp"), "Host");
        assert_eq!(ProfileFieldKind::Username.label_for("sftp"), "Username");

        // Notes: S3 points advanced fields to the GUI; OAuth protocols too; basic
        // protocols have no note.
        assert!(protocol_form_note("s3").unwrap().contains("bucket"));
        assert!(protocol_form_note("onedrive").is_some());
        assert!(protocol_form_note("ftp").is_none());
        assert!(protocol_form_note("webdav").is_none());
    }

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
    fn prompt_cursor_inserts_and_deletes_mid_string() {
        let mut prompt = PromptState::new(
            PromptKind::Download {
                remote: "/srv/file.txt".to_string(),
            },
            "Download",
            "local path",
            "abcd".to_string(),
        );
        // Cursor starts at the end.
        assert_eq!(prompt.cursor, 4);
        // Move left twice -> between 'b' and 'c', insert 'X'.
        prompt.move_left();
        prompt.move_left();
        prompt.push_char('X');
        assert_eq!(prompt.buffer, "abXcd");
        assert_eq!(prompt.cursor, 3);
        // Backspace deletes the char before the cursor ('X'), not the end.
        prompt.backspace();
        assert_eq!(prompt.buffer, "abcd");
        assert_eq!(prompt.cursor, 2);
        // Right to the end; move_right clamps at the length.
        prompt.move_right();
        prompt.move_right();
        prompt.move_right();
        assert_eq!(prompt.cursor, 4);
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
