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

// What the running sweep has found so far, and who wants to hear about it.
// Results are published as they land rather than only at the end, for two
// reasons. A probe that never answers (rclone shelling out to a binary that
// hangs) would otherwise hold the entire answer hostage: with a final-only
// result the other fourteen tools stay invisible forever. And the dialog this
// feeds is currently on a machine where the page has a few healthy seconds to
// be read, so an answer that arrives in pieces is worth more than a complete
// one that arrives last.
let partial: DetectedBridgeConfigs = {};
type Listener = (found: DetectedBridgeConfigs) => void;
const listeners = new Set<Listener>();

function publish(): void {
    const snapshot = { ...partial };
    listeners.forEach(listener => listener(snapshot));
}

/** Hear about detections as they land. Returns the unsubscribe function. */
export function subscribeDetectedBridgeConfigs(listener: Listener): () => void {
    listeners.add(listener);
    return () => {
        listeners.delete(listener);
    };
}

async function probeAll(probe: BridgeConfigProbe): Promise<DetectedBridgeConfigs> {
    const queue = GENERIC_BRIDGE_SOURCES.map(s => s.id);

    const worker = async (): Promise<void> => {
        for (;;) {
            const id = queue.shift();
            if (id === undefined) return;
            try {
                const path = await probe(id);
                if (path) {
                    partial[id] = path;
                    publish();
                }
            } catch {
                // A source we cannot probe is simply not reported as found.
                // The tool stays in the list and stays pickable by hand.
            }
        }
    };

    await Promise.all(Array.from({ length: Math.min(PROBE_CONCURRENCY, queue.length) }, worker));
    return { ...partial };
}

/**
 * The detected configs, probing at most once per app run. Concurrent callers
 * share the sweep in progress instead of starting a second one. The promise
 * resolves when every probe has answered; a consumer that cannot wait for the
 * slowest one subscribes instead.
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

/**
 * How long a single-source probe may take before the UI stops waiting on it.
 * Not a cancellation: the backend call runs on, we simply stop letting it hold
 * a spinner. Six seconds is far above a normal answer (a path lookup, or one
 * `rclone config file` spawn) and far below a user's patience.
 */
export const BRIDGE_PROBE_TIMEOUT_MS = 6000;

/**
 * The config path for ONE source, for a caller that has a spinner on screen:
 * the sweep's answer when it already has one, otherwise a probe that gives up
 * rather than spinning forever. Resolves to '' for "no config here", which is
 * also what a timeout looks like, because both mean the same thing to the user:
 * pick the file by hand.
 *
 * A hanging probe used to leave BridgeSourcePanel on an eternal "Detecting..."
 * with no way to reach the browse button.
 */
export function detectBridgeConfigBounded(
    source: string,
    probe: BridgeConfigProbe = tauriProbe,
    timeoutMs: number = BRIDGE_PROBE_TIMEOUT_MS,
): Promise<string> {
    const known = (cache ?? partial)[source];
    if (known) return Promise.resolve(known);
    return new Promise<string>(resolve => {
        let settled = false;
        const finish = (value: string) => {
            if (settled) return;
            settled = true;
            clearTimeout(timer);
            resolve(value);
        };
        const timer = setTimeout(() => finish(''), timeoutMs);
        probe(source).then(path => finish(path || '')).catch(() => finish(''));
    });
}

/** What the sweep has found so far, empty before it starts. */
export function detectedBridgeConfigsSoFar(): DetectedBridgeConfigs {
    return cache ?? { ...partial };
}

/** Test seam: forget the cached answer, any sweep in progress, and listeners. */
export function resetDetectedBridgeConfigsCache(): void {
    cache = null;
    inFlight = null;
    partial = {};
    listeners.clear();
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
    const [detected, setDetected] = useState<DetectedBridgeConfigs>(detectedBridgeConfigsSoFar);
    const [probing, setProbing] = useState(false);

    useEffect(() => {
        if (!enabled) return;
        let cancelled = false;
        setProbing(true);
        // Subscribe first, then start (or join) the sweep: a result that lands
        // between the two calls would otherwise be missed.
        const unsubscribe = subscribeDetectedBridgeConfigs(found => {
            if (!cancelled) setDetected(found);
        });
        setDetected(detectedBridgeConfigsSoFar());
        void loadDetectedBridgeConfigs().then(result => {
            if (cancelled) return;
            setDetected(result);
            setProbing(false);
        });
        return () => {
            cancelled = true;
            unsubscribe();
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
