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
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};

use self::{
    app::{AppState, BrowserSide, TuiActionIntent, TuiFocus, TUI_ACTION_ITEMS},
    event::{key_to_action, key_to_overlay},
    overlay::TuiOverlay,
    panes::transfers::{TransferItem, TransferStatus},
    theme::TuiTheme,
    worker::{TuiWorkerClient, TuiWorkerOperation, WorkerCommand, WorkerEvent},
};

pub mod app;
pub mod event;
pub mod overlay;
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
    // Always populate the local pane at launch (local fs is always available, no remote needed).
    // This ensures the preview/local side shows files from the launch CWD (or profile default if set later).
    if let Some(ref client) = worker {
        if !app.local.path.is_empty() {
            let _ = client.commands.send(WorkerCommand::LocalList {
                path: app.local.path.clone(),
            });
        }
    }
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
                    let commands = if app.overlay_active() {
                        app.handle_overlay_key(key_to_overlay(key))
                    } else {
                        app.apply_action(key_to_action(key))
                    };
                    dispatch_commands(&mut app, &mut worker, commands);
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
    let mut follow_up = Vec::new();
    {
        let Some(client) = worker.as_mut() else {
            return;
        };

        loop {
            match client.events.try_recv() {
                Ok(event) => follow_up.extend(app.apply_worker_event(event)),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    app.apply_worker_event(WorkerEvent::Failed {
                        operation: TuiWorkerOperation::Connect,
                        identity: None,
                        message: "worker stopped".to_string(),
                    });
                    *worker = None;
                    break;
                }
            }
        }
    }
    dispatch_commands(app, worker, follow_up);
}

/// Forward worker commands, degrading gracefully if the worker channel closed.
fn dispatch_commands(
    app: &mut AppState,
    worker: &mut Option<TuiWorkerClient>,
    commands: Vec<WorkerCommand>,
) {
    if commands.is_empty() {
        return;
    }
    let Some(client) = worker.as_ref() else {
        return;
    };
    for command in commands {
        if client.commands.send(command).is_err() {
            app.apply_worker_event(WorkerEvent::Failed {
                operation: TuiWorkerOperation::Connect,
                identity: None,
                message: "worker channel closed".to_string(),
            });
            break;
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
            "User -> profile -> action -> browser. Live reads are queued through CLI handlers.",
            theme.muted_style(),
        )),
    ]);
    frame.render_widget(header, rows[0]);

    let show_transfers =
        !app.transfers.items.is_empty() || matches!(app.focus, TuiFocus::Transfers);
    let body_area = if show_transfers {
        let strip_height = transfers_strip_height(app, rows[1].height);
        let split =
            Layout::vertical([Constraint::Min(6), Constraint::Length(strip_height)]).split(rows[1]);
        render_transfers(frame, split[1], app, theme);
        split[0]
    } else {
        rows[1]
    };

    let body_direction = if area.width >= 96 {
        Direction::Horizontal
    } else {
        Direction::Vertical
    };
    // The Users column only ever shows a short list (often a single user), so it
    // does not deserve a wide share. When a live session is connected the focus is
    // on the dual-pane browser, so the picker columns shrink further and the
    // browser container takes the lion's share (incremental step toward the #311
    // full-screen dual-pane; the full Shape B pivot is a dedicated follow-up).
    let body_constraints = if app.is_live_connected() {
        [
            Constraint::Percentage(10),
            Constraint::Percentage(18),
            Constraint::Percentage(14),
            Constraint::Percentage(58),
        ]
    } else {
        [
            Constraint::Percentage(12),
            Constraint::Percentage(27),
            Constraint::Percentage(23),
            Constraint::Percentage(38),
        ]
    };
    let body = Layout::new(body_direction, body_constraints).split(body_area);

    render_users(frame, body[0], app, theme);
    render_profiles(frame, body[1], app, theme);
    render_actions(frame, body[2], app, theme);

    // Phase 3: true dual-pane split (local | remote) when we have a live connected session.
    // The "browser" column is split horizontally; lists take most space, active summary below.
    let browser_area = body[3];
    if app.is_live_connected() {
        // Give a bit more space to files when dual (the 30% column is now container).
        // We keep the outer percentages for now; inside we split 50/50 for the two lists.
        let summary_lines = 5u16; // compact summary for active side
        let lists_height = browser_area.height.saturating_sub(summary_lines + 2); // +2 for borders/padding safety
        let lists_area = Rect {
            x: browser_area.x,
            y: browser_area.y,
            width: browser_area.width,
            height: lists_height,
        };
        let dual = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(lists_area);

        render_file_pane_list(frame, dual[0], app, theme, BrowserSide::Local);
        render_file_pane_list(frame, dual[1], app, theme, BrowserSide::Remote);

        // Active side summary at the bottom of the browser column
        let summary_area = Rect {
            x: browser_area.x,
            y: lists_area.y + lists_height,
            width: browser_area.width,
            height: summary_lines + 1,
        };
        render_active_file_pane_summary(frame, summary_area, app, theme);
    } else {
        // In the picker/dashboard (non-live), show a split preview of Local and Remote paths
        // based on the selected profile's saved paths (or launch CWD). This gives the dual
        // info even before activating "Connect & browse".
        render_browser_preview(frame, browser_area, app, theme);
    }

    let footer = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(" Move", theme.accent_style()),
            Span::raw(" Up/Down   "),
            Span::styled("Pane", theme.accent_style()),
            Span::raw(" Tab   "),
            Span::styled("Open", theme.accent_style()),
            Span::raw(" Enter   "),
            Span::styled("n", theme.accent_style()),
            Span::raw(" mkdir   "),
            Span::styled("r", theme.accent_style()),
            Span::raw(" rename   "),
            Span::styled("d", theme.accent_style()),
            Span::raw(" delete   "),
            Span::styled("g/u", theme.accent_style()),
            Span::raw(" get/put   "),
            Span::styled("c", theme.accent_style()),
            Span::raw(" cancel   "),
            Span::styled("D", theme.accent_style()),
            Span::raw(" clear   "),
            Span::styled("s", theme.accent_style()),
            Span::raw(" show   "),
            Span::styled("q", theme.accent_style()),
            Span::raw(" quit"),
        ]),
        Line::from(Span::styled(app.status.as_str(), theme.muted_style())),
    ]);
    frame.render_widget(footer, rows[2]);

    render_overlay(frame, area, app, theme);
}

/// Height of the transfers strip, including borders, clamped so it never starves
/// the dashboard panes above it on short terminals.
fn transfers_strip_height(app: &AppState, available: u16) -> u16 {
    let rows = app.transfers.items.len().clamp(1, 6) as u16;
    let desired = rows + 2;
    let ceiling = available.saturating_sub(8).max(3);
    desired.min(ceiling)
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
                        // host always clear (per spec: "host in chiaro"); username (auth id) masked by default.
                        // For cloud providers without a real host the subtitle echoes the
                        // username/credential itself: rendering it here would both duplicate
                        // the username column AND leak it in clear (defeating the `s` toggle),
                        // so suppress it and let the maskable username column carry it.
                        let host_is_credential_echo =
                            !profile.host.is_empty() && profile.host == profile.username;
                        let host_span = if host_is_credential_echo {
                            Span::raw("")
                        } else {
                            Span::styled(format!("  {}", profile.host), theme.muted_style())
                        };
                        let user_span = if profile.username.is_empty() {
                            Span::raw("")
                        } else if app.show_credentials {
                            Span::styled(
                                format!("  {}", profile.username),
                                Style::default().fg(theme.accent), // raw shown emphasized
                            )
                        } else {
                            Span::styled(
                                format!("  {}", app::mask_credential(&profile.username)),
                                theme.muted_style(),
                            )
                        };
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
                            host_span,
                            user_span,
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
    ];
    // Echo auth username (masked by default) in the Intent preview per masking task.
    if let Some(p) = profile {
        if !p.username.is_empty() {
            let auth = if app.show_credentials {
                p.username.clone()
            } else {
                app::mask_credential(&p.username)
            };
            detail_lines.push(Line::from(vec![
                Span::styled("Auth:    ", theme.muted_style()),
                Span::raw(auth),
            ]));
        }
    }
    detail_lines.extend(vec![
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
    ]);
    // Phase 3: use active side for the "Listed" info in Intent.
    if let Some(summary) = app.active_browser().summary.as_ref() {
        detail_lines.push(Line::from(vec![
            Span::styled("Listed:  ", theme.muted_style()),
            Span::raw(format!(
                "{} ({} items, {} dirs, {} files)",
                app.active_browser().path,
                summary.total,
                summary.dirs,
                summary.files
            )),
        ]));
    }
    let details = Paragraph::new(detail_lines)
        .block(Block::default().borders(Borders::ALL).title(" Intent "))
        .wrap(Wrap { trim: true });
    frame.render_widget(details, chunks[1]);
}

/// Phase 3: render just the file list for one side of the dual-pane (local or remote).
/// No internal summary (summary is rendered once below for the active side).
fn render_file_pane_list(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &AppState,
    theme: TuiTheme,
    side: BrowserSide,
) {
    let state = if side == BrowserSide::Local {
        &app.local
    } else {
        &app.browser
    };
    let is_active = app.focus == TuiFocus::Browser && app.active_browser_side == side;

    let title_prefix = if side == BrowserSide::Local {
        " Local"
    } else {
        " Remote"
    };
    let title = if is_active {
        format!("{} * ", title_prefix)
    } else {
        format!("{}  ", title_prefix)
    };
    let title = if state.path.is_empty() {
        title
    } else {
        format!("{}{} ", title, state.path)
    };

    let items: Vec<ListItem> = if state.entries.is_empty() {
        let message = if state.summary.is_some() {
            "(empty directory)"
        } else {
            "No listing loaded"
        };
        vec![ListItem::new(Line::from(Span::styled(
            message,
            theme.muted_style(),
        )))]
    } else {
        state
            .entries
            .iter()
            .map(|entry| {
                let kind = if entry.is_dir { "DIR " } else { "FILE" };
                let kind_style = if entry.is_dir {
                    theme.accent_style()
                } else {
                    theme.muted_style()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(kind, kind_style),
                    Span::raw(" "),
                    Span::styled(
                        format_browser_entry(entry),
                        if entry.is_dir {
                            Style::default().add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        },
                    ),
                    Span::styled(browser_entry_meta(entry), theme.muted_style()),
                ]))
            })
            .collect()
    };

    let mut list_state = ListState::default();
    if is_active && !state.entries.is_empty() {
        list_state.select(Some(state.selected));
    }
    // For inactive pane, no cursor highlight (or very subtle). We still show the list content.
    let highlight = if is_active {
        selection_style(app.focus, TuiFocus::Browser, theme)
    } else {
        Style::default()
    };

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(highlight)
        .highlight_symbol(if is_active { "> " } else { "  " });
    frame.render_stateful_widget(list, area, &mut list_state);
}

/// Phase 3: compact summary for the currently active file pane (local or remote).
fn render_active_file_pane_summary(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &AppState,
    theme: TuiTheme,
) {
    let state = app.active_browser();
    let side_label = if app.active_browser_side == BrowserSide::Local {
        "Local"
    } else {
        "Remote"
    };

    let summary = match &state.summary {
        Some(summary) => {
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(format!("{} Path:  ", side_label), theme.muted_style()),
                    Span::raw(state.path.as_str()),
                ]),
                Line::from(vec![
                    Span::styled("Items: ", theme.muted_style()),
                    Span::raw(format!(
                        "{} total, {} dirs, {} files, {}",
                        summary.total,
                        summary.dirs,
                        summary.files,
                        format_browser_size(summary.total_bytes)
                    )),
                    Span::styled(
                        if summary.truncated { " truncated" } else { "" },
                        theme.muted_style(),
                    ),
                ]),
            ];
            if let Some(preview) = &state.preview {
                lines.extend(browser_preview_lines(preview, theme));
            } else if let Some(entry) = state.selected_entry() {
                lines.push(Line::from(vec![
                    Span::styled("Sel:   ", theme.muted_style()),
                    Span::raw(format_browser_entry(entry)),
                    Span::styled(
                        if entry.is_dir {
                            "  directory"
                        } else {
                            "  file"
                        },
                        theme.muted_style(),
                    ),
                ]));
            }
            lines
        }
        None => vec![
            Line::from(vec![
                Span::styled(format!("{} Path:  ", side_label), theme.muted_style()),
                Span::raw("-"),
            ]),
            Line::from(vec![
                Span::styled("Items: ", theme.muted_style()),
                Span::raw("-"),
            ]),
        ],
    };

    let details = Paragraph::new(summary)
        .block(Block::default().borders(Borders::ALL).title(" Listing "))
        .wrap(Wrap { trim: true });
    frame.render_widget(details, area);
}

/// Preview for the browser column in the picker/dashboard (before live connect).
/// Splits the area to show the expected Local and Remote starting paths for the
/// selected profile (respecting saved localPath/defaultLocalPath if present).
/// This gives a "dual" feel even in the initial view.
fn render_browser_preview(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &AppState,
    _theme: TuiTheme,
) {
    let chunks =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);

    let remote_path = app
        .selected_profile()
        .map(|p| p.initial_path.clone())
        .unwrap_or_else(|| "/".to_string());

    let local_path = app
        .selected_profile()
        .and_then(|p| {
            if !p.default_local_path.is_empty() {
                Some(p.default_local_path.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| app.local.path.clone());

    let local_block = Block::default()
        .borders(Borders::ALL)
        .title(" Local (profile or launch) ");
    let local_para = Paragraph::new(format!(
        "Path: {}\n\n(Connect to list live content)",
        local_path
    ))
    .block(local_block)
    .wrap(Wrap { trim: true });
    frame.render_widget(local_para, chunks[0]);

    let remote_block = Block::default()
        .borders(Borders::ALL)
        .title(" Remote (profile initialPath) ");
    let remote_para = Paragraph::new(format!(
        "Path: {}\n\n(Connect to list live content)",
        remote_path
    ))
    .block(remote_block)
    .wrap(Wrap { trim: true });
    frame.render_widget(remote_para, chunks[1]);
}

fn render_transfers(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState, theme: TuiTheme) {
    let items: Vec<ListItem> = if app.transfers.items.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "No transfers yet. In Browser: g downloads the selected file, u uploads a local file.",
            theme.muted_style(),
        )))]
    } else {
        app.transfers
            .items
            .iter()
            .map(|item| transfer_list_item(item, theme))
            .collect()
    };

    let title = if matches!(app.focus, TuiFocus::Transfers) {
        " Transfers * "
    } else {
        " Transfers "
    };
    let mut state = ListState::default();
    if !app.transfers.items.is_empty() {
        state.select(Some(app.transfers.selected));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(selection_style(app.focus, TuiFocus::Transfers, theme))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn transfer_list_item(item: &TransferItem, theme: TuiTheme) -> ListItem<'static> {
    let bar = progress_bar(item.ratio(), 12);
    let pct = (item.ratio() * 100.0).round() as u64;
    let status_style = match item.status {
        TransferStatus::Active => theme.accent_style(),
        TransferStatus::Done => Style::default().fg(theme.ready),
        TransferStatus::Failed(_) => Style::default().fg(theme.planned),
        TransferStatus::Cancelled => theme.muted_style(),
    };
    let detail = match &item.status {
        TransferStatus::Failed(message) => format!("  {}", message),
        _ => {
            let total = if item.total > 0 {
                format_browser_size(item.total)
            } else {
                "?".to_string()
            };
            format!("  {} / {}", format_browser_size(item.transferred), total)
        }
    };
    ListItem::new(Line::from(vec![
        Span::styled(
            format!("{:<8} ", item.direction.label()),
            theme.muted_style(),
        ),
        Span::styled(format!("{} {:>3}% ", bar, pct), status_style),
        Span::styled(
            item.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  [{}]", item.status.label()), status_style),
        Span::styled(detail, theme.muted_style()),
    ]))
}

fn progress_bar(ratio: f64, width: usize) -> String {
    let filled = ((ratio.clamp(0.0, 1.0)) * width as f64).round() as usize;
    let filled = filled.min(width);
    format!("[{}{}]", "#".repeat(filled), "-".repeat(width - filled))
}

fn render_overlay(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState, theme: TuiTheme) {
    match &app.overlay {
        TuiOverlay::None => {}
        TuiOverlay::Prompt(prompt) => {
            let popup = centered_rect(60, 7, area);
            frame.render_widget(Clear, popup);
            let body = Paragraph::new(vec![
                Line::from(Span::styled(prompt.hint.clone(), theme.muted_style())),
                Line::from(""),
                Line::from(vec![
                    Span::styled("> ", theme.accent_style()),
                    Span::styled(
                        prompt.buffer.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("_", theme.accent_style()),
                ]),
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {} ", prompt.title)),
            )
            .wrap(Wrap { trim: false });
            frame.render_widget(body, popup);
        }
        TuiOverlay::Confirm(confirm) => {
            let popup = centered_rect(60, 7, area);
            frame.render_widget(Clear, popup);
            let body = Paragraph::new(vec![
                Line::from(Span::styled(
                    confirm.message.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("y", theme.accent_style()),
                    Span::raw(" confirm    "),
                    Span::styled("n / Esc", theme.accent_style()),
                    Span::raw(" cancel"),
                ]),
            ])
            .block(Block::default().borders(Borders::ALL).title(" Confirm "))
            .wrap(Wrap { trim: true });
            frame.render_widget(body, popup);
        }
    }
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let width = (area.width * percent_x / 100).max(20).min(area.width);
    let height = height.min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn format_browser_entry(entry: &crate::cli_tui::panes::browser::BrowserEntry) -> String {
    if entry.is_dir {
        format!("{}/", entry.name)
    } else {
        entry.name.clone()
    }
}

fn browser_entry_meta(entry: &crate::cli_tui::panes::browser::BrowserEntry) -> String {
    if entry.is_dir {
        return entry
            .modified
            .as_ref()
            .map(|modified| format!("  {}", short_modified(modified)))
            .unwrap_or_default();
    }

    let modified = entry
        .modified
        .as_ref()
        .map(|modified| format!("  {}", short_modified(modified)))
        .unwrap_or_default();
    format!("  {}{}", format_browser_size(entry.size), modified)
}

fn browser_preview_lines(
    preview: &crate::cli_tui::panes::browser::BrowserPreview,
    theme: TuiTheme,
) -> Vec<Line<'static>> {
    let kind = if preview.is_dir { "directory" } else { "file" };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Sel:   ", theme.muted_style()),
            Span::raw(format!(
                "{}  {}",
                preview.name,
                if preview.is_symlink { "symlink" } else { kind }
            )),
        ]),
        Line::from(vec![
            Span::styled("Size:  ", theme.muted_style()),
            Span::raw(if preview.is_dir {
                "-".to_string()
            } else {
                format!(
                    "{} ({} bytes)",
                    format_browser_size(preview.size),
                    preview.size
                )
            }),
        ]),
    ];
    if let Some(modified) = &preview.modified {
        lines.push(Line::from(vec![
            Span::styled("Mod:   ", theme.muted_style()),
            Span::raw(short_modified(modified).to_string()),
        ]));
    }
    if let Some(mime_type) = &preview.mime_type {
        lines.push(Line::from(vec![
            Span::styled("Mime:  ", theme.muted_style()),
            Span::raw(mime_type.clone()),
        ]));
    }
    if let Some(permissions) = &preview.permissions {
        lines.push(Line::from(vec![
            Span::styled("Perm:  ", theme.muted_style()),
            Span::raw(permissions.clone()),
        ]));
    }
    lines
}

fn short_modified(value: &str) -> &str {
    value.get(..16).unwrap_or(value)
}

fn format_browser_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    const TB: u64 = 1024 * GB;

    if bytes >= TB {
        format!("{:.1} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
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
