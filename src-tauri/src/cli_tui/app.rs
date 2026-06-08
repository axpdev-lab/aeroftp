use crate::cli_tui::{
    event::TuiAction,
    panes::{
        browser::BrowserPaneState, profiles::ProfilesPaneState, transfers::TransfersPaneState,
    },
    worker::WorkerEvent,
};

#[derive(Debug)]
pub struct AppState {
    pub selected: usize,
    pub should_quit: bool,
    pub status: String,
    pub browser: BrowserPaneState,
    pub profiles: ProfilesPaneState,
    pub transfers: TransfersPaneState,
    pub worker: WorkerEvent,
    intent: Option<TuiIntent>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            selected: 0,
            should_quit: false,
            status: "Select an action. Enter runs ready actions through the CLI path.".to_string(),
            browser: BrowserPaneState::default(),
            profiles: ProfilesPaneState::default(),
            transfers: TransfersPaneState::default(),
            worker: WorkerEvent::Idle,
            intent: None,
        }
    }

    pub fn selected_item(&self) -> &'static TuiMenuItem {
        &TUI_MENU_ITEMS[self.selected.min(TUI_MENU_ITEMS.len().saturating_sub(1))]
    }

    pub fn take_intent(&mut self) -> Option<TuiIntent> {
        self.intent.take()
    }

    pub fn pane_summary(&self) -> String {
        format!(
            "browser:{} profiles:{} transfers:{} worker:{}",
            self.browser.selected,
            self.profiles.selected,
            self.transfers.selected,
            self.worker.label()
        )
    }

    pub fn apply_action(&mut self, action: TuiAction) {
        match action {
            TuiAction::Quit => {
                self.should_quit = true;
                self.intent = Some(TuiIntent::Quit);
            }
            TuiAction::MoveDown => {
                if self.selected + 1 < TUI_MENU_ITEMS.len() {
                    self.selected += 1;
                    self.profiles.selected = self.selected;
                    self.status = self.selected_item().description.to_string();
                }
            }
            TuiAction::MoveUp => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.profiles.selected = self.selected;
                    self.status = self.selected_item().description.to_string();
                }
            }
            TuiAction::Activate => {
                let item = self.selected_item();
                match item.intent {
                    Some(intent) => {
                        self.intent = Some(intent);
                        self.should_quit = true;
                    }
                    None => {
                        self.status = format!(
                            "{} is planned for {}. Command preview: {}",
                            item.title, item.phase, item.command
                        );
                    }
                }
            }
            TuiAction::Noop => {}
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TuiIntent {
    Quit,
    ProfilesInteractive,
}

#[derive(Debug, Clone, Copy)]
pub struct TuiMenuItem {
    pub title: &'static str,
    pub command: &'static str,
    pub description: &'static str,
    pub phase: &'static str,
    pub intent: Option<TuiIntent>,
}

pub const TUI_MENU_ITEMS: &[TuiMenuItem] = &[
    TuiMenuItem {
        title: "Profiles navigator",
        command: "aeroftp-cli profiles -i",
        description:
            "Open the existing profile navigator and run actions through the tested profiles loop.",
        phase: "P1 ready",
        intent: Some(TuiIntent::ProfilesInteractive),
    },
    TuiMenuItem {
        title: "Remote browser",
        command: "aeroftp-cli ls --profile NAME / -l",
        description:
            "Single-pane remote listing with held session, sort, stat, mkdir, rename and delete.",
        phase: "P2",
        intent: None,
    },
    TuiMenuItem {
        title: "Disk usage",
        command: "aeroftp-cli ncdu --profile NAME /",
        description: "Embed the existing ncdu explorer as a pane after profile selection is wired.",
        phase: "P1/P2",
        intent: None,
    },
    TuiMenuItem {
        title: "Transfers",
        command: "aeroftp-cli get|put --profile NAME ...",
        description: "Live transfer queue with ratatui gauges fed by worker progress events.",
        phase: "P2/P3",
        intent: None,
    },
    TuiMenuItem {
        title: "Command palette",
        command: ": <any aeroftp-cli command>",
        description:
            "Parse line-mode commands from inside the TUI without re-implementing handlers.",
        phase: "P3",
        intent: None,
    },
];

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
