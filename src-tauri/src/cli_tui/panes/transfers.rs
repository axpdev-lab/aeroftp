use crate::cli_tui::worker::TransferDirection;

/// In-memory transfer queue rendered by the Transfers pane.
///
/// The queue is append-only for the lifetime of a transfer and is fed entirely
/// by worker events keyed on a monotonic id assigned at enqueue time. It owns no
/// provider handle: the worker performs the actual byte movement and reports
/// progress, so the pane stays pure state.
#[derive(Debug, Default)]
pub struct TransfersPaneState {
    pub selected: usize,
    pub items: Vec<TransferItem>,
    next_id: u64,
}

impl TransfersPaneState {
    /// Register a new transfer and return the id the worker command must echo
    /// back on its progress and completion events.
    pub fn enqueue(
        &mut self,
        direction: TransferDirection,
        name: String,
        remote_path: String,
        local_path: String,
    ) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.items.push(TransferItem {
            id,
            direction,
            name,
            remote_path,
            local_path,
            transferred: 0,
            total: 0,
            status: TransferStatus::Active,
        });
        self.selected = self.items.len().saturating_sub(1);
        id
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            self.selected = 0;
            return;
        }
        let next =
            (self.selected as isize + delta).clamp(0, self.items.len().saturating_sub(1) as isize);
        self.selected = next as usize;
    }

    pub fn selected_item(&self) -> Option<&TransferItem> {
        self.items.get(self.selected)
    }

    /// Apply a progress sample. Completion stays authoritative via
    /// [`Self::mark_done`]: a sample that reaches the total does not flip the
    /// status, so the gauge never claims success before the worker confirms it.
    pub fn update_progress(&mut self, id: u64, transferred: u64, total: u64) {
        if let Some(item) = self.item_mut(id) {
            item.transferred = transferred;
            if total > 0 {
                item.total = total;
            }
        }
    }

    pub fn mark_done(&mut self, id: u64) {
        if let Some(item) = self.item_mut(id) {
            if item.total == 0 {
                item.total = item.transferred;
            }
            item.transferred = item.total;
            item.status = TransferStatus::Done;
        }
    }

    pub fn mark_failed(&mut self, id: u64, message: String) {
        if let Some(item) = self.item_mut(id) {
            item.status = TransferStatus::Failed(message);
        }
    }

    /// Mark a transfer as user-cancelled. The gauge keeps its partial ratio so
    /// the row reflects how far the aborted transfer got before it stopped.
    pub fn mark_cancelled(&mut self, id: u64) {
        if let Some(item) = self.item_mut(id) {
            item.status = TransferStatus::Cancelled;
        }
    }

    /// Drop the selected transfer once it is finished. Active transfers are kept
    /// so the queue never loses an in-flight operation by accident.
    pub fn remove_selected_if_finished(&mut self) -> bool {
        let Some(item) = self.items.get(self.selected) else {
            return false;
        };
        if matches!(item.status, TransferStatus::Active) {
            return false;
        }
        self.items.remove(self.selected);
        if self.selected >= self.items.len() {
            self.selected = self.items.len().saturating_sub(1);
        }
        true
    }

    pub fn has_active(&self) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item.status, TransferStatus::Active))
    }

    fn item_mut(&mut self, id: u64) -> Option<&mut TransferItem> {
        self.items.iter_mut().find(|item| item.id == id)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TransferItem {
    pub id: u64,
    pub direction: TransferDirection,
    pub name: String,
    pub remote_path: String,
    pub local_path: String,
    pub transferred: u64,
    pub total: u64,
    pub status: TransferStatus,
}

impl TransferItem {
    /// Completion ratio in `0.0..=1.0`, saturating when the total is unknown.
    pub fn ratio(&self) -> f64 {
        match self.status {
            TransferStatus::Done => 1.0,
            _ if self.total == 0 => 0.0,
            _ => (self.transferred as f64 / self.total as f64).clamp(0.0, 1.0),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TransferStatus {
    Active,
    Done,
    Failed(String),
    Cancelled,
}

impl TransferStatus {
    pub fn label(&self) -> &str {
        match self {
            TransferStatus::Active => "active",
            TransferStatus::Done => "done",
            TransferStatus::Failed(_) => "failed",
            TransferStatus::Cancelled => "cancelled",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_assigns_monotonic_ids_and_selects_latest() {
        let mut transfers = TransfersPaneState::default();
        let first = transfers.enqueue(
            TransferDirection::Download,
            "a.txt".to_string(),
            "/srv/a.txt".to_string(),
            "./a.txt".to_string(),
        );
        let second = transfers.enqueue(
            TransferDirection::Upload,
            "b.txt".to_string(),
            "/srv/b.txt".to_string(),
            "./b.txt".to_string(),
        );

        assert_eq!(first, 1);
        assert_eq!(second, 2);
        assert_eq!(transfers.selected, 1);
        assert!(transfers.has_active());
    }

    #[test]
    fn progress_never_reaches_full_until_marked_done() {
        let mut transfers = TransfersPaneState::default();
        let id = transfers.enqueue(
            TransferDirection::Download,
            "a.txt".to_string(),
            "/srv/a.txt".to_string(),
            "./a.txt".to_string(),
        );

        transfers.update_progress(id, 50, 100);
        assert_eq!(transfers.items[0].ratio(), 0.5);
        assert_eq!(transfers.items[0].status, TransferStatus::Active);

        transfers.update_progress(id, 100, 100);
        assert_eq!(transfers.items[0].status, TransferStatus::Active);

        transfers.mark_done(id);
        assert_eq!(transfers.items[0].status, TransferStatus::Done);
        assert_eq!(transfers.items[0].ratio(), 1.0);
    }

    #[test]
    fn failure_records_message_and_keeps_item() {
        let mut transfers = TransfersPaneState::default();
        let id = transfers.enqueue(
            TransferDirection::Upload,
            "a.txt".to_string(),
            "/srv/a.txt".to_string(),
            "./a.txt".to_string(),
        );
        transfers.mark_failed(id, "network closed".to_string());

        assert_eq!(
            transfers.items[0].status,
            TransferStatus::Failed("network closed".to_string())
        );
        assert!(!transfers.has_active());
    }

    #[test]
    fn cancel_keeps_partial_ratio_and_clears_active() {
        let mut transfers = TransfersPaneState::default();
        let id = transfers.enqueue(
            TransferDirection::Download,
            "big.bin".to_string(),
            "/srv/big.bin".to_string(),
            "./big.bin".to_string(),
        );
        transfers.update_progress(id, 30, 100);
        transfers.mark_cancelled(id);

        assert_eq!(transfers.items[0].status, TransferStatus::Cancelled);
        // The gauge does not jump to full: the partial ratio is preserved.
        assert_eq!(transfers.items[0].ratio(), 0.3);
        assert!(!transfers.has_active());
        // A cancelled (finished) transfer can be dropped from the queue.
        transfers.selected = 0;
        assert!(transfers.remove_selected_if_finished());
        assert!(transfers.items.is_empty());
    }

    #[test]
    fn finished_transfers_can_be_dropped_but_active_ones_stay() {
        let mut transfers = TransfersPaneState::default();
        let active = transfers.enqueue(
            TransferDirection::Download,
            "a.txt".to_string(),
            "/srv/a.txt".to_string(),
            "./a.txt".to_string(),
        );
        transfers.selected = 0;
        assert!(!transfers.remove_selected_if_finished());

        transfers.mark_done(active);
        assert!(transfers.remove_selected_if_finished());
        assert!(transfers.items.is_empty());
        assert_eq!(transfers.selected, 0);
    }
}
