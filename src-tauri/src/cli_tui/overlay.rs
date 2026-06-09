//! Modal overlay state for the interactive TUI.
//!
//! Overlays are the only place the dashboard collects free-form input or a
//! yes/no decision before issuing a mutating worker command. They carry no
//! provider or vault knowledge: the enclosing [`crate::cli_tui::app::AppState`]
//! turns a committed overlay into the concrete
//! [`crate::cli_tui::worker::WorkerCommand`], keeping the "TUI is state, render
//! and input only" invariant intact.

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub enum TuiOverlay {
    #[default]
    None,
    Prompt(PromptState),
    Confirm(ConfirmState),
}

impl TuiOverlay {
    pub fn is_active(&self) -> bool {
        !matches!(self, TuiOverlay::None)
    }
}

/// A single-line text prompt (create directory, rename, transfer paths).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PromptState {
    pub kind: PromptKind,
    pub title: String,
    pub hint: String,
    pub buffer: String,
}

impl PromptState {
    pub fn new(
        kind: PromptKind,
        title: impl Into<String>,
        hint: impl Into<String>,
        initial: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            title: title.into(),
            hint: hint.into(),
            buffer: initial.into(),
        }
    }

    /// Append a printable character, rejecting control characters and the path
    /// separators that would let a single-segment prompt escape its directory.
    pub fn push_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        if matches!(
            self.kind,
            PromptKind::Mkdir { .. } | PromptKind::Rename { .. }
        ) && matches!(c, '/' | '\\')
        {
            return;
        }
        self.buffer.push(c);
    }

    pub fn backspace(&mut self) {
        self.buffer.pop();
    }

    pub fn trimmed(&self) -> &str {
        self.buffer.trim()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PromptKind {
    /// Create a directory named by the buffer inside `parent`.
    Mkdir { parent: String },
    /// Rename `from` to a sibling named by the buffer.
    Rename { from: String },
    /// Download `remote` to the local path in the buffer.
    Download { remote: String },
    /// Upload the local path in the buffer into `remote_dir`.
    Upload { remote_dir: String },
}

/// A yes/no confirmation guarding a destructive action.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ConfirmState {
    pub kind: ConfirmKind,
    pub message: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ConfirmKind {
    /// Delete `path`; `recursive` when the entry is a non-empty directory.
    Delete { path: String, recursive: bool },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mkdir_prompt_rejects_separators_and_control_chars() {
        let mut prompt = PromptState::new(
            PromptKind::Mkdir {
                parent: "/srv".to_string(),
            },
            "New folder",
            "name",
            String::new(),
        );
        for c in "ab/c\\d\n".chars() {
            prompt.push_char(c);
        }
        assert_eq!(prompt.buffer, "abcd");
    }

    #[test]
    fn download_prompt_keeps_local_separators() {
        let mut prompt = PromptState::new(
            PromptKind::Download {
                remote: "/srv/file.txt".to_string(),
            },
            "Download",
            "local path",
            String::new(),
        );
        for c in "./out/file.txt".chars() {
            prompt.push_char(c);
        }
        assert_eq!(prompt.buffer, "./out/file.txt");
    }

    #[test]
    fn backspace_and_trim_behave_like_a_line_editor() {
        let mut prompt = PromptState::new(
            PromptKind::Rename {
                from: "/srv/a.txt".to_string(),
            },
            "Rename",
            "new name",
            "a.txt".to_string(),
        );
        prompt.backspace();
        assert_eq!(prompt.buffer, "a.tx");
        prompt.buffer = "  spaced  ".to_string();
        assert_eq!(prompt.trimmed(), "spaced");
    }
}
