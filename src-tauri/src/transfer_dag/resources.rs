// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::capabilities::TransferCapabilities;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    FileSlot,
    CheckerSlot,
    ChunkSlot,
    HttpSlot,
    ApiSlot,
    DiskReadSlot,
    DiskWriteSlot,
    HashSlot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferBudget {
    pub file_slots: u16,
    pub checker_slots: u16,
    pub chunk_slots: u16,
    pub http_slots: u16,
    pub api_slots: u16,
    pub disk_read_slots: u16,
    pub disk_write_slots: u16,
    pub hash_slots: u16,
}

impl Default for TransferBudget {
    fn default() -> Self {
        Self {
            file_slots: 4,
            checker_slots: 8,
            chunk_slots: 1,
            http_slots: 8,
            api_slots: 4,
            disk_read_slots: 4,
            disk_write_slots: 4,
            hash_slots: 2,
        }
    }
}

impl TransferBudget {
    pub fn from_file_slots(file_slots: u16) -> Self {
        let file_slots = file_slots.max(1);
        Self {
            file_slots,
            disk_read_slots: file_slots,
            disk_write_slots: file_slots,
            ..Self::default()
        }
    }

    pub fn clamped_for_capabilities(mut self, caps: &TransferCapabilities) -> Self {
        if let Some(max) = caps.max_file_slots {
            self.file_slots = self.file_slots.min(max.max(1));
        }
        if let Some(max) = caps.max_checker_slots {
            self.checker_slots = self.checker_slots.min(max.max(1));
        }
        if let Some(max) = caps.max_chunk_slots {
            self.chunk_slots = self.chunk_slots.min(max.max(1));
        }
        self.http_slots = self.http_slots.max(1);
        self.api_slots = self.api_slots.max(1);
        self.disk_read_slots = self.disk_read_slots.max(1);
        self.disk_write_slots = self.disk_write_slots.max(1);
        self.hash_slots = self.hash_slots.max(1);
        self
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceRequest {
    pub file_slots: u16,
    pub checker_slots: u16,
    pub chunk_slots: u16,
    pub http_slots: u16,
    pub api_slots: u16,
    pub disk_read_slots: u16,
    pub disk_write_slots: u16,
    pub hash_slots: u16,
}

impl ResourceRequest {
    pub fn file_transfer() -> Self {
        Self {
            file_slots: 1,
            disk_read_slots: 1,
            disk_write_slots: 1,
            ..Self::default()
        }
    }

    pub fn checker() -> Self {
        Self {
            checker_slots: 1,
            ..Self::default()
        }
    }

    pub fn range_chunk() -> Self {
        Self {
            chunk_slots: 1,
            http_slots: 1,
            disk_write_slots: 1,
            ..Self::default()
        }
    }
}

#[derive(Clone)]
pub struct TransferResourceManager {
    budget: TransferBudget,
    file_slots: Arc<Semaphore>,
    checker_slots: Arc<Semaphore>,
    chunk_slots: Arc<Semaphore>,
    http_slots: Arc<Semaphore>,
    api_slots: Arc<Semaphore>,
    disk_read_slots: Arc<Semaphore>,
    disk_write_slots: Arc<Semaphore>,
    hash_slots: Arc<Semaphore>,
}

pub struct ResourceLease {
    _permits: Vec<OwnedSemaphorePermit>,
}

impl TransferResourceManager {
    pub fn new(budget: TransferBudget) -> Self {
        Self {
            budget,
            file_slots: Arc::new(Semaphore::new(budget.file_slots.max(1) as usize)),
            checker_slots: Arc::new(Semaphore::new(budget.checker_slots.max(1) as usize)),
            chunk_slots: Arc::new(Semaphore::new(budget.chunk_slots.max(1) as usize)),
            http_slots: Arc::new(Semaphore::new(budget.http_slots.max(1) as usize)),
            api_slots: Arc::new(Semaphore::new(budget.api_slots.max(1) as usize)),
            disk_read_slots: Arc::new(Semaphore::new(budget.disk_read_slots.max(1) as usize)),
            disk_write_slots: Arc::new(Semaphore::new(budget.disk_write_slots.max(1) as usize)),
            hash_slots: Arc::new(Semaphore::new(budget.hash_slots.max(1) as usize)),
        }
    }

    pub fn budget(&self) -> TransferBudget {
        self.budget
    }

    pub async fn acquire(&self, request: ResourceRequest) -> Result<ResourceLease, String> {
        let mut permits = Vec::new();

        acquire_many(
            &self.file_slots,
            request.file_slots,
            &mut permits,
            ResourceKind::FileSlot,
        )
        .await?;
        acquire_many(
            &self.checker_slots,
            request.checker_slots,
            &mut permits,
            ResourceKind::CheckerSlot,
        )
        .await?;
        acquire_many(
            &self.chunk_slots,
            request.chunk_slots,
            &mut permits,
            ResourceKind::ChunkSlot,
        )
        .await?;
        acquire_many(
            &self.http_slots,
            request.http_slots,
            &mut permits,
            ResourceKind::HttpSlot,
        )
        .await?;
        acquire_many(
            &self.api_slots,
            request.api_slots,
            &mut permits,
            ResourceKind::ApiSlot,
        )
        .await?;
        acquire_many(
            &self.disk_read_slots,
            request.disk_read_slots,
            &mut permits,
            ResourceKind::DiskReadSlot,
        )
        .await?;
        acquire_many(
            &self.disk_write_slots,
            request.disk_write_slots,
            &mut permits,
            ResourceKind::DiskWriteSlot,
        )
        .await?;
        acquire_many(
            &self.hash_slots,
            request.hash_slots,
            &mut permits,
            ResourceKind::HashSlot,
        )
        .await?;

        Ok(ResourceLease { _permits: permits })
    }
}

async fn acquire_many(
    semaphore: &Arc<Semaphore>,
    count: u16,
    permits: &mut Vec<OwnedSemaphorePermit>,
    kind: ResourceKind,
) -> Result<(), String> {
    for _ in 0..count {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| format!("resource manager closed while acquiring {:?}", kind))?;
        permits.push(permit);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_budget_to_capabilities() {
        let budget =
            TransferBudget::from_file_slots(8).clamped_for_capabilities(&TransferCapabilities {
                max_file_slots: Some(1),
                max_checker_slots: Some(2),
                max_chunk_slots: Some(1),
                ..TransferCapabilities::default()
            });

        assert_eq!(budget.file_slots, 1);
        assert_eq!(budget.checker_slots, 2);
        assert_eq!(budget.chunk_slots, 1);
    }

    #[test]
    fn file_slot_budget_preserves_legacy_batch_parallelism() {
        let budget = TransferBudget::from_file_slots(8);

        assert_eq!(budget.file_slots, 8);
        assert_eq!(budget.disk_read_slots, 8);
        assert_eq!(budget.disk_write_slots, 8);
    }

    #[tokio::test]
    async fn resource_lease_returns_permits_on_drop() {
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));
        let lease = manager
            .acquire(ResourceRequest::file_transfer())
            .await
            .unwrap();
        assert_eq!(manager.file_slots.available_permits(), 0);
        drop(lease);
        assert_eq!(manager.file_slots.available_permits(), 1);
    }
}
