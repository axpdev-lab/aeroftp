// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};

use super::capabilities::TransferCapabilities;
use super::resources::TransferBudget;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferPlanContext {
    pub requested_budget: TransferBudget,
    pub effective_budget: TransferBudget,
    pub capabilities: TransferCapabilities,
    pub fallback_reason: Option<String>,
}

impl TransferPlanContext {
    pub fn new(requested_budget: TransferBudget, capabilities: TransferCapabilities) -> Self {
        let effective_budget = requested_budget.clamped_for_capabilities(&capabilities);
        let fallback_reason = (effective_budget != requested_budget)
            .then(|| "requested transfer budget was clamped by provider capabilities".to_string());

        Self {
            requested_budget,
            effective_budget,
            capabilities,
            fallback_reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_budget_fallback_when_caps_reduce_parallelism() {
        let plan = TransferPlanContext::new(
            TransferBudget::from_file_slots(4),
            TransferCapabilities {
                max_file_slots: Some(1),
                ..TransferCapabilities::default()
            },
        );

        assert_eq!(plan.effective_budget.file_slots, 1);
        assert!(plan.fallback_reason.is_some());
    }
}
