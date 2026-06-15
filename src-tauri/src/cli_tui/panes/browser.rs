use crate::cli_tui::worker::{TuiListResult, TuiStatResult};
use std::collections::BTreeSet;

/// Which column a browser pane is sorted by (rev 1.0.3 table stakes). Directories
/// are always grouped before files; the field orders within each group.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub enum SortField {
    #[default]
    Name,
    Size,
    Modified,
    Type,
}

impl SortField {
    pub fn label(self) -> &'static str {
        match self {
            SortField::Name => "name",
            SortField::Size => "size",
            SortField::Modified => "date",
            SortField::Type => "type",
        }
    }
}

/// Per-pane sort mode: a field plus a direction. `B` cycles through the eight
/// (field, direction) states; the order is session-persistent and survives `cd`
/// within a session (reset only on disconnect via [`BrowserPaneState::clear`]).
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct BrowserSort {
    pub field: SortField,
    pub ascending: bool,
}

impl Default for BrowserSort {
    fn default() -> Self {
        Self {
            field: SortField::Name,
            ascending: true,
        }
    }
}

impl BrowserSort {
    /// Advance to the next sort state: each field cycles ascending then
    /// descending before moving to the next field, wrapping back to name-asc.
    pub fn cycle(&mut self) {
        if self.ascending {
            self.ascending = false;
            return;
        }
        self.ascending = true;
        self.field = match self.field {
            SortField::Name => SortField::Size,
            SortField::Size => SortField::Modified,
            SortField::Modified => SortField::Type,
            SortField::Type => SortField::Name,
        };
    }

    /// Short indicator for the pane title, e.g. `name up` / `size down`.
    pub fn indicator(self) -> String {
        let arrow = if self.ascending {
            "\u{2191}"
        } else {
            "\u{2193}"
        };
        format!("{}{}", arrow, self.field.label())
    }
}

#[derive(Debug, Default)]
pub struct BrowserPaneState {
    pub selected: usize,
    pub path: String,
    pub root_path: Option<String>,
    /// Visible, sorted, filtered view derived from `all_entries`. `selected` and
    /// every existing accessor index into this list, so the sort/filter/hidden
    /// features are a pure view layer the rest of the TUI never has to know about.
    pub entries: Vec<BrowserEntry>,
    /// The unfiltered listing as the worker reported it (source of truth). The
    /// visible `entries` are rebuilt from this whenever the sort/filter/hidden
    /// state changes; navigation and ops still read `entries`.
    pub all_entries: Vec<BrowserEntry>,
    pub summary: Option<BrowserSummary>,
    pub preview: Option<BrowserPreview>,
    /// Active sort mode (session-persistent across `cd`, reset on disconnect).
    pub sort: BrowserSort,
    /// Live in-list filter (`/`): `None` shows everything, `Some(pattern)`
    /// narrows the listing (case-insensitive substring, or a `*`/`?` wildcard).
    /// Cleared on `cd`/reload so it only ever narrows the listing it was set on.
    pub filter: Option<String>,
    /// Whether dotfiles are shown (`a` toggle). Per-pane mode that persists across
    /// `cd` like the sort.
    pub show_hidden: bool,
    /// Paths of entries marked for a batch op (`Space`/`m`). Keyed by absolute
    /// path so the mark survives a re-sort/filter; cleared on `cd`/reload.
    pub marked: BTreeSet<String>,
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
        self.preview = None;
        // A fresh listing clears the per-listing narrowing and selection set; the
        // pane-level sort and hidden mode persist across navigation.
        self.filter = None;
        self.marked.clear();
        self.entries.clear();
        self.selected = 0;
        self.summary = Some(BrowserSummary {
            total: result.summary.total,
            files: result.summary.files,
            dirs: result.summary.dirs,
            total_bytes: result.summary.total_bytes,
            truncated: result.summary.truncated,
            total_before_limit: result.summary.total_before_limit,
        });
        self.all_entries = result
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
        self.rebuild_view();
    }

    /// Rebuild the visible `entries` from `all_entries`, applying the hidden
    /// filter, the live filter, and the sort. Keeps the cursor on the same entry
    /// (by path) when it survives the new view, else clamps to a valid index.
    pub fn rebuild_view(&mut self) {
        // Capture the currently selected entry's path so the cursor can follow it.
        let keep = self.selected_entry_path();
        let filter = self
            .filter
            .as_deref()
            .map(str::to_ascii_lowercase)
            .filter(|f| !f.is_empty());
        let show_hidden = self.show_hidden;
        let mut view: Vec<BrowserEntry> = self
            .all_entries
            .iter()
            .filter(|entry| show_hidden || !is_hidden_name(&entry.name))
            .filter(|entry| match &filter {
                Some(pattern) => name_matches_filter(&entry.name, pattern),
                None => true,
            })
            .cloned()
            .collect();
        let sort = self.sort;
        view.sort_by(|a, b| compare_entries(a, b, sort));
        self.entries = view;
        self.selected = keep
            .and_then(|kept| {
                let kept = normalize_browser_path(&kept);
                self.entries
                    .iter()
                    .position(|entry| entry_view_path(&self.path, entry) == kept)
            })
            .unwrap_or(0);
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
        // Marks follow the visible view: a mark on an entry now hidden (by the
        // filter or the dotfile toggle) is dropped, so a batch op can never act
        // on something the user cannot see. "What shows a check is what batches."
        if !self.marked.is_empty() {
            let visible: BTreeSet<String> = self
                .entries
                .iter()
                .map(|entry| entry_view_path(&self.path, entry))
                .collect();
            self.marked.retain(|path| visible.contains(path));
        }
        self.preview = None;
    }

    /// Advance the sort mode (`B`) and re-sort in place.
    pub fn cycle_sort(&mut self) {
        self.sort.cycle();
        self.rebuild_view();
    }

    /// Set the live filter (`/`) and narrow the listing.
    pub fn set_filter(&mut self, pattern: String) {
        let trimmed = pattern.trim();
        self.filter = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
        self.rebuild_view();
    }

    /// Clear the live filter (Esc in the filter prompt, or reload).
    pub fn clear_filter(&mut self) {
        if self.filter.is_some() {
            self.filter = None;
            self.rebuild_view();
        }
    }

    /// Toggle dotfile visibility (`a`) and rebuild the view.
    pub fn toggle_hidden(&mut self) -> bool {
        self.show_hidden = !self.show_hidden;
        self.rebuild_view();
        self.show_hidden
    }

    /// Toggle the mark on the selected entry (`Space`/`m`). Returns the new mark
    /// state, or `None` when nothing is selected.
    pub fn toggle_mark(&mut self) -> Option<bool> {
        let path = self.selected_entry_path()?;
        if self.marked.remove(&path) {
            Some(false)
        } else {
            self.marked.insert(path);
            Some(true)
        }
    }

    /// Mark every currently visible entry (`Ctrl+A`).
    pub fn mark_all_visible(&mut self) {
        for entry in &self.entries {
            self.marked.insert(entry_view_path(&self.path, entry));
        }
    }

    /// Drop all marks (`Alt+A`, and after a batch op / reload).
    pub fn clear_marks(&mut self) {
        self.marked.clear();
    }

    /// Whether the entry at `index` of the visible view is marked.
    pub fn is_marked(&self, entry: &BrowserEntry) -> bool {
        self.marked.contains(&entry_view_path(&self.path, entry))
    }

    pub fn marked_count(&self) -> usize {
        self.marked.len()
    }

    /// The marked entries that still exist in the listing, resolved to
    /// `(absolute_path, is_dir, name)` for a batch op. Sorted by path for a
    /// deterministic order.
    pub fn marked_entries(&self) -> Vec<(String, bool, String)> {
        let mut out: Vec<(String, bool, String)> = self
            .all_entries
            .iter()
            .filter_map(|entry| {
                let path = entry_view_path(&self.path, entry);
                if self.marked.contains(&path) {
                    Some((path, entry.is_dir, entry.name.clone()))
                } else {
                    None
                }
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
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

/// Absolute path of a browser entry, mirroring [`BrowserPaneState::selected_entry_path`]
/// for an arbitrary entry: prefer the worker-supplied path, fall back to joining
/// the name onto the pane directory. Used to key marks and follow the cursor
/// across a re-sort.
fn entry_view_path(dir: &str, entry: &BrowserEntry) -> String {
    let path = if entry.path.trim().is_empty() {
        join_remote_child(dir, &entry.name)
    } else {
        entry.path.clone()
    };
    normalize_browser_path(&path)
}

/// A dotfile is hidden by default (the `a` toggle reveals it). `.` and `..` never
/// appear in our listings, so a leading dot is the only rule.
fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.')
}

/// Match a listing entry name against a live filter pattern (already lowercased).
/// A pattern containing `*`/`?` is matched as an anchored wildcard glob; anything
/// else is a case-insensitive substring match (the friendlier default).
fn name_matches_filter(name: &str, pattern: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if pattern.contains('*') || pattern.contains('?') {
        wildcard_match(&name, pattern)
    } else {
        name.contains(pattern)
    }
}

/// Classic `*`/`?` wildcard match (no character classes), anchored at both ends.
/// `*` matches any run (including empty), `?` matches exactly one character.
/// Iterative with backtracking so it stays linear on typical patterns and never
/// recurses unboundedly.
fn wildcard_match(text: &str, pattern: &str) -> bool {
    let text: Vec<char> = text.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    let (mut t, mut p) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_t = 0usize;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            t += 1;
            p += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            star_t = t;
            p += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// Order two entries for the active sort: directories always group before files,
/// then the chosen field orders within each group (direction applies to the
/// field only, not to the dir-first grouping).
fn compare_entries(a: &BrowserEntry, b: &BrowserEntry, sort: BrowserSort) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a.is_dir, b.is_dir) {
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    let ord = match sort.field {
        SortField::Name => a
            .name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase()),
        SortField::Size => a.size.cmp(&b.size),
        SortField::Modified => a.modified.cmp(&b.modified),
        SortField::Type => name_extension(&a.name)
            .cmp(&name_extension(&b.name))
            .then_with(|| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            }),
    };
    // A stable tiebreak on the lowercased name keeps equal-field entries in a
    // deterministic order regardless of direction.
    let ord = ord.then_with(|| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
    });
    if sort.ascending {
        ord
    } else {
        ord.reverse()
    }
}

/// Lowercased file extension (after the last dot), or empty when there is none.
/// A leading-dot name (dotfile) has no extension for sort purposes.
fn name_extension(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
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

    // --- rev 1.0.3 view layer: sort / filter / hidden / marks --------------

    fn entry(name: &str, is_dir: bool, size: u64, modified: Option<&str>) -> TuiListEntry {
        TuiListEntry {
            name: name.to_string(),
            path: format!("/dir/{}", name),
            is_dir,
            size,
            modified: modified.map(str::to_string),
        }
    }

    fn sample_dir() -> BrowserPaneState {
        let mut browser = BrowserPaneState::default();
        browser.apply_list_result(
            "/dir".to_string(),
            list(vec![
                entry("readme.txt", false, 30, Some("2026-01-02 09:00")),
                entry("alpha.log", false, 10, Some("2026-03-01 09:00")),
                entry("zeta", true, 0, Some("2026-02-01 09:00")),
                entry(".hidden", false, 5, Some("2026-01-01 09:00")),
                entry("beta", true, 0, Some("2026-01-10 09:00")),
            ]),
        );
        browser
    }

    fn names(browser: &BrowserPaneState) -> Vec<String> {
        browser.entries.iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn default_sort_is_dirs_first_then_name_and_hides_dotfiles() {
        let browser = sample_dir();
        // Dirs first (beta, zeta), then files by name; dotfile hidden by default.
        assert_eq!(
            names(&browser),
            vec!["beta", "zeta", "alpha.log", "readme.txt"]
        );
    }

    #[test]
    fn toggle_hidden_reveals_and_hides_dotfiles() {
        let mut browser = sample_dir();
        assert!(browser.toggle_hidden());
        assert!(names(&browser).contains(&".hidden".to_string()));
        assert!(!browser.toggle_hidden());
        assert!(!names(&browser).contains(&".hidden".to_string()));
    }

    #[test]
    fn sort_cycle_walks_field_and_direction() {
        let mut browser = sample_dir();
        assert_eq!(browser.sort, BrowserSort::default());
        // name-asc -> name-desc: files reverse, dirs still grouped first.
        browser.cycle_sort();
        assert!(!browser.sort.ascending);
        assert_eq!(browser.sort.field, SortField::Name);
        assert_eq!(
            names(&browser),
            vec!["zeta", "beta", "readme.txt", "alpha.log"]
        );
        // -> size-asc: files by ascending size (alpha 10 < readme 30).
        browser.cycle_sort();
        assert_eq!(browser.sort.field, SortField::Size);
        assert!(browser.sort.ascending);
        assert_eq!(
            names(&browser),
            vec!["beta", "zeta", "alpha.log", "readme.txt"]
        );
    }

    #[test]
    fn sort_by_size_descending_orders_largest_file_first() {
        let mut browser = sample_dir();
        browser.sort = BrowserSort {
            field: SortField::Size,
            ascending: false,
        };
        browser.rebuild_view();
        // Dirs still first (both size 0, ordered by the descending name tiebreak),
        // then files largest-first: readme (30) before alpha (10).
        assert_eq!(
            names(&browser),
            vec!["zeta", "beta", "readme.txt", "alpha.log"]
        );
    }

    #[test]
    fn filter_narrows_by_substring_and_wildcard() {
        let mut browser = sample_dir();
        browser.set_filter("alph".to_string());
        // Case-insensitive substring: only alpha.log contains "alph".
        assert_eq!(names(&browser), vec!["alpha.log"]);
        browser.set_filter("*.log".to_string());
        assert_eq!(names(&browser), vec!["alpha.log"]);
        browser.clear_filter();
        assert_eq!(names(&browser).len(), 4);
    }

    #[test]
    fn rebuild_keeps_selection_on_the_same_entry() {
        let mut browser = sample_dir();
        // Select readme.txt (last under default sort).
        browser.selected = 3;
        assert_eq!(
            browser.selected_entry().map(|e| e.name.clone()),
            Some("readme.txt".to_string())
        );
        // Reverse the sort: readme moves but the cursor follows it.
        browser.cycle_sort();
        assert_eq!(
            browser.selected_entry().map(|e| e.name.clone()),
            Some("readme.txt".to_string())
        );
    }

    #[test]
    fn marks_toggle_select_all_and_resolve_entries() {
        let mut browser = sample_dir();
        // Mark the first two visible (beta dir, zeta dir).
        assert_eq!(browser.toggle_mark(), Some(true));
        browser.selected = 2; // alpha.log
        assert_eq!(browser.toggle_mark(), Some(true));
        assert_eq!(browser.marked_count(), 2);
        let marked = browser.marked_entries();
        let paths: Vec<String> = marked.iter().map(|(p, _, _)| p.clone()).collect();
        assert!(paths.contains(&"/dir/beta".to_string()));
        assert!(paths.contains(&"/dir/alpha.log".to_string()));
        // A second toggle on alpha clears it.
        assert_eq!(browser.toggle_mark(), Some(false));
        assert_eq!(browser.marked_count(), 1);
        // Mark-all then clear.
        browser.mark_all_visible();
        assert_eq!(browser.marked_count(), browser.entries.len());
        browser.clear_marks();
        assert_eq!(browser.marked_count(), 0);
    }

    #[test]
    fn fresh_listing_clears_filter_and_marks_but_keeps_sort_and_hidden() {
        let mut browser = sample_dir();
        browser.toggle_hidden();
        browser.cycle_sort(); // name-desc
        browser.set_filter("z".to_string());
        browser.toggle_mark();
        let sort = browser.sort;
        let hidden = browser.show_hidden;
        // A new directory listing.
        browser.apply_list_result("/dir2".to_string(), list(vec![entry("x", false, 1, None)]));
        assert_eq!(browser.filter, None);
        assert_eq!(browser.marked_count(), 0);
        assert_eq!(browser.sort, sort, "sort persists across cd");
        assert_eq!(
            browser.show_hidden, hidden,
            "hidden mode persists across cd"
        );
    }

    #[test]
    fn marks_follow_the_visible_view_when_filtering() {
        let mut browser = sample_dir();
        browser.mark_all_visible();
        assert_eq!(browser.marked_count(), browser.entries.len());
        // Narrow to just the log file: marks on now-hidden entries are dropped,
        // so a batch op can only touch what is still visible.
        browser.set_filter("*.log".to_string());
        assert_eq!(browser.entries.len(), 1);
        assert_eq!(browser.marked_count(), 1);
        let marked = browser.marked_entries();
        assert_eq!(marked.len(), 1);
        assert_eq!(marked[0].2, "alpha.log");
    }

    #[test]
    fn marks_survive_a_re_sort() {
        let mut browser = sample_dir();
        browser.toggle_mark(); // mark first visible (beta)
        assert_eq!(browser.marked_count(), 1);
        browser.cycle_sort(); // reorder only; visibility unchanged
        assert_eq!(browser.marked_count(), 1);
    }

    #[test]
    fn wildcard_matcher_handles_star_and_question() {
        assert!(wildcard_match("readme.txt", "*.txt"));
        assert!(wildcard_match("readme.txt", "read*"));
        assert!(wildcard_match("a.log", "?.log"));
        assert!(!wildcard_match("ab.log", "?.log"));
        assert!(wildcard_match("anything", "*"));
        assert!(!wildcard_match("readme.txt", "*.log"));
    }
}
