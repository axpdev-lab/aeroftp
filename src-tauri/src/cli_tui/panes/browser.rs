use crate::cli_tui::worker::TuiListResult;

#[derive(Debug, Default)]
pub struct BrowserPaneState {
    pub selected: usize,
    pub path: String,
    pub entries: Vec<BrowserEntry>,
    pub summary: Option<BrowserSummary>,
}

impl BrowserPaneState {
    pub fn apply_list_result(&mut self, path: String, result: TuiListResult) {
        self.path = path;
        self.selected = 0;
        self.summary = Some(BrowserSummary {
            total: result.summary.total,
            files: result.summary.files,
            dirs: result.summary.dirs,
            total_bytes: result.summary.total_bytes,
            truncated: result.summary.truncated,
            total_before_limit: result.summary.total_before_limit,
        });
        self.entries = result
            .entries
            .into_iter()
            .map(|entry| BrowserEntry {
                name: entry.name,
                path: entry.path,
                is_dir: entry.is_dir,
                size: entry.size,
                modified: entry.modified,
            })
            .collect();
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BrowserEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BrowserSummary {
    pub total: usize,
    pub files: usize,
    pub dirs: usize,
    pub total_bytes: u64,
    pub truncated: bool,
    pub total_before_limit: usize,
}
