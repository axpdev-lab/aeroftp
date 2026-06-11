use std::{io, io::IsTerminal, time::Duration};

use crossterm::{
    event::{
        self as crossterm_event, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind,
    },
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
        Wrap,
    },
    Terminal,
};

use self::{
    app::{AppState, BrowserSide, TuiFocus},
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

pub use app::{TuiContext, TuiGroup, TuiIntent, TuiProfile, TuiProfileAction, TuiUser};

pub type CliTuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

/// Reference version of the interactive TUI, tracked independently of the
/// AeroFTP release (a sub-version surfaced in the TUI header). Beta 1.0.0 ships
/// the IntroHub + multi-user dual-pane file manager with the full transfer set;
/// later versions grow toward GUI parity (Discover/profile editing, etc).
pub const TUI_VERSION: &str = "1.0.1";

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
            terminal.draw(|frame| render_dashboard(frame, &mut app, theme))?;

            if app.should_quit {
                break;
            }

            if crossterm_event::poll(Duration::from_millis(100))? {
                match crossterm_event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
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
                    Event::Mouse(me) => {
                        let commands = app.handle_mouse(me);
                        dispatch_commands(&mut app, &mut worker, commands);
                    }
                    Event::Resize(_, _) => {
                        // Consume so the next draw re-computes layout for narrow
                        // terminals etc. ratatui handles the actual reflow.
                    }
                    _ => {}
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

fn render_dashboard(frame: &mut ratatui::Frame<'_>, app: &mut AppState, theme: TuiTheme) {
    let area = frame.area();

    // Shape B pivot: the pre-connection screen is the GUI-style IntroHub
    // (My Servers detailed table + header user switcher), NOT the old 5-column
    // picker. Once a live session is connected the dashboard below renders the
    // dual-pane browser. The two layouts are deliberately distinct surfaces.
    if !app.is_live_connected() {
        render_introhub(frame, area, app, theme);
    } else {
        render_browser_fullscreen(frame, area, app, theme);
    }
    render_overlay(frame, area, app, theme);
}

/// Shape B connected view: a full-screen dual-pane file manager. A header bar
/// (profile, connection dot, aggregate up/down speed, active-transfer count),
/// the REMOTE | LOCAL lists at full width, the TRANSFERS strip when transfers
/// exist, and a key-hint footer. No picker columns are shown while browsing.
/// Records pane rects into app.layout for mouse hit-testing (click to select,
/// click inactive side to focus it, wheel scroll, double-click activate).
fn render_browser_fullscreen(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &mut AppState,
    theme: TuiTheme,
) {
    let rows = Layout::vertical([
        Constraint::Length(1), // header bar
        Constraint::Min(4),    // dual panes (+ transfers strip)
        Constraint::Length(2), // footer key hints + status
    ])
    .split(area);

    render_fullscreen_header(frame, rows[0], app, theme);

    let show_transfers =
        !app.transfers.items.is_empty() || matches!(app.focus, TuiFocus::Transfers);
    let body_area = if show_transfers {
        let strip_height = transfers_strip_height(app, rows[1].height);
        let split =
            Layout::vertical([Constraint::Min(3), Constraint::Length(strip_height)]).split(rows[1]);
        app.layout.transfers_strip = Some(split[1]);
        render_transfers(frame, split[1], app, theme);
        split[0]
    } else {
        app.layout.transfers_strip = None;
        rows[1]
    };

    // REMOTE on the left, LOCAL on the right (the #311 mockup order).
    let dual = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(body_area);
    app.layout.remote_pane = Some(dual[0]);
    app.layout.local_pane = Some(dual[1]);
    render_file_pane_list(frame, dual[0], app, theme, BrowserSide::Remote);
    render_file_pane_list(frame, dual[1], app, theme, BrowserSide::Local);

    let footer = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(" Tab", theme.accent_style()),
            Span::raw(" switch   "),
            Span::styled("Enter", theme.accent_style()),
            Span::raw(" open   "),
            Span::styled("Bksp", theme.accent_style()),
            Span::raw(" up   "),
            Span::styled("g/u", theme.accent_style()),
            Span::raw(" get/put   "),
            Span::styled("n", theme.accent_style()),
            Span::raw(" mkdir   "),
            Span::styled("r", theme.accent_style()),
            Span::raw(" rename   "),
            Span::styled("d", theme.accent_style()),
            Span::raw(" delete   "),
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
}

/// Header bar of the connected view: app name + version, the profile name, a
/// connection dot, and the live transfer readout (aggregate up/down speed and
/// active-transfer count).
fn render_fullscreen_header(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &AppState,
    theme: TuiTheme,
) {
    let profile_name = app
        .session
        .identity
        .as_ref()
        .map(|identity| identity.profile_name.as_str())
        .unwrap_or("-");

    let (up, down) = app.transfers.aggregate_speed();
    let active = app.transfers.active_count();

    let mut spans = vec![
        Span::styled(
            " AeroFTP TUI",
            theme.accent_style().add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {}", TUI_VERSION), theme.muted_style()),
        Span::styled("  \u{00b7}  ", theme.muted_style()),
        Span::styled(profile_name, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("  \u{00b7}  ", theme.muted_style()),
        Span::styled("\u{25cf} connected", Style::default().fg(theme.ready)),
    ];
    spans.push(Span::styled(
        format!(
            "    \u{2191}{}/s \u{2193}{}/s",
            format_browser_size(up),
            format_browser_size(down)
        ),
        if active > 0 {
            theme.accent_style()
        } else {
            theme.muted_style()
        },
    ));
    spans.push(Span::styled(
        format!("  \u{00b7}  {} active", active),
        theme.muted_style(),
    ));
    // Surface an in-flight worker operation (connect/list/stat/transfer) so the
    // header reflects ongoing activity, not just transfer throughput.
    if matches!(app.worker, WorkerEvent::Busy { .. }) {
        spans.push(Span::styled(
            format!("  \u{00b7}  {}", app.worker.label()),
            theme.muted_style(),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// IntroHub: the GUI-style pre-connection screen. A header with the user
/// switcher and tab labels, a detailed "My Servers" table for the selected
/// user's saved profiles, a detail panel for the highlighted profile, and a
/// footer of key hints. Mirrors the GUI `MyServersTable` list view. Quota /
/// health / time columns are placeholders here and fill in once the df/health
/// probes are wired (next TUI version toward GUI parity).
fn render_introhub(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &mut AppState,
    theme: TuiTheme,
) {
    let rows = Layout::vertical([
        Constraint::Length(3), // header (identity + user switcher)
        Constraint::Min(6),    // My Servers table
        Constraint::Length(8), // selected-profile detail
        Constraint::Length(2), // footer key hints + status
    ])
    .split(area);

    app.layout.intro_table = Some(rows[1]);
    render_introhub_header(frame, rows[0], app, theme);
    render_introhub_table(frame, rows[1], app, theme);
    render_introhub_detail(frame, rows[2], app, theme);
    render_introhub_footer(frame, rows[3], app, theme);
}

fn render_introhub_header(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &AppState,
    theme: TuiTheme,
) {
    let users = &app.context.users;
    let user = app.selected_user();
    let user_name = user.map(|u| u.name.as_str()).unwrap_or("-");
    let user_pos = format!("{}/{}", app.selected_user + 1, users.len().max(1));
    let profile_count = user.map(|u| u.profile_count).unwrap_or(0);
    let fav_count = user
        .map(|u| u.profiles.iter().filter(|p| p.favorite).count())
        .unwrap_or(0);
    let is_locked = user.map(|u| u.is_locked).unwrap_or(false);
    let is_admin = user.map(|u| u.is_admin).unwrap_or(false);

    let mut user_line = vec![
        Span::styled("user: ", theme.muted_style()),
        Span::styled(user_name, theme.accent_style().add_modifier(Modifier::BOLD)),
        Span::styled(format!(" ({})", user_pos), theme.muted_style()),
    ];
    if is_admin {
        user_line.push(Span::styled("  admin", theme.muted_style()));
    }
    if is_locked {
        user_line.push(Span::styled("  LOCKED", Style::default().fg(theme.planned)));
    }
    user_line.push(Span::styled(
        format!("  ·  {} profiles", profile_count),
        theme.muted_style(),
    ));
    if fav_count > 0 {
        user_line.push(Span::styled(
            format!("  ·  {}\u{2605}", fav_count),
            theme.accent_style(),
        ));
    }
    // Group filter indicator (#320): show the active filter, or the group count.
    match &app.selected_group {
        Some(group) => user_line.push(Span::styled(
            format!("  ·  group: {}", group),
            theme.accent_style().add_modifier(Modifier::BOLD),
        )),
        None if !app.context.groups.is_empty() => user_line.push(Span::styled(
            format!("  ·  {} groups", app.context.groups.len()),
            theme.muted_style(),
        )),
        None => {}
    }

    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                " AeroFTP TUI",
                theme.accent_style().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {}", TUI_VERSION), theme.muted_style()),
            Span::raw("   "),
            Span::styled(
                "[ My Servers ]",
                theme.accent_style().add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Discover (next)", theme.muted_style()),
        ]),
        Line::from(user_line),
    ]);
    frame.render_widget(header, area);
}

fn render_introhub_table(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &AppState,
    theme: TuiTheme,
) {
    let user = app.selected_user();
    let header_cells = [
        "#",
        "Name",
        "Type",
        "Host / Account",
        "Used",
        "Total",
        "%",
        "Last",
        "\u{2605}",
    ]
    .into_iter()
    .map(|h| {
        Cell::from(Span::styled(
            h,
            theme.muted_style().add_modifier(Modifier::BOLD),
        ))
    });
    let header_row = Row::new(header_cells).height(1);

    let rows: Vec<Row> = match user {
        Some(user) if user.is_locked => vec![Row::new(vec![
            Cell::from(""),
            Cell::from(Span::styled(
                "Locked user",
                Style::default().fg(theme.planned),
            )),
            Cell::from(""),
            Cell::from(Span::styled(
                "Enter unlocks via the CLI prompt",
                theme.muted_style(),
            )),
        ])],
        Some(user) if user.profiles.is_empty() => vec![Row::new(vec![
            Cell::from(""),
            Cell::from(Span::styled(
                "No saved servers for this user",
                theme.muted_style(),
            )),
        ])],
        Some(user) => {
            // Under a group filter (#320) only the group's members are listed.
            let visible = app.visible_profile_indices();
            if visible.is_empty() {
                let label = match &app.selected_group {
                    Some(group) => format!("No servers in group '{}' for this user", group),
                    None => "No saved servers for this user".to_string(),
                };
                vec![Row::new(vec![
                    Cell::from(""),
                    Cell::from(Span::styled(label, theme.muted_style())),
                ])]
            } else {
                visible
                    .iter()
                    .map(|&i| introhub_table_row(&user.profiles[i], app, theme))
                    .collect()
            }
        }
        None => vec![Row::new(vec![Cell::from(Span::styled(
            "No users found in the local vault",
            theme.muted_style(),
        ))])],
    };

    let widths = [
        Constraint::Length(4),
        Constraint::Min(12),
        Constraint::Length(8),
        Constraint::Min(18),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(6),
        Constraint::Length(9),
        Constraint::Length(3),
    ];

    let mut state = TableState::default();
    if matches!(user, Some(u) if !u.is_locked && !u.profiles.is_empty()) {
        // Highlight the display row, not the raw profile index: under a filter
        // the visible rows are a subset, so map the selection into that subset.
        let visible = app.visible_profile_indices();
        if let Some(pos) = visible.iter().position(|&i| i == app.selected_profile) {
            state.select(Some(pos));
        }
    }

    let table = Table::new(rows, widths)
        .header(header_row)
        .block(Block::default().borders(Borders::ALL).title(" My Servers "))
        .row_highlight_style(
            Style::default()
                .bg(theme.selection)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    frame.render_stateful_widget(table, area, &mut state);
}

/// Build one detailed table row for a saved profile. Host stays in clear; the
/// account/username is masked unless the session reveal toggle (`s`) is on.
fn introhub_table_row(profile: &TuiProfile, app: &AppState, theme: TuiTheme) -> Row<'static> {
    let host = if profile.host.is_empty() || profile.host == profile.username {
        String::new()
    } else {
        profile.host.clone()
    };
    let account = if profile.username.is_empty() {
        String::new()
    } else if app.show_credentials {
        profile.username.clone()
    } else {
        app::mask_credential(&profile.username)
    };
    let subtitle = match (host.is_empty(), account.is_empty()) {
        (false, false) => format!("{}  {}", host, account),
        (false, true) => host,
        (true, false) => account,
        (true, true) => "-".to_string(),
    };

    let dash = || Cell::from(Span::styled("\u{2014}", theme.muted_style()));
    let used_cell = match profile.used {
        Some(bytes) => Cell::from(Span::styled(
            format_browser_size(bytes),
            theme.muted_style(),
        )),
        None => dash(),
    };
    let total_cell = match profile.total {
        Some(bytes) => Cell::from(Span::styled(
            format_browser_size(bytes),
            theme.muted_style(),
        )),
        None => dash(),
    };
    let pct_cell = match (profile.used, profile.total) {
        (Some(u), Some(t)) if t > 0 => Cell::from(Span::styled(
            format!("{:.0}%", (u as f64 / t as f64 * 100.0).min(999.0)),
            theme.muted_style(),
        )),
        _ => dash(),
    };
    let last_cell = match &profile.last_connected_label {
        Some(label) => Cell::from(Span::styled(label.clone(), theme.muted_style())),
        None => dash(),
    };

    let fav = if profile.favorite { "\u{2605}" } else { "" };

    // The leading cell carries a reachability dot (once probed with `H`) next to
    // the selector number.
    let (dot_glyph, dot_style) = health_glyph_style(profile.health.as_ref(), theme);
    let selector_cell = Cell::from(Line::from(vec![
        Span::styled(dot_glyph, dot_style),
        Span::styled(format!(" {}", profile.selector), theme.muted_style()),
    ]));

    Row::new(vec![
        selector_cell,
        Cell::from(Span::styled(
            profile.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Cell::from(Span::styled(
            profile.protocol.to_uppercase(),
            theme.accent_style(),
        )),
        Cell::from(subtitle),
        used_cell,
        total_cell,
        pct_cell,
        last_cell,
        Cell::from(Span::styled(fav, theme.accent_style())),
    ])
}

/// Reachability dot for a profile: filled when probed (green/amber/red by
/// status), hollow when not yet probed.
fn health_glyph_style(health: Option<&app::TuiHealth>, theme: TuiTheme) -> (&'static str, Style) {
    match health {
        None => ("\u{25cc}", theme.muted_style()), // hollow circle: not probed
        Some(h) => {
            let color = match h.status.as_str() {
                "healthy" => theme.ready,
                "degraded" => theme.planned,
                _ => Color::Red, // unreachable / error
            };
            ("\u{25cf}", Style::default().fg(color)) // filled circle
        }
    }
}

fn render_introhub_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &AppState,
    theme: TuiTheme,
) {
    let user = app.selected_user();
    let lines: Vec<Line> = match user {
        Some(user) if user.is_locked => vec![
            Line::from(Span::styled(
                format!("User '{}' is locked", user.name),
                Style::default()
                    .fg(theme.planned)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Press Enter to unlock via the existing CLI prompt (leaves the TUI).",
                theme.muted_style(),
            )),
        ],
        Some(_) => match app.selected_profile() {
            Some(profile) => {
                let account = if profile.username.is_empty() {
                    "-".to_string()
                } else if app.show_credentials {
                    profile.username.clone()
                } else {
                    app::mask_credential(&profile.username)
                };
                let host = if profile.host.is_empty() {
                    "-".to_string()
                } else {
                    profile.host.clone()
                };
                let local = if profile.default_local_path.is_empty() {
                    "launch directory".to_string()
                } else {
                    profile.default_local_path.clone()
                };
                vec![
                    Line::from(vec![
                        Span::styled(
                            profile.name.clone(),
                            theme.accent_style().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("  {}", profile.protocol.to_uppercase()),
                            theme.accent_style(),
                        ),
                        if profile.favorite {
                            Span::styled("   \u{2605} favorite", theme.accent_style())
                        } else {
                            Span::raw("")
                        },
                        if profile.groups.is_empty() {
                            Span::raw("")
                        } else {
                            Span::styled(
                                format!("   [{}]", profile.groups.join(", ")),
                                theme.muted_style(),
                            )
                        },
                    ]),
                    Line::from(vec![
                        Span::styled("Host:    ", theme.muted_style()),
                        Span::raw(host),
                    ]),
                    Line::from(vec![
                        Span::styled("Account: ", theme.muted_style()),
                        Span::raw(account),
                    ]),
                    Line::from(vec![
                        Span::styled("Remote:  ", theme.muted_style()),
                        Span::raw(if profile.initial_path.is_empty() {
                            "/".to_string()
                        } else {
                            profile.initial_path.clone()
                        }),
                        Span::styled("   Local: ", theme.muted_style()),
                        Span::raw(local),
                    ]),
                    Line::from({
                        let quota = match (profile.used, profile.total) {
                            (Some(u), Some(t)) if t > 0 => format!(
                                "{} / {} ({:.0}%)",
                                format_browser_size(u),
                                format_browser_size(t),
                                (u as f64 / t as f64 * 100.0).min(999.0)
                            ),
                            (Some(u), _) => format!("{} used", format_browser_size(u)),
                            _ => "not cached (press Q to refresh)".to_string(),
                        };
                        let last = profile
                            .last_connected_label
                            .clone()
                            .unwrap_or_else(|| "never".to_string());
                        vec![
                            Span::styled("Quota:   ", theme.muted_style()),
                            Span::raw(quota),
                            Span::styled("   Last: ", theme.muted_style()),
                            Span::raw(last),
                        ]
                    }),
                    Line::from({
                        let (glyph, style) = health_glyph_style(profile.health.as_ref(), theme);
                        let text = match &profile.health {
                            Some(h) => match h.latency_ms {
                                Some(ms) => format!(" {} ({}) {}ms", h.status, h.score, ms),
                                None => format!(" {} ({})", h.status, h.score),
                            },
                            None => " not probed (press H)".to_string(),
                        };
                        vec![
                            Span::styled("Health:  ", theme.muted_style()),
                            Span::styled(glyph, style),
                            Span::raw(text),
                        ]
                    }),
                ]
            }
            None => vec![Line::from(Span::styled(
                "No profile selected.",
                theme.muted_style(),
            ))],
        },
        None => vec![Line::from(Span::styled(
            "No users found in the local vault.",
            theme.muted_style(),
        ))],
    };

    let detail = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" Profile "))
        .wrap(Wrap { trim: true });
    frame.render_widget(detail, area);
}

fn render_introhub_footer(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &AppState,
    theme: TuiTheme,
) {
    let footer = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(" \u{2191}/\u{2193}", theme.accent_style()),
            Span::raw(" select   "),
            Span::styled("\u{2190}/\u{2192}", theme.accent_style()),
            Span::raw(" switch user   "),
            Span::styled("Enter", theme.accent_style()),
            Span::raw(" connect   "),
            Span::styled("f", theme.accent_style()),
            Span::raw(" favorite   "),
            Span::styled("G", theme.accent_style()),
            Span::raw(" groups   "),
            Span::styled("H", theme.accent_style()),
            Span::raw(" health   "),
            Span::styled("Q", theme.accent_style()),
            Span::raw(" quota   "),
            Span::styled("s", theme.accent_style()),
            Span::raw(" show creds   "),
            Span::styled("q", theme.accent_style()),
            Span::raw(" quit"),
        ]),
        Line::from(Span::styled(app.status.as_str(), theme.muted_style())),
    ]);
    frame.render_widget(footer, area);
}

/// Height of the transfers strip, including borders, clamped so it never starves
/// the dashboard panes above it on short terminals.
fn transfers_strip_height(app: &AppState, available: u16) -> u16 {
    let rows = app.transfers.items.len().clamp(1, 6) as u16;
    let desired = rows + 2;
    let ceiling = available.saturating_sub(8).max(3);
    desired.min(ceiling)
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
        TuiOverlay::None | TuiOverlay::Palette(_) => {}
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
        TuiOverlay::Groups(state) => {
            // List: an "All servers" row (clear filter) then one row per group
            // with a membership checkbox and the global member count.
            let rows = state.row_count().max(1) as u16;
            let height = rows.saturating_add(5).min(area.height);
            let popup = centered_rect(60, height, area);
            frame.render_widget(Clear, popup);

            let mut lines: Vec<Line> = Vec::new();
            let all_selected = state.cursor == 0;
            let all_style = if all_selected {
                theme.accent_style().add_modifier(Modifier::BOLD)
            } else {
                theme.muted_style()
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "{}All servers (clear filter)",
                    if all_selected { "> " } else { "  " }
                ),
                all_style,
            )));
            for (i, item) in state.groups.iter().enumerate() {
                let selected = state.cursor == i + 1;
                let style = if selected {
                    theme.accent_style().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let marker = if item.is_member { "[x]" } else { "[ ]" };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{}{} ", if selected { "> " } else { "  " }, marker),
                        style,
                    ),
                    Span::styled(item.name.clone(), style),
                    Span::styled(
                        format!(
                            "  ({} member{})",
                            item.member_count,
                            if item.member_count == 1 { "" } else { "s" }
                        ),
                        theme.muted_style(),
                    ),
                ]));
            }
            if state.groups.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  no groups yet, press n to create one",
                    theme.muted_style(),
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("Enter", theme.accent_style()),
                Span::raw(" filter  "),
                Span::styled("space", theme.accent_style()),
                Span::raw(" toggle  "),
                Span::styled("n", theme.accent_style()),
                Span::raw(" new  "),
                Span::styled("r", theme.accent_style()),
                Span::raw(" rename  "),
                Span::styled("d", theme.accent_style()),
                Span::raw(" delete  "),
                Span::styled("Esc", theme.accent_style()),
                Span::raw(" close"),
            ]));

            let body = Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" Groups: {} ", state.profile_name)),
                )
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
/// Run a ratatui surface in raw-mode alternate-screen mode.
///
/// The restore path is deliberately centralized because every interactive
/// surface must leave the user's terminal usable even when drawing or input
/// handling returns an error. Mouse capture (added for 1.0.1) is enabled here
/// and disabled on every exit path (including early errors and the future
/// panic guard in P4).
pub fn with_terminal<R>(run: impl FnOnce(&mut CliTuiTerminal) -> io::Result<R>) -> io::Result<R> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;

    if let Err(err) = stdout.execute(EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(err);
    }

    if let Err(err) = stdout.execute(EnableMouseCapture) {
        let _ = stdout.execute(LeaveAlternateScreen);
        let _ = disable_raw_mode();
        return Err(err);
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(err) => {
            let _ = io::stdout().execute(DisableMouseCapture);
            let _ = io::stdout().execute(LeaveAlternateScreen);
            let _ = disable_raw_mode();
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
    // Reverse enable order: mouse, alt screen, raw, cursor.
    let mouse_result = terminal
        .backend_mut()
        .execute(DisableMouseCapture)
        .map(|_| ());
    let screen_result = terminal
        .backend_mut()
        .execute(LeaveAlternateScreen)
        .map(|_| ());
    let raw_result = disable_raw_mode();
    let cursor_result = terminal.show_cursor();

    // Return the first error but attempt all restores.
    mouse_result?;
    screen_result?;
    raw_result?;
    cursor_result
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use crate::cli_tui::app::{TuiProfile, TuiUser};
    use crate::cli_tui::session::TuiSessionPhase;
    use ratatui::backend::TestBackend;

    fn smoke_context() -> TuiContext {
        let profile = |sel: &str, name: &str| TuiProfile {
            selector: sel.to_string(),
            id: format!("srv-{sel}"),
            name: name.to_string(),
            protocol: "sftp".to_string(),
            host: "nas.example.com".to_string(),
            username: "ale@example.com".to_string(),
            initial_path: "/srv".to_string(),
            default_local_path: "/home/ale/dl".to_string(),
            favorite: sel == "1",
            groups: if sel == "1" {
                vec!["Production".to_string()]
            } else {
                Vec::new()
            },
            // Exercise the cached-quota / last-connected render path.
            used: Some(3 * 1024 * 1024 * 1024),
            total: Some(10 * 1024 * 1024 * 1024),
            last_connected_label: Some("2d ago".to_string()),
            port: 22,
            endpoint: None,
            health: Some(crate::cli_tui::app::TuiHealth {
                status: "healthy".to_string(),
                score: 98,
                latency_ms: Some(41),
            }),
        };
        TuiContext {
            users: vec![TuiUser {
                name: "ale".to_string(),
                is_active: true,
                is_locked: false,
                is_admin: true,
                profile_count: 2,
                profiles: vec![profile("1", "NAS"), profile("2", "Archive")],
            }],
            initial_user: 0,
            download_base: "/home/ale".to_string(),
            groups: vec![crate::cli_tui::app::TuiGroup {
                name: "Production".to_string(),
                member_count: 1,
            }],
        }
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    /// The IntroHub renders the My Servers table without panicking, at a normal
    /// size and a deliberately tiny one (layout subtraction overflow guard).
    #[test]
    fn introhub_renders_my_servers_table() {
        let mut app = AppState::new_live(smoke_context());
        let theme = TuiTheme::default();

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| render_dashboard(frame, &mut app, theme))
            .unwrap();
        // Rects recorded during render for mouse hit-testing (B7 mouse task).
        assert!(
            app.layout.intro_table.is_some(),
            "layout rects recorded for mouse"
        );
        let text = buffer_text(&terminal);
        assert!(text.contains("My Servers"), "table title present");
        assert!(text.contains("AeroFTP TUI"), "header present");
        assert!(text.contains(TUI_VERSION), "TUI version surfaced");
        // Cached quota / last-connected columns render real values, not dashes.
        assert!(text.contains("GB"), "cached quota size rendered");
        assert!(text.contains("30%"), "usage percent rendered");
        assert!(text.contains("2d ago"), "last-connected label rendered");
        assert!(text.contains("healthy"), "health status rendered in detail");

        // Tiny terminal must not panic (Length(1)/Min layouts, Table widths).
        let mut tiny = Terminal::new(TestBackend::new(20, 4)).unwrap();
        tiny.draw(|frame| render_dashboard(frame, &mut app, theme))
            .unwrap();
    }

    /// The connected dual-pane view renders header + REMOTE/LOCAL panes without
    /// panicking, again including a tiny terminal.
    #[test]
    fn connected_fullscreen_renders_dual_panes() {
        let mut app = AppState::new_live(smoke_context());
        // Force a connected session so render_dashboard takes the fullscreen path.
        app.session.phase = TuiSessionPhase::Connected;
        app.focus = TuiFocus::Browser;
        let theme = TuiTheme::default();

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| render_dashboard(frame, &mut app, theme))
            .unwrap();
        assert!(
            app.layout.remote_pane.is_some() && app.layout.local_pane.is_some(),
            "dual pane rects recorded for mouse"
        );
        let text = buffer_text(&terminal);
        assert!(text.contains("Remote"), "remote pane present");
        assert!(text.contains("Local"), "local pane present");
        assert!(text.contains("connected"), "connection dot present");

        let mut tiny = Terminal::new(TestBackend::new(18, 5)).unwrap();
        tiny.draw(|frame| render_dashboard(frame, &mut app, theme))
            .unwrap();
    }
}
