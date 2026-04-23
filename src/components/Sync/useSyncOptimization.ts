// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Hook for fetching per-provider transfer optimization hints
 */

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { TransferOptimizationHints, ProviderType } from "../../types";

export function useSyncOptimization(
  protocol?: ProviderType,
  refreshKey?: unknown,
): {
  hints: TransferOptimizationHints | null;
  loading: boolean;
} {
  const [hints, setHints] = useState<TransferOptimizationHints | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!protocol) {
      setHints(null);
      return;
    }

    let cancelled = false;
    setLoading(true);

    invoke<TransferOptimizationHints>("get_transfer_optimization_hints", {
      providerType: protocol,
    })
      .then((h) => {
        if (!cancelled) setHints(h);
      })
      .catch(() => {
        if (!cancelled) setHints(null);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [protocol, refreshKey]);

  return { hints, loading };
}
