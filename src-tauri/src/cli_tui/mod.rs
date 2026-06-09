use std::{io, io::IsTerminal, time::Duration};

use crossterm::{
    event::{self as crossterm_event, Event, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};

use self::{
    app::{AppState, TuiActionIntent, TuiFocus, TUI_ACTION_ITEMS},
    event::key_to_action,
    theme::TuiTheme,
    worker::{TuiWorkerClient, WorkerEvent},
};

pub mod app;
pub mod event;
pub mod panes;
pub mod session;
pub mod theme;
pub mod worker;

pub use app::{TuiContext, TuiIntent, TuiProfile, TuiProfileAction, TuiUser};

pub type CliTuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

#[allow(dead_code)]
pub fn run_tui(context: TuiContext) -> io::Result<TuiIntent> {
    run_tui_inner(context, None)
}

pub fn run_tui_with_worker(context: TuiContext, worker: TuiWorkerClient) -> io::Result<TuiIntent> {
    run_tui_inner(context, Some(worker))
}

fn run_tui_inner(context: TuiContext, worker: Option<TuiWorkerClient>) -> io::Result<TuiIntent> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "tui requires an interactive terminal",
        ));
    }

    let mut app = if worker.is_some() {
        AppState::new_live(context)
    } else {
        AppState::new(context)
    };
    let mut worker = worker;
    let theme = TuiTheme::default();

    with_terminal(|terminal| {
        loop {
            drain_worker_events(&mut app, &mut worker);
            terminal.draw(|frame| render_dashboard(frame, &app, theme))?;

            if app.should_quit {
                break;
            }

            if crossterm_event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = crossterm_event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    let commands = app.apply_action(key_to_action(key));
                    if !commands.is_empty() {
                        if let Some(worker) = worker.as_ref() {
                            for command in commands {
                                if worker.commands.send(command).is_err() {
                                    app.apply_worker_event(WorkerEvent::Failed {
                                        operation:
                                            crate::cli_tui::worker::TuiWorkerOperation::Connect,
                                        identity: None,
                                        message: "worker channel closed".to_string(),
                                    });
                                    break;
                                }
                            }
                        }
                    }
                    if app.should_quit {
                        break;
                    }
                }
            }
        }

        Ok(app.take_intent().unwrap_or(TuiIntent::Quit))
    })
}

fn drain_worker_events(app: &mut AppState, worker: &mut Option<TuiWorkerClient>) {
    let Some(client) = worker.as_mut() else {
        return;
    };

    loop {
        match client.events.try_recv() {
            Ok(event) => app.apply_worker_event(event),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                app.apply_worker_event(WorkerEvent::Failed {
                    operation: crate::cli_tui::worker::TuiWorkerOperation::Connect,
                    identity: None,
                    message: "worker stopped".to_string(),
                });
                *worker = None;
                break;
            }
        }
    }
}

fn render_dashboard(frame: &mut ratatui::Frame<'_>, app: &AppState, theme: TuiTheme) {
    let area = frame.area();
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(12),
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
            Span::styled(app.phase_label(), theme.muted_style()),
        ]),
        Line::from(Span::styled(
            "User -> profile -> action. The selected intent exits raw mode and runs through CLI handlers.",
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
        [
            Constraint::Percentage(28),
            Constraint::Percentage(34),
            Constraint::Percentage(38),
        ],
    )
    .split(rows[1]);

    render_users(frame, body[0], app, theme);
    render_profiles(frame, body[1], app, theme);
    render_actions(frame, body[2], app, theme);

    let footer = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(" Up/Down", theme.accent_style()),
            Span::raw(" move   "),
            Span::styled("Left/Right/Tab", theme.accent_style()),
            Span::raw(" pane   "),
            Span::styled("Enter", theme.accent_style()),
            Span::raw(" run   "),
            Span::styled("q/Esc", theme.accent_style()),
            Span::raw(" quit"),
        ]),
        Line::from(Span::styled(app.status.as_str(), theme.muted_style())),
    ]);
    frame.render_widget(footer, rows[2]);
}

fn render_users(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState, theme: TuiTheme) {
    let items: Vec<ListItem> = app
        .context
        .users
        .iter()
        .map(|user| {
            let lock = if user.is_locked { "locked" } else { "open" };
            let active = if user.is_active { " active" } else { "" };
            let admin = if user.is_admin { " admin" } else { "" };
            ListItem::new(Line::from(vec![
                Span::styled(format!("#{} ", user.id), theme.muted_style()),
                Span::styled(&user.name, Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(lock, lock_style(user.is_locked, theme)),
                Span::styled(active, theme.muted_style()),
                Span::styled(admin, theme.muted_style()),
                Span::styled(
                    format!("  {} profile(s)", user.profile_count),
                    theme.muted_style(),
                ),
            ]))
        })
        .collect();
    let title = if matches!(app.focus, TuiFocus::Users) {
        " Users * "
    } else {
        " Users "
    };
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(app.selected_user));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(selection_style(app.focus, TuiFocus::Users, theme))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_profiles(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState, theme: TuiTheme) {
    let user = app.selected_user();
    let items: Vec<ListItem> = user
        .map(|user| {
            if user.is_locked {
                vec![ListItem::new(Line::from(vec![
                    Span::styled("Locked user", Style::default().fg(theme.planned)),
                    Span::raw("  "),
                    Span::styled("Enter unlocks via CLI prompt", theme.muted_style()),
                ]))]
            } else if user.profiles.is_empty() {
                vec![ListItem::new(Line::from(Span::styled(
                    "No profiles for this user",
                    theme.muted_style(),
                )))]
            } else {
                user.profiles
                    .iter()
                    .map(|profile| {
                        let fav = if profile.favorite { "*" } else { " " };
                        ListItem::new(Line::from(vec![
                            Span::styled(format!("{:>2}.", profile.selector), theme.muted_style()),
                            Span::raw(fav),
                            Span::styled(
                                &profile.name,
                                Style::default().add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                format!("  {}", profile.protocol),
                                Style::default().fg(theme.accent),
                            ),
                            Span::styled(format!("  {}", profile.host), theme.muted_style()),
                        ]))
                    })
                    .collect()
            }
        })
        .unwrap_or_else(|| {
            vec![ListItem::new(Line::from(Span::styled(
                "No user selected",
                theme.muted_style(),
            )))]
        });

    let title = if matches!(app.focus, TuiFocus::Profiles) {
        " Profiles * "
    } else {
        " Profiles "
    };
    let mut state = ListState::default();
    if user
        .map(|u| !u.is_locked && !u.profiles.is_empty())
        .unwrap_or(false)
    {
        state.select(Some(app.selected_profile));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(selection_style(app.focus, TuiFocus::Profiles, theme))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_actions(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState, theme: TuiTheme) {
    let chunks = Layout::vertical([Constraint::Min(7), Constraint::Length(8)]).split(area);

    let items: Vec<ListItem> = TUI_ACTION_ITEMS
        .iter()
        .map(|item| {
            let phase_style = match item.intent {
                TuiActionIntent::ProfilesInteractive | TuiActionIntent::Profile(_) => {
                    Style::default().fg(theme.ready)
                }
                TuiActionIntent::Planned => Style::default().fg(theme.planned),
            };
            ListItem::new(Line::from(vec![
                Span::styled(item.title, Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(item.phase, phase_style),
            ]))
        })
        .collect();
    let mut list_state = ListState::default();
    list_state.select(Some(app.selected_action));
    let title = if matches!(app.focus, TuiFocus::Actions) {
        " Actions * "
    } else {
        " Actions "
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(selection_style(app.focus, TuiFocus::Actions, theme))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, chunks[0], &mut list_state);

    let selected = app.selected_action();
    let user_name = app
        .selected_user()
        .map(|user| user.name.as_str())
        .unwrap_or("-");
    let profile = app.selected_profile();
    let profile_name = profile.map(|p| p.name.as_str()).unwrap_or("-");
    let profile_path = profile.map(|p| p.initial_path.as_str()).unwrap_or("-");
    let mut detail_lines = vec![
        Line::from(Span::styled(
            selected.title,
            theme.accent_style().add_modifier(Modifier::BOLD),
        )),
        Line::from(selected.description),
        Line::from(vec![
            Span::styled("User:    ", theme.muted_style()),
            Span::raw(user_name),
        ]),
        Line::from(vec![
            Span::styled("Profile: ", theme.muted_style()),
            Span::raw(profile_name),
        ]),
        Line::from(vec![
            Span::styled("Path:    ", theme.muted_style()),
            Span::raw(profile_path),
        ]),
        Line::from(vec![
            Span::styled("Command: ", theme.muted_style()),
            Span::raw(selected.command),
        ]),
        Line::from(vec![
            Span::styled("Status:  ", theme.muted_style()),
            Span::raw(selected.phase),
        ]),
        Line::from(Span::styled(app.pane_summary(), theme.muted_style())),
    ];
    if let Some(summary) = &app.browser.summary {
        detail_lines.push(Line::from(vec![
            Span::styled("Listed:  ", theme.muted_style()),
            Span::raw(format!(
                "{} ({} items, {} dirs, {} files)",
                app.browser.path, summary.total, summary.dirs, summary.files
            )),
        ]));
        for entry in app.browser.entries.iter().take(3) {
            detail_lines.push(Line::from(vec![
                Span::styled(
                    if entry.is_dir { "dir  " } else { "file " },
                    theme.muted_style(),
                ),
                Span::raw(format_browser_entry(entry)),
            ]));
        }
    }
    let details = Paragraph::new(detail_lines)
        .block(Block::default().borders(Borders::ALL).title(" Intent "))
        .wrap(Wrap { trim: true });
    frame.render_widget(details, chunks[1]);
}

fn format_browser_entry(entry: &crate::cli_tui::panes::browser::BrowserEntry) -> String {
    if entry.is_dir {
        format!("{}/", entry.name)
    } else {
        format!("{}  {} B", entry.name, entry.size)
    }
}

fn selection_style(focus: TuiFocus, pane: TuiFocus, theme: TuiTheme) -> Style {
    if focus == pane {
        Style::default()
            .bg(theme.selection)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .bg(Color::DarkGray)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    }
}

fn lock_style(locked: bool, theme: TuiTheme) -> Style {
    if locked {
        Style::default().fg(theme.planned)
    } else {
        Style::default().fg(theme.ready)
    }
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
