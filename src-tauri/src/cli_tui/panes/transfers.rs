use std::time::Instant;

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
            speed_bps: 0,
            last_sample: None,
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
            item.observe_speed(transferred, Instant::now());
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
            item.speed_bps = 0;
        }
    }

    pub fn mark_failed(&mut self, id: u64, message: String) {
        if let Some(item) = self.item_mut(id) {
            item.status = TransferStatus::Failed(message);
            item.speed_bps = 0;
        }
    }

    /// Mark a transfer as user-cancelled. The gauge keeps its partial ratio so
    /// the row reflects how far the aborted transfer got before it stopped.
    pub fn mark_cancelled(&mut self, id: u64) {
        if let Some(item) = self.item_mut(id) {
            item.status = TransferStatus::Cancelled;
            item.speed_bps = 0;
        }
    }

    /// Number of transfers still moving bytes. Surfaced in the connected
    /// header bar ("N active").
    pub fn active_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| matches!(item.status, TransferStatus::Active))
            .count()
    }

    /// Aggregate live throughput of the active transfers, split by direction,
    /// as `(upload_bps, download_bps)`. Feeds the header speed readout.
    pub fn aggregate_speed(&self) -> (u64, u64) {
        let mut up = 0u64;
        let mut down = 0u64;
        for item in &self.items {
            if !matches!(item.status, TransferStatus::Active) {
                continue;
            }
            match item.direction {
                TransferDirection::Upload => up = up.saturating_add(item.speed_bps),
                TransferDirection::Download => down = down.saturating_add(item.speed_bps),
            }
        }
        (up, down)
    }

    /// Drop the selected transfer once it is finished, returning its local path
    /// so the caller can discard the `.aerotmp` leftover. Active transfers are
    /// kept (returns `None`) so the queue never loses an in-flight operation.
    pub fn remove_selected_if_finished(&mut self) -> Option<String> {
        let item = self.items.get(self.selected)?;
        if matches!(item.status, TransferStatus::Active) {
            return None;
        }
        let local_path = self.items.remove(self.selected).local_path;
        if self.selected >= self.items.len() {
            self.selected = self.items.len().saturating_sub(1);
        }
        Some(local_path)
    }

    /// Remove every finished (non-active) transfer at once, returning their
    /// local paths so the caller can discard each `.aerotmp` leftover. Active
    /// transfers stay in the queue.
    pub fn clear_finished(&mut self) -> Vec<String> {
        let mut discarded = Vec::new();
        self.items.retain(|item| {
            if matches!(item.status, TransferStatus::Active) {
                true
            } else {
                discarded.push(item.local_path.clone());
                false
            }
        });
        if self.selected >= self.items.len() {
            self.selected = self.items.len().saturating_sub(1);
        }
        discarded
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
    /// Most recent throughput estimate in bytes/sec, recomputed from the delta
    /// between progress samples. Zeroed once the transfer leaves Active.
    pub speed_bps: u64,
    /// Last `(instant, transferred)` sample used to derive `speed_bps`.
    last_sample: Option<(Instant, u64)>,
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

    /// Recompute the throughput estimate from a new progress sample. Samples
    /// closer than 50ms are folded into the previous one (the backend already
    /// throttles to ~150ms) so the readout never divides by a near-zero dt.
    fn observe_speed(&mut self, transferred: u64, now: Instant) {
        match self.last_sample {
            Some((t0, b0)) => {
                let dt = now.duration_since(t0).as_secs_f64();
                if dt >= 0.05 {
                    let delta = transferred.saturating_sub(b0) as f64;
                    self.speed_bps = (delta / dt).max(0.0) as u64;
                    self.last_sample = Some((now, transferred));
                }
            }
            None => self.last_sample = Some((now, transferred)),
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
        // A cancelled (finished) transfer can be dropped, surrendering its local
        // path so the caller can delete the `.aerotmp` leftover.
        transfers.selected = 0;
        assert_eq!(
            transfers.remove_selected_if_finished(),
            Some("./big.bin".to_string())
        );
        assert!(transfers.items.is_empty());
    }

    #[test]
    fn clear_finished_drops_finished_and_returns_their_paths_keeping_active() {
        let mut transfers = TransfersPaneState::default();
        let done = transfers.enqueue(
            TransferDirection::Download,
            "a.txt".to_string(),
            "/srv/a.txt".to_string(),
            "./a.txt".to_string(),
        );
        let cancelled = transfers.enqueue(
            TransferDirection::Download,
            "b.bin".to_string(),
            "/srv/b.bin".to_string(),
            "./b.bin".to_string(),
        );
        let active = transfers.enqueue(
            TransferDirection::Upload,
            "c.bin".to_string(),
            "/srv/c.bin".to_string(),
            "./c.bin".to_string(),
        );
        transfers.mark_done(done);
        transfers.mark_cancelled(cancelled);
        // `active` stays Active.

        let discarded = transfers.clear_finished();

        assert_eq!(
            discarded,
            vec!["./a.txt".to_string(), "./b.bin".to_string()]
        );
        assert_eq!(transfers.items.len(), 1);
        assert_eq!(transfers.items[0].id, active);
        assert!(transfers.has_active());
    }

    #[test]
    fn aggregate_speed_sums_active_transfers_by_direction() {
        let mut transfers = TransfersPaneState::default();
        let down = transfers.enqueue(
            TransferDirection::Download,
            "a.bin".to_string(),
            "/srv/a.bin".to_string(),
            "./a.bin".to_string(),
        );
        let up = transfers.enqueue(
            TransferDirection::Upload,
            "b.bin".to_string(),
            "/srv/b.bin".to_string(),
            "./b.bin".to_string(),
        );
        let done = transfers.enqueue(
            TransferDirection::Download,
            "c.bin".to_string(),
            "/srv/c.bin".to_string(),
            "./c.bin".to_string(),
        );
        transfers.items[0].speed_bps = 2_000;
        transfers.items[1].speed_bps = 500;
        transfers.items[2].speed_bps = 9_999;
        transfers.mark_done(done);

        // Active aggregate: download 2000, upload 500; the finished one drops out
        // and its speed is zeroed.
        assert_eq!(transfers.aggregate_speed(), (500, 2_000));
        assert_eq!(transfers.active_count(), 2);
        let _ = (down, up);
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
        assert!(transfers.remove_selected_if_finished().is_none());

        transfers.mark_done(active);
        assert!(transfers.remove_selected_if_finished().is_some());
        assert!(transfers.items.is_empty());
        assert_eq!(transfers.selected, 0);
    }
}
