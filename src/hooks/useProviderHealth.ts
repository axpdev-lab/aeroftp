// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

/**
 * useProviderHealth — Per-tab progressive provider health scan.
 *
 * - Scans only the providers visible in the current tab
 * - Results arrive progressively via Tauri events (wave scan effect)
 * - Shows "pending" state for providers being scanned
 * - Cached per-provider for 5 minutes
 * - Offline-safe: skips scan if navigator.onLine is false
 */

import { useState, useCallback, useRef, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { createTauriListener } from './useTauriListener';

export type HealthStatus = 'up' | 'slow' | 'down' | 'pending' | 'unknown';

export interface ProviderHealthState {
    status: HealthStatus;
    latencyMs: number;
}

export interface HealthTarget {
    id: string;
    url: string;
    /** Optional — when provided the backend probes via TCP for ftp/ftps/sftp. */
    protocol?: string;
    host?: string;
    port?: number;
}

/** Cache duration: 5 minutes */
const CACHE_TTL_MS = 5 * 60 * 1000;

/** Safety timeout: max time a scan can take before auto-reset.
 *  Bumped from 30s → 90s for users with 50+ saved servers, where the wave
 *  scan worst-case (count / MAX_CONCURRENT × per-probe timeout) blows past
 *  half a minute. The hook still fires `health-scan-complete` on the happy
 *  path; this is just the hung-scan fallback. */
const SCAN_SAFETY_TIMEOUT_MS = 90_000;

/** Module-level cache (survives re-renders and re-mounts) */
const healthCache: Map<string, { state: ProviderHealthState; timestamp: number }> = new Map();

/** Generation counter — increments each scan, used to make stable scan IDs */
let scanGeneration = 0;

/**
 * External mark-as-healthy entry point. Used by the connect flow so a
 * successful `provider_connect` immediately flips the My Servers card dot
 * from "unknown / pending" to green without waiting for the next batched
 * health scan. Notifies every mounted `useProviderHealth` instance via a
 * window custom event so subscribers re-read the cache.
 */
export function markProfileHealthy(id: string, latencyMs = 0): void {
    if (!id) return;
    healthCache.set(id, {
        state: { status: 'up', latencyMs },
        timestamp: Date.now(),
    });
    if (typeof window !== 'undefined') {
        window.dispatchEvent(new CustomEvent('provider-health-updated', { detail: { id } }));
    }
}

export function useProviderHealth() {
    const [results, setResults] = useState<Map<string, ProviderHealthState>>(new Map());
    const [scanning, setScanning] = useState(false);
    // Per-hook-instance listener disposers — prevents cross-mount pollution.
    const unlistenRef = useRef<Array<() => void>>([]);
    // Per-instance safety timer — previously module-level, which meant two
    // concurrent mounts would clobber each other's timer reference.
    const safetyTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
    const scanInProgressRef = useRef(false);
    const activeScanIdRef = useRef<string | null>(null);
    const mountedRef = useRef(true);

    const resetScanState = useCallback(() => {
        scanInProgressRef.current = false;
        activeScanIdRef.current = null;
        if (safetyTimerRef.current) {
            clearTimeout(safetyTimerRef.current);
            safetyTimerRef.current = null;
        }
    }, []);

    // Cleanup listeners on unmount + reset scan lock
    useEffect(() => {
        mountedRef.current = true;
        return () => {
            mountedRef.current = false;
            unlistenRef.current.forEach(fn => fn());
            unlistenRef.current = [];
            // Critical: reset the module-level lock so future mounts can scan
            resetScanState();
        };
    }, [resetScanState]);

    // Build current view from cache
    const syncFromCache = useCallback(() => {
        if (!mountedRef.current) return;
        const now = Date.now();
        const view = new Map<string, ProviderHealthState>();
        for (const [id, entry] of healthCache) {
            if (now - entry.timestamp < CACHE_TTL_MS) {
                view.set(id, entry.state);
            }
        }
        setResults(view);
    }, []);

    // External cache writes (e.g. markProfileHealthy on connect success): rebuild
    // the local view so the dot flips immediately without waiting for a scan.
    useEffect(() => {
        const onUpdate = () => syncFromCache();
        window.addEventListener('provider-health-updated', onUpdate);
        return () => window.removeEventListener('provider-health-updated', onUpdate);
    }, [syncFromCache]);

    /**
     * Scan a specific list of items (the currently visible tab).
     * Skips items already cached (unless force=true).
     */
    const scanItems = useCallback(async (items: HealthTarget[], force = false) => {
        if (!navigator.onLine) return;
        const allowInterrupt = force && items.length === 1;
        if (scanInProgressRef.current && !allowInterrupt) return;
        if (allowInterrupt && safetyTimerRef.current) {
            clearTimeout(safetyTimerRef.current);
            safetyTimerRef.current = null;
        }

        const now = Date.now();

        // Filter to items that need scanning
        const toScan = items.filter(item => {
            if (!item.url) return false;
            if (force) return true;
            const cached = healthCache.get(item.id);
            return !cached || (now - cached.timestamp >= CACHE_TTL_MS);
        });

        // Set pending state for items about to be scanned
        if (toScan.length > 0) {
            for (const item of toScan) {
                healthCache.set(item.id, {
                    state: { status: 'pending', latencyMs: 0 },
                    timestamp: now,
                });
            }
            syncFromCache();
        } else {
            // All cached, just sync
            syncFromCache();
            return;
        }

        // Capture generation for this scan
        const scanId = `health-${Date.now()}-${++scanGeneration}`;
        const activeIds = new Set(toScan.map(item => item.id));
        activeScanIdRef.current = scanId;
        scanInProgressRef.current = true;
        if (mountedRef.current) setScanning(true);

        // Safety timeout: auto-reset if scan hangs
        safetyTimerRef.current = setTimeout(() => {
            if (scanInProgressRef.current && activeScanIdRef.current === scanId) {
                resetScanState();
                if (mountedRef.current) setScanning(false);
            }
        }, SCAN_SAFETY_TIMEOUT_MS);

        // Register listeners FIRST so no event can slip through the gap
        // between the old cleanup and the new subscription. createTauriListener
        // returns a synchronous disposer and handles late resolution safely.
        const unlisten1 = createTauriListener<{ id: string; status: string; latency_ms: number; scan_id?: string }>(
            'health-scan-result',
            (event) => {
                if (activeScanIdRef.current !== scanId) return; // stale event
                const { id, status, latency_ms, scan_id } = event.payload;
                if (scan_id && scan_id !== scanId) return;
                if (!activeIds.has(id)) return;
                healthCache.set(id, {
                    state: { status: status as HealthStatus, latencyMs: latency_ms },
                    timestamp: Date.now(),
                });
                syncFromCache();
            },
        );

        const unlisten2 = createTauriListener<{ scan_id?: string }>('health-scan-complete', (event) => {
            if (activeScanIdRef.current !== scanId) return; // stale event
            if (event.payload?.scan_id && event.payload.scan_id !== scanId) return;
            resetScanState();
            if (mountedRef.current) setScanning(false);
        });

        // Swap in the new listeners atomically after they are registered.
        const previous = unlistenRef.current;
        unlistenRef.current = [unlisten1, unlisten2];
        previous.forEach(fn => fn());

        // Fire scan
        try {
            await invoke('start_health_scan', {
                targets: toScan.map(t => ({
                    id: t.id,
                    url: t.url,
                    protocol: t.protocol,
                    host: t.host,
                    port: t.port,
                })),
                scanId,
            });
        } catch {
            if (activeScanIdRef.current === scanId) {
                resetScanState();
                if (mountedRef.current) setScanning(false);
            }
        }
    }, [syncFromCache, resetScanState]);

    const getStatus = useCallback((id: string): ProviderHealthState => {
        return results.get(id) || { status: 'unknown', latencyMs: 0 };
    }, [results]);

    return { results, scanning, scanItems, getStatus };
}
