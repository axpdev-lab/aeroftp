use crossterm::event::{KeyCode, KeyEvent};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TuiAction {
    Quit,
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
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => TuiAction::Quit,
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
        KeyCode::Char('u') => TuiAction::Upload,
        KeyCode::Char('c') => TuiAction::CancelOp,
        KeyCode::Char('f') | KeyCode::Char('F') => TuiAction::ToggleFavorite,
        KeyCode::Char('H') => TuiAction::HealthCheck,
        KeyCode::Char('s') | KeyCode::Char('S') => TuiAction::ToggleShowCredentials,
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
        KeyCode::Char(c) => OverlayKey::Char(c),
        _ => OverlayKey::Noop,
    }
}
