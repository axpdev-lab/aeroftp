// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// AeroSync keeps one tab mounted at a time: the dialog renders
// `{activeTab === 'plan' && <PlanTabContent/>}`, so switching to Sync unmounts
// the Plan tab and React drops every `useState` it held. Every choice the user
// had made — preset, direction, conflict policy, verify policy, canary, budget,
// streams, compression — was gone on the way back, which made "schedule a Plan
// with these settings" impossible (discussion #347).
//
// The tabs stay unmounted (mounting all three would fire the Compare scan and
// the Sync effects on open), and their settings survive in this store instead:
// a plain key/value box owned by the dialog and read back as the initial value
// when a tab remounts. One dialog open == one store; it is reset on each open
// so a new AeroSync session never inherits the previous one's choices.

import * as React from 'react';

export interface TabStateStore {
  get<T>(key: string, fallback: T): T;
  set(key: string, value: unknown): void;
  has(key: string): boolean;
  subscribe(key: string, listener: (value: unknown) => void): () => void;
  reset(): void;
}

export const createTabStateStore = (): TabStateStore => {
  const values = new Map<string, unknown>();
  const listeners = new Map<string, Set<(value: unknown) => void>>();
  return {
    get<T>(key: string, fallback: T): T {
      return values.has(key) ? (values.get(key) as T) : fallback;
    },
    set(key: string, value: unknown): void {
      values.set(key, value);
      listeners.get(key)?.forEach((listener) => listener(value));
    },
    has(key: string): boolean {
      return values.has(key);
    },
    subscribe(key: string, listener: (value: unknown) => void): () => void {
      const keyListeners = listeners.get(key) ?? new Set<(value: unknown) => void>();
      keyListeners.add(listener);
      listeners.set(key, keyListeners);
      return () => {
        keyListeners.delete(listener);
        if (keyListeners.size === 0) listeners.delete(key);
      };
    },
    reset(): void {
      values.clear();
    },
  };
};

export const TabStateStoreContext = React.createContext<TabStateStore | null>(null);

/**
 * `useState` whose value outlives the component when a TabStateStore is in
 * context, and behaves exactly like `useState` when it is not (so a tab
 * component can still be rendered standalone).
 *
 * The key is namespaced by the caller, e.g. 'plan.preset'.
 */
export function useStickyState<T>(
  key: string,
  initial: T | (() => T),
): [T, React.Dispatch<React.SetStateAction<T>>] {
  const store = React.useContext(TabStateStoreContext);
  const [value, setValue] = React.useState<T>(() => {
    const fallback = typeof initial === 'function' ? (initial as () => T)() : initial;
    return store ? store.get<T>(key, fallback) : fallback;
  });
  const valueRef = React.useRef(value);
  React.useEffect(() => { valueRef.current = value; }, [value]);

  // Imports and other dialog-level actions write the store while a tab may be
  // mounted. Subscribe so its visible controls change immediately rather than
  // only after an unmount/remount cycle (#514).
  React.useEffect(() => {
    if (!store) return;
    return store.subscribe(key, (next) => {
      valueRef.current = next as T;
      setValue(next as T);
    });
  }, [store, key]);

  const set = React.useCallback<React.Dispatch<React.SetStateAction<T>>>((next) => {
    const resolved = typeof next === 'function'
      ? (next as (p: T) => T)(valueRef.current)
      : next;
    valueRef.current = resolved;
    setValue(resolved);
    store?.set(key, resolved);
  }, [store, key]);

  return [value, set];
}

/**
 * Guard for a *seeding* effect — one that derives state from another control and
 * therefore also fires on mount. After a tab switch the mount is a remount, and
 * re-seeding would overwrite the very values the store just restored (pick
 * "Turbo", raise the streams by hand, leave, come back: the streams snap back).
 *
 * Returns a ref that starts `true` when any of `keys` was restored. The effect
 * skips its first run and clears the flag, so a later real change still seeds.
 */
export function useSkipSeedOnRestore(...keys: string[]): React.MutableRefObject<boolean> {
  const store = React.useContext(TabStateStoreContext);
  return React.useRef(keys.some((k) => store?.has(k) ?? false));
}
