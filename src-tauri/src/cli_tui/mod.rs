use std::{io, io::IsTerminal, time::Duration};

use crossterm::{
    event::{self as crossterm_event, Event, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};

use self::{
    app::{AppState, TUI_MENU_ITEMS},
    event::key_to_action,
    theme::TuiTheme,
};

pub mod app;
pub mod event;
pub mod panes;
pub mod theme;
pub mod worker;

pub use app::TuiIntent;

pub type CliTuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

pub fn run_tui() -> io::Result<TuiIntent> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "tui requires an interactive terminal",
        ));
    }

    let mut app = AppState::new();
    let theme = TuiTheme::default();

    with_terminal(|terminal| {
        loop {
            terminal.draw(|frame| render_dashboard(frame, &app, theme))?;

            if app.should_quit {
                break;
            }

            if crossterm_event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = crossterm_event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    app.apply_action(key_to_action(key));
                    if app.should_quit {
                        break;
                    }
                }
            }
        }

        Ok(app.take_intent().unwrap_or(TuiIntent::Quit))
    })
}

fn render_dashboard(frame: &mut ratatui::Frame<'_>, app: &AppState, theme: TuiTheme) {
    let area = frame.area();
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(3),
    ])
    .split(area);

    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                " AeroFTP TUI",
                theme.accent_style().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("Phase 1", theme.muted_style()),
        ]),
        Line::from(Span::styled(
            "Picker-pattern front-end: intent is routed back through CLI handlers.",
            theme.muted_style(),
        )),
    ]);
    frame.render_widget(header, rows[0]);

    let body_direction = if area.width >= 96 {
        Direction::Horizontal
    } else {
        Direction::Vertical
    };
    let body = Layout::new(
        body_direction,
        [Constraint::Percentage(40), Constraint::Percentage(60)],
    )
    .split(rows[1]);

    let items: Vec<ListItem> = TUI_MENU_ITEMS
        .iter()
        .map(|item| {
            let phase_style = if item.intent.is_some() {
                Style::default().fg(theme.ready)
            } else {
                Style::default().fg(theme.planned)
            };
            ListItem::new(Line::from(vec![
                Span::styled(item.title, Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(item.phase, phase_style),
            ]))
        })
        .collect();
    let mut list_state = ListState::default();
    list_state.select(Some(app.selected));
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Actions "))
        .highlight_style(
            Style::default()
                .bg(theme.selection)
                .fg(ratatui::style::Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, body[0], &mut list_state);

    let selected = app.selected_item();
    let details = Paragraph::new(vec![
        Line::from(Span::styled(
            selected.title,
            theme.accent_style().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(selected.description),
        Line::from(""),
        Line::from(vec![
            Span::styled("Command: ", theme.muted_style()),
            Span::raw(selected.command),
        ]),
        Line::from(vec![
            Span::styled("Status:  ", theme.muted_style()),
            Span::raw(selected.phase),
        ]),
        Line::from(""),
        Line::from(Span::styled(app.pane_summary(), theme.muted_style())),
    ])
    .block(Block::default().borders(Borders::ALL).title(" Intent "))
    .wrap(Wrap { trim: true });
    frame.render_widget(details, body[1]);

    let footer = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(" Up/Down", theme.accent_style()),
            Span::raw(" move   "),
            Span::styled("Enter", theme.accent_style()),
            Span::raw(" activate ready action   "),
            Span::styled("q/Esc", theme.accent_style()),
            Span::raw(" quit"),
        ]),
        Line::from(Span::styled(app.status.as_str(), theme.muted_style())),
    ]);
    frame.render_widget(footer, rows[2]);
}

/// Run a ratatui surface in raw-mode alternate-screen mode.
///
/// The restore path is deliberately centralized because every interactive
/// surface must leave the user's terminal usable even when drawing or input
/// handling returns an error.
pub fn with_terminal<R>(run: impl FnOnce(&mut CliTuiTerminal) -> io::Result<R>) -> io::Result<R> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;

    if let Err(err) = stdout.execute(EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(err);
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(err) => {
            let _ = disable_raw_mode();
            let _ = io::stdout().execute(LeaveAlternateScreen);
            return Err(err);
        }
    };

    let run_result = run(&mut terminal);
    let restore_result = restore_terminal(&mut terminal);

    match run_result {
        Ok(value) => {
            restore_result?;
            Ok(value)
        }
        Err(err) => {
            let _ = restore_result;
            Err(err)
        }
    }
}

fn restore_terminal(terminal: &mut CliTuiTerminal) -> io::Result<()> {
    let raw_result = disable_raw_mode();
    let screen_result = terminal
        .backend_mut()
        .execute(LeaveAlternateScreen)
        .map(|_| ());
    let cursor_result = terminal.show_cursor();

    raw_result?;
    screen_result?;
    cursor_result
}
