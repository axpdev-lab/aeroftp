// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect, useRef } from 'react';
import { listen, type EventCallback, type UnlistenFn } from '@tauri-apps/api/event';

/**
 * Tauri event listener with a synchronous disposable shell.
 *
 * The standard pattern — `const p = listen(...); return () => p.then(fn => fn())` —
 * is teardown-racy: if cleanup runs before the promise resolves, the unlisten
 * function never runs and the listener leaks. Under React StrictMode every
 * effect is setup/cleanup/setup-tested, so the race is guaranteed in development.
 *
 * This hook owns a synchronous `disposed` flag and a late-resolution guard:
 * when `listen()` resolves after cleanup, the freshly-registered handler is
 * dropped immediately instead of being stored.
 *
 * The handler is captured via a ref so changing handler identity does not
 * re-subscribe. To re-subscribe on dependency change, pass `deps`.
 *
 * @example
 * useTauriListener<TransferEvent>('transfer_event', (ev) => {
 *   dispatch(ev.payload);
 * });
 */
export function useTauriListener<T>(
    event: string,
    handler: EventCallback<T>,
    deps: React.DependencyList = [],
    options?: { enabled?: boolean },
): void {
    const handlerRef = useRef(handler);
    // Keep the ref current without re-subscribing.
    useEffect(() => {
        handlerRef.current = handler;
    }, [handler]);

    const enabled = options?.enabled ?? true;

    useEffect(() => {
        if (!enabled) return;

        let disposed = false;
        let off: UnlistenFn | null = null;

        void listen<T>(event, (payload) => {
            // Late events that arrive after unmount are ignored — the listener
            // may not have been unregistered yet if the Tauri bridge is slow.
            if (disposed) return;
            handlerRef.current(payload);
        }).then((fn) => {
            if (disposed) {
                // Cleanup already ran: drop the freshly-registered listener
                // instead of leaving it orphaned.
                fn();
            } else {
                off = fn;
            }
        });

        return () => {
            disposed = true;
            if (off) {
                off();
                off = null;
            }
        };
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [event, enabled, ...deps]);
}

/**
 * Imperative variant for call sites that need to register listeners outside
 * of React's lifecycle (e.g. inside a click handler that spans the whole
 * modal lifetime). Returns a disposer function that is safe to call multiple
 * times and before the underlying `listen()` resolves.
 *
 * Prefer {@link useTauriListener} whenever the listener's lifetime matches a
 * component effect; use this only when the registration is intrinsically
 * imperative.
 */
export function createTauriListener<T>(
    event: string,
    handler: EventCallback<T>,
): () => void {
    return guardedUnlisten(listen<T>(event, (payload) => handler(payload)));
}

/**
 * Take any promise that resolves to an `UnlistenFn` (e.g. `listen()`,
 * `getCurrentWindow().onResized()`, Tauri plugin event helpers) and return
 * a synchronous disposer with the same late-resolution safety as
 * {@link useTauriListener}: if the disposer runs before the promise
 * resolves, the eventual `UnlistenFn` is called immediately on resolve.
 *
 * Use this to wrap any `Promise<UnlistenFn>` source without having to
 * reimplement the guard by hand.
 */
export function guardedUnlisten(source: Promise<UnlistenFn>): () => void {
    let disposed = false;
    let off: UnlistenFn | null = null;

    void source.then((fn) => {
        if (disposed) {
            fn();
        } else {
            off = fn;
        }
    });

    return () => {
        if (disposed) return;
        disposed = true;
        if (off) {
            off();
            off = null;
        }
    };
}
