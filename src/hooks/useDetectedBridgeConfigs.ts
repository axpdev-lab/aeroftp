// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Which of the 15 bridge tools have a config file on THIS machine.
//
// `detect_bridge_config` (src-tauri/src/bridge_commands.rs) resolves a tool's
// conventional config path per platform and returns Some only when that file
// exists, so a returned path means "installed and configured here". The
// Export/Import dialog used to ask this one tool at a time, and only after
// the user had already picked one (BridgeSourcePanel); asking for all of them
// while the picker is on screen is what lets the tools you actually use rise
// to the top of the list instead of hiding at position 11 of 15.
//
// Probes run with bounded concurrency because they are not all pure path
// math: the rclone probe shells out to `rclone config file`. Results stream
// in as they land, so the list renders immediately and decorates itself
// rather than waiting for the slowest probe.

import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { GENERIC_BRIDGE_SOURCES, type BridgeSourceDescriptor } from '../components/bridge/bridgeSources';

const PROBE_CONCURRENCY = 4;

/** Bridge source id -> absolute path of the config found on this machine. */
export type DetectedBridgeConfigs = Record<string, string>;

export interface DetectedBridgeConfigsState {
    detected: DetectedBridgeConfigs;
    /** True while at least one probe is still outstanding. */
    probing: boolean;
}

/**
 * Probe every bridge source once, the first time `enabled` turns true.
 * Disabled callers pay nothing: no probe is issued until the picker is shown.
 */
export function useDetectedBridgeConfigs(enabled: boolean): DetectedBridgeConfigsState {
    const [detected, setDetected] = useState<DetectedBridgeConfigs>({});
    const [probing, setProbing] = useState(false);
    // One sweep per mount. Without this, toggling back and forth between the
    // import and export pickers would re-probe (and re-spawn rclone) on every
    // switch, for an answer that cannot have changed meanwhile.
    const started = useRef(false);

    useEffect(() => {
        if (!enabled || started.current) return;
        started.current = true;

        let cancelled = false;
        const queue = GENERIC_BRIDGE_SOURCES.map(s => s.id);
        setProbing(true);

        const worker = async (): Promise<void> => {
            for (;;) {
                const id = queue.shift();
                if (id === undefined || cancelled) return;
                try {
                    const path = await invoke<string | null>('detect_bridge_config', { source: id });
                    if (!cancelled && path) {
                        setDetected(prev => ({ ...prev, [id]: path }));
                    }
                } catch {
                    // A source we cannot probe is simply not reported as found.
                    // The tool stays in the list and stays pickable by hand.
                }
            }
        };

        void Promise.all(
            Array.from({ length: Math.min(PROBE_CONCURRENCY, queue.length) }, worker),
        ).finally(() => {
            if (!cancelled) setProbing(false);
        });

        return () => {
            cancelled = true;
        };
    }, [enabled]);

    return { detected, probing };
}

/**
 * Last two segments of a config path, prefixed with an ellipsis when anything
 * was dropped: `/home/me/.config/filezilla/sitemanager.xml` becomes
 * `.../filezilla/sitemanager.xml`.
 *
 * The row shows the path rather than a "found on this computer" label, so the
 * information carries no translated string. Plain CSS truncation would defeat
 * that: it cuts the tail, and the tail is the only part that differs between
 * one tool's config and another's. The full path stays in the row's tooltip.
 */
export function shortenConfigPath(path: string): string {
    const parts = path.split(/[\\/]+/).filter(Boolean);
    if (parts.length <= 2) return path;
    return `…/${parts.slice(-2).join('/')}`;
}

/**
 * Tools whose config was found on this machine first, everything else after,
 * curated order preserved inside each group (Array.sort is stable).
 *
 * Kept out of the JSX so it can be tested without rendering the dialog.
 */
export function orderBridgeSourcesByDetection<T extends BridgeSourceDescriptor>(
    sources: readonly T[],
    detected: DetectedBridgeConfigs,
): T[] {
    return [...sources].sort((a, b) => (detected[b.id] ? 1 : 0) - (detected[a.id] ? 1 : 0));
}
