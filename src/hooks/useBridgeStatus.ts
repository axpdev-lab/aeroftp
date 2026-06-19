// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { BridgeKind } from '../components/providerModeGroups';

/** Mirror of the Rust `BridgeStatus` struct returned by `bridge_status`. */
export interface BridgeStatus {
    installed: boolean;
    active: boolean;
    port: number;
    /** False when installation could not be confidently determined (Filen
     *  Desktop when not running). The UI must NOT show 🔴 / block Save then. */
    install_known: boolean;
}

/** Visual state driving the 🔴/🟠/🟢 dot and the app-aware message. */
export type BridgeUiState = 'red' | 'amber' | 'green';

/**
 * Map a probe result to the dot state (pure, for tests):
 *   - green: bridge port is reachable (app installed and active);
 *   - red:   confidently not installed (the only Save-blocking state);
 *   - amber: installed but not active, OR installation is unknown — Save stays
 *            enabled so a user about to start the app is never blocked.
 *
 * A null result (probe in flight or failed) maps to amber: never block on doubt.
 */
export function deriveBridgeUiState(s: BridgeStatus | null): BridgeUiState {
    if (!s) return 'amber';
    if (s.active) return 'green';
    if (s.install_known && !s.installed) return 'red';
    return 'amber';
}

/** Re-probe cadence while the bridge tab is open (loopback connect is cheap). */
const POLL_INTERVAL_MS = 4000;

export interface UseBridgeStatusResult {
    status: BridgeStatus | null;
    uiState: BridgeUiState;
    /** True only in the confidently-not-installed state (drives Save gating). */
    saveBlocked: boolean;
    loading: boolean;
    refresh: () => void;
}

/**
 * Poll the backend `bridge_status` command for a local helper-app bridge
 * (Filen Desktop / MEGAcmd). Probes on mount / when `kind` changes, re-polls
 * every {@link POLL_INTERVAL_MS} while mounted, and exposes a manual `refresh`.
 * Pass `kind = undefined` (e.g. when the active mode is not a bridge) to idle.
 */
export function useBridgeStatus(
    kind: BridgeKind | undefined,
    port?: number,
): UseBridgeStatusResult {
    const [status, setStatus] = useState<BridgeStatus | null>(null);
    const [loading, setLoading] = useState<boolean>(!!kind);
    const mountedRef = useRef(true);

    const probe = useCallback(async () => {
        if (!kind) return;
        try {
            const res = await invoke<BridgeStatus>('bridge_status', { kind, port });
            if (mountedRef.current) setStatus(res);
        } catch {
            // Probe failed (non-Tauri context, command error): leave the last
            // known status; deriveBridgeUiState(null) stays amber, never red.
        } finally {
            if (mountedRef.current) setLoading(false);
        }
    }, [kind, port]);

    useEffect(() => {
        mountedRef.current = true;
        if (!kind) {
            setStatus(null);
            setLoading(false);
            return () => {
                mountedRef.current = false;
            };
        }
        setStatus(null);
        setLoading(true);
        probe();
        const id = window.setInterval(probe, POLL_INTERVAL_MS);
        return () => {
            mountedRef.current = false;
            window.clearInterval(id);
        };
    }, [kind, probe]);

    const uiState = deriveBridgeUiState(status);
    return {
        status,
        uiState,
        saveBlocked: uiState === 'red',
        loading,
        refresh: probe,
    };
}

export default useBridgeStatus;
