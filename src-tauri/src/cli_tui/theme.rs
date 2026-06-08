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
    pub fn muted_style(self) -> Style {
        Style::default().fg(self.muted)
    }

    pub fn accent_style(self) -> Style {
        Style::default().fg(self.accent)
    }
}
