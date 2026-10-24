// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Drains the queue of files the OS handed to the app (`open_request.rs`):
 * a double-click on an associated format, "Open with AeroFTP" on any file
 * or on a multi-selection, `aeroftp <path>`, a macOS open event.
 *
 * The backend parks each request and emits `open-request`; nothing is taken
 * until `ready` (vault boot done, app unlocked, local panel bootstrapped),
 * then the queue is drained and again on every later event. The first launch
 * used to emit the path after a guessed 1.5 s delay instead, so a window
 * still booting or behind the lock screen had no listener yet and the file
 * was dropped.
 *
 * One drain runs at a time, through the handling of what it took: a second
 * navigation started while the first is in flight is refused by the panel,
 * and its entries would already have left the queue. An event that arrives
 * meanwhile makes the running drain take the queue once more when done.
 */

import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { OpenRequest } from '../types';
import { useTauriListener } from './useTauriListener';
import { logger } from '../utils/logger';

export function useOpenRequests(ready: boolean, handle: (requests: OpenRequest[]) => Promise<void> | void): void {
    const handleRef = useRef(handle);
    handleRef.current = handle;
    const readyRef = useRef(ready);
    readyRef.current = ready;
    const running = useRef(false);
    const again = useRef(false);
    const [signal, setSignal] = useState(0);
    useTauriListener('open-request', () => setSignal(n => n + 1));
    useEffect(() => {
        if (!ready) return;
        if (running.current) {
            again.current = true;
            return;
        }
        running.current = true;
        void (async () => {
            try {
                do {
                    again.current = false;
                    const requests = await invoke<OpenRequest[]>('take_open_requests');
                    // Taken entries have left the queue, so they are handled
                    // even if the app locks meanwhile: what they open sits
                    // under the lock screen.
                    if (requests.length > 0) await handleRef.current(requests);
                } while (again.current && readyRef.current);
            } catch (err) {
                logger.error('[OpenRequest] could not open the queued files', err);
            } finally {
                running.current = false;
            }
        })();
    }, [ready, signal]);
}
