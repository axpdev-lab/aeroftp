use ratatui::style::{Color, Style};

#[derive(Debug, Clone, Copy)]
pub struct TuiTheme {
    pub accent: Color,
    pub muted: Color,
    pub selection: Color,
    pub ready: Color,
    pub planned: Color,
}

impl Default for TuiTheme {
    fn default() -> Self {
        Self {
            accent: Color::Cyan,
            muted: Color::DarkGray,
            selection: Color::Blue,
            ready: Color::Green,
            planned: Color::Yellow,
        }
    }
}

impl TuiTheme {
    /// The colour theme honouring `NO_COLOR` (https://no-color.org/, matching the
    /// CLI's `use_color`): any presence of the variable degrades the TUI to a
    /// monochrome palette that relies on bold/reverse instead of colour. The TUI
    /// only runs on a TTY, so the TTY check `use_color` also performs is moot.
    pub fn from_env() -> Self {
        if std::env::var_os("NO_COLOR").is_some() {
            Self::monochrome()
        } else {
            Self::default()
        }
    }

    /// A no-colour palette: every accent collapses to the terminal default, so
    /// distinction comes from the bold/reverse modifiers the widgets already add.
    pub fn monochrome() -> Self {
        Self {
            accent: Color::Reset,
            muted: Color::Reset,
            selection: Color::Reset,
            ready: Color::Reset,
            planned: Color::Reset,
        }
    }

    pub fn muted_style(self) -> Style {
        Style::default().fg(self.muted)
    }

    pub fn accent_style(self) -> Style {
        Style::default().fg(self.accent)
    }
}
