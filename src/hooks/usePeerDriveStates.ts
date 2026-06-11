// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * AeroShare live drive-state hook (Phase 1 task 10).
 *
 * Owns the per-drive replication/serving state used to badge friend cards in
 * My Servers and to drive the handshake dialog progress. State comes from two
 * sources, reconciled into one `Map<namespace, PeerDriveBadge>`:
 *   - an initial (and on-demand) `peer_drives_list` pull, which reports the
 *     authoritative `syncing`/`serving` booleans, and
 *   - the `peer://sync-status` / `peer://share-status` events the PeerRuntime
 *     emits as tasks transition (starting | syncing | serving | error |
 *     stopped). There is NO per-file progress in Phase 1: render state, not
 *     percentages (handoff §2).
 *
 * Only active while `enabled` (the AeroShare experimental flag): when off it
 * holds no listeners and returns an empty map.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { peerDrivesList } from '../utils/aeroShare';

// 'live' = the replica has converged and is just watching for updates (the
// backend emits it after a replicate pass completes). Distinct from 'syncing'
// (a pass is actively pulling) so the friend dot can settle green when idle.
export type PeerDriveState = 'starting' | 'syncing' | 'live' | 'serving' | 'error' | 'stopped';

export interface PeerDriveBadge {
  /** Most recent state for the drive (event- or list-derived). */
  state: PeerDriveState;
  /** A replication task is converging the drive right now. */
  syncing: boolean;
  /** A publish task is serving the drive right now. */
  serving: boolean;
  /** Error text from the last `error` event, if any. */
  detail?: string | null;
  /** Monotonic-ish event timestamp (ms) for staleness ordering. */
  atMs: number;
}

interface PeerSyncEvent {
  namespace: string;
  state: string;
  detail?: string | null;
  at_ms: number;
}

interface PeerShareEvent {
  dir: string;
  namespace?: string | null;
  state: string;
  detail?: string | null;
  at_ms: number;
}

const derive = (syncing: boolean, serving: boolean, lastState: PeerDriveState): PeerDriveState => {
  if (lastState === 'error') return 'error';
  if (syncing) return 'syncing';
  if (serving) return 'serving';
  return lastState;
};

export interface UsePeerDriveStates {
  /** namespace -> live badge. */
  states: Map<string, PeerDriveBadge>;
  /** Re-pull `peer_drives_list` (e.g. after a handshake). */
  refresh: () => void;
}

export const usePeerDriveStates = (enabled: boolean): UsePeerDriveStates => {
  const [states, setStates] = useState<Map<string, PeerDriveBadge>>(new Map());
  // Refresh trigger that survives re-renders without re-subscribing listeners.
  const [tick, setTick] = useState(0);
  const refresh = useCallback(() => setTick((n) => n + 1), []);

  // Keep a ref so the event handlers always merge onto the latest map without
  // resubscribing on every state change.
  const statesRef = useRef(states);
  statesRef.current = states;

  const apply = useCallback((ns: string, next: Partial<PeerDriveBadge> & { atMs: number }) => {
    setStates((prev) => {
      const cur = prev.get(ns);
      const syncing = next.syncing ?? cur?.syncing ?? false;
      const serving = next.serving ?? cur?.serving ?? false;
      const lastState = (next.state ?? cur?.state ?? 'stopped') as PeerDriveState;
      const merged: PeerDriveBadge = {
        syncing,
        serving,
        detail: next.detail ?? cur?.detail ?? null,
        atMs: next.atMs,
        state: derive(syncing, serving, lastState),
      };
      const out = new Map(prev);
      out.set(ns, merged);
      return out;
    });
  }, []);

  // Initial + on-demand pull of the authoritative booleans.
  useEffect(() => {
    if (!enabled) {
      setStates(new Map());
      return;
    }
    let cancelled = false;
    peerDrivesList()
      .then((drives) => {
        if (cancelled) return;
        setStates((prev) => {
          const out = new Map(prev);
          for (const d of drives) {
            const cur = out.get(d.namespace);
            const lastState = (cur?.state ?? 'stopped') as PeerDriveState;
            out.set(d.namespace, {
              syncing: d.syncing,
              serving: d.serving,
              detail: cur?.detail ?? null,
              atMs: cur?.atMs ?? 0,
              state: derive(d.syncing, d.serving, lastState),
            });
          }
          return out;
        });
      })
      .catch(() => {
        /* no identity / not ready: leave the map as-is */
      });
    return () => {
      cancelled = true;
    };
  }, [enabled, tick]);

  // Event subscriptions (only while enabled).
  useEffect(() => {
    if (!enabled) return;
    let active = true;
    const unlisteners: Array<() => void> = [];

    const syncP = listen<PeerSyncEvent>('peer://sync-status', (e) => {
      const p = e.payload;
      const stopped = p.state === 'stopped' || p.state === 'error';
      // 'live' means converged-and-idle: NOT syncing, so derive() settles the
      // badge to 'live' (green) instead of being pinned to 'syncing' (blue).
      apply(p.namespace, {
        state: p.state as PeerDriveState,
        syncing: !stopped && p.state !== 'live',
        detail: p.state === 'error' ? p.detail ?? null : null,
        atMs: p.at_ms,
      });
    });
    const shareP = listen<PeerShareEvent>('peer://share-status', (e) => {
      const p = e.payload;
      if (!p.namespace) return; // namespace is null until the drive is ready
      const stopped = p.state === 'stopped' || p.state === 'error';
      apply(p.namespace, {
        state: (p.state === 'serving' ? 'serving' : (p.state as PeerDriveState)),
        serving: !stopped,
        detail: p.state === 'error' ? p.detail ?? null : null,
        atMs: p.at_ms,
      });
    });

    void syncP.then((u) => (active ? unlisteners.push(u) : u()));
    void shareP.then((u) => (active ? unlisteners.push(u) : u()));

    return () => {
      active = false;
      unlisteners.forEach((u) => u());
    };
  }, [enabled, apply]);

  return { states, refresh };
};

export default usePeerDriveStates;
