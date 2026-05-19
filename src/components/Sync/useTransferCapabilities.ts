// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Hook for fetching per-provider transfer capabilities.
 *
 * Backed by the `get_transfer_capabilities` Tauri command (single source of
 * truth: TransferCapabilities::from_provider_hints). The GUI consumes this so
 * speed presets and parallel-stream controls reflect what the backend can
 * actually honor instead of advertising parallelism it will never apply.
 */

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { TransferCapabilities, Capability, ProviderType } from "../../types";

/** Mirrors Rust Capability::is_available. */
export function isCapabilityAvailable(c?: Capability | null): boolean {
  return (
    c === "supported" || c === "supported_after_probe" || c === "experimental"
  );
}

export function useTransferCapabilities(
  protocol?: ProviderType,
  refreshKey?: unknown,
): {
  caps: TransferCapabilities | null;
  loading: boolean;
} {
  const [caps, setCaps] = useState<TransferCapabilities | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!protocol) {
      setCaps(null);
      return;
    }

    let cancelled = false;
    setLoading(true);

    invoke<TransferCapabilities>("get_transfer_capabilities", {
      providerType: protocol,
    })
      .then((c) => {
        if (!cancelled) setCaps(c);
      })
      .catch(() => {
        if (!cancelled) setCaps(null);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [protocol, refreshKey]);

  return { caps, loading };
}
