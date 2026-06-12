use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TuiAction {
    Quit,
    /// Esc: contextual "back". Disconnects to the IntroHub when a live session
    /// is connected; quits the app from the IntroHub. Resolved in `AppState`.
    Back,
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    Activate,
    Parent,
    /// Open the "create directory" prompt for the current browser path.
    NewDir,
    /// Confirm-then-delete the selected browser entry, or drop a finished
    /// transfer when the Transfers pane is focused.
    Delete,
    /// Open the "rename" prompt for the selected browser entry.
    Rename,
    /// Open the "download" prompt for the selected remote file.
    Download,
    /// Open the "upload" prompt targeting the current browser directory.
    Upload,
    /// Cancel the in-flight worker operation (used by long transfers).
    CancelOp,
    /// Clear every finished transfer from the queue (Transfers pane), discarding
    /// their resumable `.aerotmp` leftovers.
    ClearTransfers,
    /// Toggle reveal/mask of sensitive credential values (profile username / auth id)
    /// in the Profiles pane and Intent preview. Session-only (never persisted).
    /// Default: masked. Key 's'/'S'.
    ToggleShowCredentials,
    /// When focus is on the dual browser area (live), Tab switches between Local and Remote pane.
    /// Left/Right still move global focus (e.g. Browser <-> Transfers).
    SwitchBrowserSide,
    /// IntroHub: toggle the favorite flag of the highlighted saved profile,
    /// persisted via the worker (`f`). No-op in the connected browser.
    ToggleFavorite,
    /// IntroHub: probe reachability (health) of the highlighted profile (`H`).
    HealthCheck,
    /// IntroHub: refresh storage quota of the highlighted profile via a
    /// transient connection (`Q`).
    RefreshQuota,
    /// IntroHub: open the named-group manager for the highlighted profile (`G`),
    /// the generalisation of the `f` favourite toggle. No-op in the browser.
    ManageGroups,
    /// Open the command palette (`:`) for line-mode dispatch against the live
    /// session. Connected only.
    OpenPalette,
    /// IntroHub: open the DiscoveryHub form to add a new saved profile (`a`).
    AddProfile,
    /// IntroHub: open the DiscoveryHub form to edit the highlighted profile (`e`).
    EditProfile,
    /// IntroHub: confirm-then-delete the highlighted saved profile (`x`).
    DeleteProfile,
    /// Connected browser: cycle the active pane's sort (name/size/date/type,
    /// asc/desc) (`B`). No-op on the IntroHub.
    CycleSort,
    /// Connected browser: open the live in-list filter prompt for the active
    /// pane (`/`). No-op on the IntroHub.
    OpenFilter,
    /// Connected browser: reload the active pane's directory, dropping marks and
    /// any filter (`L`). No-op on the IntroHub.
    Reload,
    /// Connected browser: toggle the mark on the selected entry (`Space`/`m`).
    MarkToggle,
    /// Connected browser: mark every visible entry in the active pane (`Ctrl+A`).
    MarkAll,
    /// Connected browser: clear every mark in the active pane (`Alt+A`).
    MarkNone,
    /// Connected browser: view the selected file in a read-only pager (`v`).
    ViewFile,
    /// Connected browser: edit the selected file in `$EDITOR` (`o`).
    EditFile,
    /// Connected browser: show full metadata for the selected entry (`i`).
    Info,
    /// Connected browser: recursive size of the selected directory (`Ctrl+S`).
    SizeRecursive,
    /// Connected browser: create an empty file in the active directory (`N`).
    Touch,
    /// Connected browser: toggle synced browsing so both panes `cd` together
    /// (`Y`).
    SyncedBrowsing,
    /// Open the full help overlay (`?`/`F1`).
    Help,
    Noop,
}

/// Translate a key press into a dashboard action.
///
/// This mapping is intentionally context-free: it never inspects the focused
/// pane or any overlay state. The [`crate::cli_tui::app::AppState`] decides
/// whether a given action is meaningful for the current focus, which keeps the
/// translation pure and unit-testable. Overlay (text-entry/confirm) keys are
/// handled separately by [`key_to_overlay`].
pub fn key_to_action(key: KeyEvent) -> TuiAction {
    // Modifier combos first (rev 1.0.3 table stakes). Only the specific Ctrl/Alt
    // shortcuts are intercepted; every other key falls through to the plain
    // mapping below, which ignores modifiers (so Ctrl+c still cancels, etc).
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('a') | KeyCode::Char('A') => return TuiAction::MarkAll,
            KeyCode::Char('s') | KeyCode::Char('S') => return TuiAction::SizeRecursive,
            KeyCode::Char('r') | KeyCode::Char('R') => return TuiAction::Reload,
            _ => {}
        }
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        if let KeyCode::Char('a') | KeyCode::Char('A') = key.code {
            return TuiAction::MarkNone;
        }
    }
    match key.code {
        KeyCode::Char('q') => TuiAction::Quit,
        // Esc is contextual "back": disconnect to the IntroHub when connected,
        // quit the app from the IntroHub. AppState decides (see TuiAction::Back).
        KeyCode::Esc => TuiAction::Back,
        KeyCode::Up | KeyCode::Char('k') => TuiAction::MoveUp,
        KeyCode::Down | KeyCode::Char('j') => TuiAction::MoveDown,
        KeyCode::Left | KeyCode::Char('h') => TuiAction::MoveLeft,
        KeyCode::Right | KeyCode::Char('l') => TuiAction::MoveRight,
        KeyCode::Tab => TuiAction::SwitchBrowserSide,
        KeyCode::Enter => TuiAction::Activate,
        KeyCode::Backspace => TuiAction::Parent,
        KeyCode::Char('n') => TuiAction::NewDir,
        KeyCode::Char('d') | KeyCode::Delete => TuiAction::Delete,
        KeyCode::Char('D') => TuiAction::ClearTransfers,
        KeyCode::Char('r') => TuiAction::Rename,
        KeyCode::Char('g') => TuiAction::Download,
        KeyCode::Char('p') => TuiAction::Upload,
        KeyCode::Char('c') => TuiAction::CancelOp,
        KeyCode::Char('f') | KeyCode::Char('F') => TuiAction::ToggleFavorite,
        KeyCode::Char('H') => TuiAction::HealthCheck,
        KeyCode::Char('Q') => TuiAction::RefreshQuota,
        KeyCode::Char('G') => TuiAction::ManageGroups,
        KeyCode::Char('s') | KeyCode::Char('S') => TuiAction::ToggleShowCredentials,
        KeyCode::Char(':') => TuiAction::OpenPalette,
        KeyCode::Char('a') | KeyCode::Char('A') => TuiAction::AddProfile,
        KeyCode::Char('e') | KeyCode::Char('E') => TuiAction::EditProfile,
        KeyCode::Char('x') | KeyCode::Char('X') => TuiAction::DeleteProfile,
        // Connected-browser table stakes (rev 1.0.3). These keys are free in the
        // browser; AppState makes them no-ops on the IntroHub.
        KeyCode::Char('B') | KeyCode::Char('b') => TuiAction::CycleSort,
        KeyCode::Char('/') => TuiAction::OpenFilter,
        KeyCode::Char('L') => TuiAction::Reload,
        KeyCode::Char(' ') | KeyCode::Char('m') | KeyCode::Char('M') => TuiAction::MarkToggle,
        KeyCode::Char('v') | KeyCode::Char('V') => TuiAction::ViewFile,
        KeyCode::Char('o') | KeyCode::Char('O') => TuiAction::EditFile,
        KeyCode::Char('i') | KeyCode::Char('I') => TuiAction::Info,
        KeyCode::Char('N') => TuiAction::Touch,
        KeyCode::Char('Y') | KeyCode::Char('y') => TuiAction::SyncedBrowsing,
        KeyCode::Char('?') | KeyCode::F(1) => TuiAction::Help,
        _ => TuiAction::Noop,
    }
}

/// A key press routed to an active overlay (text prompt or confirmation).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum OverlayKey {
    /// A printable character typed into a text prompt.
    Char(char),
    /// Delete the last character of a text prompt.
    Backspace,
    /// Commit the overlay (Enter / `y` on a confirmation).
    Submit,
    /// Dismiss the overlay without acting (Esc / `n` on a confirmation).
    Cancel,
    /// Move a menu cursor up (Up arrow). Menu overlays only; ignored by prompts.
    Up,
    /// Move a menu cursor down (Down arrow). Menu overlays only; ignored by prompts.
    Down,
    /// Move left (Left arrow). Cycles the profile-form protocol field (B4);
    /// ignored by text prompts and menus.
    Left,
    /// Move right (Right arrow). Cycles the profile-form protocol field (B4);
    /// ignored by text prompts and menus.
    Right,
    /// Advance to the next field (Tab). Used by the profile form (B4); ignored
    /// by text prompts and menus.
    Tab,
    /// Scroll a screenful up (PageUp). Pager/help overlays only.
    PageUp,
    /// Scroll a screenful down (PageDown). Pager/help overlays only.
    PageDown,
    /// Jump to the start (Home). Pager/help overlays only.
    Home,
    /// Jump to the end (End). Pager/help overlays only.
    End,
    /// A key with no overlay meaning.
    Noop,
}

/// Translate a key press while an overlay is active.
///
/// Overlays own the keyboard while visible, so the ambiguous dashboard keys
/// (Enter, Esc, Backspace, printable characters) are reinterpreted here rather
/// than in [`key_to_action`].
pub fn key_to_overlay(key: KeyEvent) -> OverlayKey {
    match key.code {
        KeyCode::Enter => OverlayKey::Submit,
        KeyCode::Esc => OverlayKey::Cancel,
        KeyCode::Backspace => OverlayKey::Backspace,
        KeyCode::Tab => OverlayKey::Tab,
        KeyCode::Up => OverlayKey::Up,
        KeyCode::Down => OverlayKey::Down,
        KeyCode::Left => OverlayKey::Left,
        KeyCode::Right => OverlayKey::Right,
        KeyCode::PageUp => OverlayKey::PageUp,
        KeyCode::PageDown => OverlayKey::PageDown,
        KeyCode::Home => OverlayKey::Home,
        KeyCode::End => OverlayKey::End,
        KeyCode::Char(c) => OverlayKey::Char(c),
        _ => OverlayKey::Noop,
    }
}
