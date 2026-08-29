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
// The sweep is owned by this module rather than by the component, and that is
// the whole design. The first version kept it inside the effect, guarded by a
// ref so it would run once per mount, and it never produced a single result in
// the running app: StrictMode mounts, cleans up, and remounts, so the first
// pass was discarded by its own cleanup while the second returned early on a
// ref that had survived the remount. Every gate was green, the wiring read
// correctly, and the backend answered when called by hand from the same page.
// A cache plus a shared in-flight promise has no such state to get wrong: a
// late consumer joins the sweep already running, and the answer outlives any
// single mount, which is right because a config file does not appear halfway
// through a session.
//
// Probes run with bounded concurrency because they are not all pure path math:
// the rclone probe shells out to `rclone config file`.

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { GENERIC_BRIDGE_SOURCES, type BridgeSourceDescriptor } from '../components/bridge/bridgeSources';

const PROBE_CONCURRENCY = 4;

/** Bridge source id -> absolute path of the config found on this machine. */
export type DetectedBridgeConfigs = Record<string, string>;

/** Asks the backend about one source; null when that tool has no config here. */
export type BridgeConfigProbe = (source: string) => Promise<string | null>;

const tauriProbe: BridgeConfigProbe = source =>
    invoke<string | null>('detect_bridge_config', { source });

let cache: DetectedBridgeConfigs | null = null;
let inFlight: Promise<DetectedBridgeConfigs> | null = null;

async function probeAll(probe: BridgeConfigProbe): Promise<DetectedBridgeConfigs> {
    const queue = GENERIC_BRIDGE_SOURCES.map(s => s.id);
    const found: DetectedBridgeConfigs = {};

    const worker = async (): Promise<void> => {
        for (;;) {
            const id = queue.shift();
            if (id === undefined) return;
            try {
                const path = await probe(id);
                if (path) found[id] = path;
            } catch {
                // A source we cannot probe is simply not reported as found.
                // The tool stays in the list and stays pickable by hand.
            }
        }
    };

    await Promise.all(Array.from({ length: Math.min(PROBE_CONCURRENCY, queue.length) }, worker));
    return found;
}

/**
 * The detected configs, probing at most once per app run. Concurrent callers
 * share the sweep in progress instead of starting a second one.
 */
export function loadDetectedBridgeConfigs(
    probe: BridgeConfigProbe = tauriProbe,
): Promise<DetectedBridgeConfigs> {
    if (cache) return Promise.resolve(cache);
    if (!inFlight) {
        inFlight = probeAll(probe).then(result => {
            cache = result;
            inFlight = null;
            return result;
        });
    }
    return inFlight;
}

/** Test seam: forget both the cached answer and any sweep in progress. */
export function resetDetectedBridgeConfigsCache(): void {
    cache = null;
    inFlight = null;
}

export interface DetectedBridgeConfigsState {
    detected: DetectedBridgeConfigs;
    /** True while the sweep this consumer is waiting on has not landed yet. */
    probing: boolean;
}

/**
 * The detected configs for a component. Nothing is probed until `enabled`
 * turns true, so a caller that never opens the picker pays nothing.
 */
export function useDetectedBridgeConfigs(enabled: boolean): DetectedBridgeConfigsState {
    const [detected, setDetected] = useState<DetectedBridgeConfigs>(() => cache ?? {});
    const [probing, setProbing] = useState(false);

    useEffect(() => {
        if (!enabled) return;
        let cancelled = false;
        setProbing(true);
        void loadDetectedBridgeConfigs().then(result => {
            if (cancelled) return;
            setDetected(result);
            setProbing(false);
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
 * `…/filezilla/sitemanager.xml`.
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
