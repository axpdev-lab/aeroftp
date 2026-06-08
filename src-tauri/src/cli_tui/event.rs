use crossterm::event::{KeyCode, KeyEvent};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TuiAction {
    Quit,
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    Activate,
    Noop,
}

pub fn key_to_action(key: KeyEvent) -> TuiAction {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => TuiAction::Quit,
        KeyCode::Up | KeyCode::Char('k') => TuiAction::MoveUp,
        KeyCode::Down | KeyCode::Char('j') => TuiAction::MoveDown,
        KeyCode::Left | KeyCode::Char('h') => TuiAction::MoveLeft,
        KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => TuiAction::MoveRight,
        KeyCode::Enter => TuiAction::Activate,
        _ => TuiAction::Noop,
    }
}
