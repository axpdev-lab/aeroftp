// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useState } from 'react';

/**
 * The operations a modal has in flight, by key (a row id, a file name).
 *
 * Rows can run operations at the same time, so one "busy row" value is not
 * enough: starting B overwrote A's marker and re-enabled A's row while A
 * still ran, and the first operation to finish cleared the marker, and the
 * busy close guard with it, while the other was still pending (Discussion
 * #1527). Each key counts its own pending operations, so a row stays busy
 * until every operation on it settles, and `count` stays above zero while
 * any operation runs.
 *
 * Call `begin(key)` before the request and `end(key)` in its `finally`.
 */
export interface InFlight<K> {
  /** True while at least one operation on `key` is pending. */
  has: (key: K) => boolean;
  /** Pending operations across all keys. */
  count: number;
  begin: (key: K) => void;
  end: (key: K) => void;
}

export function useInFlight<K>(): InFlight<K> {
  const [pending, setPending] = useState<ReadonlyMap<K, number>>(() => new Map());
  const begin = useCallback((key: K) => setPending(prev => {
    const next = new Map(prev);
    next.set(key, (next.get(key) ?? 0) + 1);
    return next;
  }), []);
  const end = useCallback((key: K) => setPending(prev => {
    const left = (prev.get(key) ?? 0) - 1;
    const next = new Map(prev);
    if (left > 0) next.set(key, left); else next.delete(key);
    return next;
  }), []);
  let count = 0;
  pending.forEach(n => { count += n; });
  return { has: (key: K) => (pending.get(key) ?? 0) > 0, count, begin, end };
}
