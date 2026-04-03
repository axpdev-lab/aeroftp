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
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export type HealthStatus = 'up' | 'slow' | 'down' | 'pending' | 'unknown';

export interface ProviderHealthState {
    status: HealthStatus;
    latencyMs: number;
}

export interface HealthTarget {
    id: string;
    url: string;
}

/** Cache duration: 5 minutes */
const CACHE_TTL_MS = 5 * 60 * 1000;

/** Safety timeout: max time a scan can take before auto-reset (30s) */
const SCAN_SAFETY_TIMEOUT_MS = 30_000;

/** Module-level cache (survives re-renders and re-mounts) */
const healthCache: Map<string, { state: ProviderHealthState; timestamp: number }> = new Map();

/** Generation counter — increments each scan, used to ignore stale events */
let scanGeneration = 0;
let scanInProgress = false;
let safetyTimer: ReturnType<typeof setTimeout> | null = null;

function resetScanState() {
    scanInProgress = false;
    if (safetyTimer) {
        clearTimeout(safetyTimer);
        safetyTimer = null;
    }
}

export function useProviderHealth() {
    const [results, setResults] = useState<Map<string, ProviderHealthState>>(new Map());
    const [scanning, setScanning] = useState(false);
    const unlistenRef = useRef<UnlistenFn[]>([]);
    const mountedRef = useRef(true);

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
    }, []);

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

    /**
     * Scan a specific list of items (the currently visible tab).
     * Skips items already cached (unless force=true).
     */
    const scanItems = useCallback(async (items: HealthTarget[], force = false) => {
        if (!navigator.onLine) return;
        if (scanInProgress) return;

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
        const gen = ++scanGeneration;
        scanInProgress = true;
        if (mountedRef.current) setScanning(true);

        // Safety timeout: auto-reset if scan hangs
        safetyTimer = setTimeout(() => {
            if (scanInProgress && scanGeneration === gen) {
                resetScanState();
                if (mountedRef.current) setScanning(false);
            }
        }, SCAN_SAFETY_TIMEOUT_MS);

        // Cleanup previous listeners
        unlistenRef.current.forEach(fn => fn());
        unlistenRef.current = [];

        // Listen for progressive results (only accept events from current generation)
        const unlisten1 = await listen<{ id: string; status: string; latency_ms: number }>(
            'health-scan-result',
            (event) => {
                if (scanGeneration !== gen) return; // stale event
                const { id, status, latency_ms } = event.payload;
                healthCache.set(id, {
                    state: { status: status as HealthStatus, latencyMs: latency_ms },
                    timestamp: Date.now(),
                });
                syncFromCache();
            }
        );

        const unlisten2 = await listen('health-scan-complete', () => {
            if (scanGeneration !== gen) return; // stale event
            resetScanState();
            if (mountedRef.current) setScanning(false);
        });

        unlistenRef.current.push(unlisten1, unlisten2);

        // Fire scan
        try {
            await invoke('start_health_scan', {
                targets: toScan.map(t => ({ id: t.id, url: t.url })),
            });
        } catch {
            if (scanGeneration === gen) {
                resetScanState();
                if (mountedRef.current) setScanning(false);
            }
        }
    }, [syncFromCache]);

    const getStatus = useCallback((id: string): ProviderHealthState => {
        return results.get(id) || { status: 'unknown', latencyMs: 0 };
    }, [results]);

    return { results, scanning, scanItems, getStatus };
}
