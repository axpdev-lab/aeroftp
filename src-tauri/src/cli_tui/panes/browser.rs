use crate::cli_tui::worker::{TuiListResult, TuiStatResult};

#[derive(Debug, Default)]
pub struct BrowserPaneState {
    pub selected: usize,
    pub path: String,
    pub root_path: Option<String>,
    pub entries: Vec<BrowserEntry>,
    pub summary: Option<BrowserSummary>,
    pub preview: Option<BrowserPreview>,
}

impl BrowserPaneState {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn apply_list_result(&mut self, path: String, result: TuiListResult) {
        let path = normalize_browser_path(&path);
        if self.root_path.is_none() {
            self.root_path = Some(path.clone());
        }
        self.path = path;
        self.selected = 0;
        self.preview = None;
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

    pub fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            self.selected = 0;
            self.preview = None;
            return;
        }

        let next = (self.selected as isize + delta)
            .clamp(0, self.entries.len().saturating_sub(1) as isize);
        if self.selected != next as usize {
            self.preview = None;
            self.selected = next as usize;
        }
    }

    pub fn selected_entry(&self) -> Option<&BrowserEntry> {
        self.entries.get(self.selected)
    }

    pub fn selected_entry_path(&self) -> Option<String> {
        let entry = self.selected_entry()?;
        let path = if entry.path.trim().is_empty() {
            join_remote_child(&self.path, &entry.name)
        } else {
            entry.path.clone()
        };
        Some(normalize_browser_path(&path))
    }

    pub fn selected_directory_path(&self) -> Option<String> {
        let entry = self.selected_entry()?;
        if !entry.is_dir {
            return None;
        }

        self.selected_entry_path()
    }

    pub fn selected_file_path(&self) -> Option<String> {
        let entry = self.selected_entry()?;
        if entry.is_dir {
            return None;
        }

        self.selected_entry_path()
    }

    pub fn clear_preview(&mut self) {
        self.preview = None;
    }

    pub fn apply_stat_result(&mut self, result: TuiStatResult) -> bool {
        if !self.selected_path_matches(&result.path) {
            return false;
        }

        self.preview = Some(BrowserPreview {
            name: result.name,
            path: normalize_browser_path(&result.path),
            is_dir: result.is_dir,
            size: result.size,
            modified: result.modified,
            permissions: result.permissions,
            owner: result.owner,
            group: result.group,
            is_symlink: result.is_symlink,
            link_target: result.link_target,
            mime_type: result.mime_type,
        });
        true
    }

    pub fn parent_path(&self) -> Option<String> {
        let current = normalize_browser_path(&self.path);
        if current == "/" {
            return None;
        }

        let root = self
            .root_path
            .as_deref()
            .map(normalize_browser_path)
            .unwrap_or_else(|| "/".to_string());
        if current == root {
            return None;
        }

        let parent = raw_parent_path(&current)?;
        if path_is_at_or_below(&parent, &root) {
            Some(parent)
        } else {
            Some(root)
        }
    }

    fn selected_path_matches(&self, path: &str) -> bool {
        self.selected_entry_path()
            .map(|selected| selected == normalize_browser_path(path))
            .unwrap_or(false)
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

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BrowserPreview {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<String>,
    pub permissions: Option<String>,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub is_symlink: bool,
    pub link_target: Option<String>,
    pub mime_type: Option<String>,
}

fn normalize_browser_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "." {
        return "/".to_string();
    }

    let without_trailing = trimmed.trim_end_matches('/');
    if without_trailing.is_empty() {
        "/".to_string()
    } else {
        without_trailing.to_string()
    }
}

fn raw_parent_path(path: &str) -> Option<String> {
    let normalized = normalize_browser_path(path);
    if normalized == "/" {
        return None;
    }

    match normalized.rsplit_once('/') {
        Some(("", _)) => Some("/".to_string()),
        Some((parent, _)) => Some(parent.to_string()),
        None => Some("/".to_string()),
    }
}

fn join_remote_child(parent: &str, child: &str) -> String {
    let parent = normalize_browser_path(parent);
    let child = child.trim_matches('/');
    if child.is_empty() {
        return parent;
    }
    if parent == "/" {
        format!("/{}", child)
    } else {
        format!("{}/{}", parent, child)
    }
}

fn path_is_at_or_below(path: &str, root: &str) -> bool {
    let path = normalize_browser_path(path);
    let root = normalize_browser_path(root);
    if root == "/" || path == root {
        return true;
    }
    path.strip_prefix(&root)
        .map(|suffix| suffix.starts_with('/'))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_tui::worker::{TuiListEntry, TuiListSummary};

    fn list(entries: Vec<TuiListEntry>) -> TuiListResult {
        TuiListResult {
            entries,
            summary: TuiListSummary {
                total: 0,
                files: 0,
                dirs: 0,
                total_bytes: 0,
                truncated: false,
                total_before_limit: 0,
            },
        }
    }

    #[test]
    fn parent_navigation_stops_at_the_first_listed_root() {
        let mut browser = BrowserPaneState::default();
        browser.apply_list_result("/srv".to_string(), list(Vec::new()));
        browser.apply_list_result("/srv/docs".to_string(), list(Vec::new()));

        assert_eq!(browser.parent_path().as_deref(), Some("/srv"));

        browser.apply_list_result("/srv".to_string(), list(Vec::new()));
        assert_eq!(browser.parent_path(), None);
    }

    #[test]
    fn selected_directory_uses_entry_path_or_falls_back_to_name() {
        let mut browser = BrowserPaneState::default();
        browser.apply_list_result(
            "/srv".to_string(),
            list(vec![TuiListEntry {
                name: "docs".to_string(),
                path: String::new(),
                is_dir: true,
                size: 0,
                modified: None,
            }]),
        );

        assert_eq!(
            browser.selected_directory_path().as_deref(),
            Some("/srv/docs")
        );
    }

    #[test]
    fn selected_file_path_uses_entry_path_or_falls_back_to_name() {
        let mut browser = BrowserPaneState::default();
        browser.apply_list_result(
            "/srv".to_string(),
            list(vec![TuiListEntry {
                name: "readme.txt".to_string(),
                path: String::new(),
                is_dir: false,
                size: 42,
                modified: None,
            }]),
        );

        assert_eq!(
            browser.selected_file_path().as_deref(),
            Some("/srv/readme.txt")
        );
    }

    #[test]
    fn selection_is_clamped_to_available_entries() {
        let mut browser = BrowserPaneState::default();
        browser.apply_list_result(
            "/".to_string(),
            list(vec![
                TuiListEntry {
                    name: "a".to_string(),
                    path: "/a".to_string(),
                    is_dir: true,
                    size: 0,
                    modified: None,
                },
                TuiListEntry {
                    name: "b".to_string(),
                    path: "/b".to_string(),
                    is_dir: true,
                    size: 0,
                    modified: None,
                },
            ]),
        );

        browser.move_selection(10);
        assert_eq!(browser.selected, 1);
        browser.move_selection(-10);
        assert_eq!(browser.selected, 0);
    }

    #[test]
    fn stat_preview_applies_only_to_the_current_selection() {
        let mut browser = BrowserPaneState::default();
        browser.apply_list_result(
            "/".to_string(),
            list(vec![
                TuiListEntry {
                    name: "a.txt".to_string(),
                    path: "/a.txt".to_string(),
                    is_dir: false,
                    size: 1,
                    modified: None,
                },
                TuiListEntry {
                    name: "b.txt".to_string(),
                    path: "/b.txt".to_string(),
                    is_dir: false,
                    size: 2,
                    modified: None,
                },
            ]),
        );

        assert!(browser.apply_stat_result(TuiStatResult {
            name: "a.txt".to_string(),
            path: "/a.txt".to_string(),
            is_dir: false,
            size: 1,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
        }));

        browser.move_selection(1);
        assert_eq!(browser.preview, None);
        assert!(!browser.apply_stat_result(TuiStatResult {
            name: "a.txt".to_string(),
            path: "/a.txt".to_string(),
            is_dir: false,
            size: 1,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
        }));
        assert_eq!(browser.preview, None);
    }
}
