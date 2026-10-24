// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Drains the queue of files the OS handed to the app (`open_request.rs`):
 * a double-click on an associated format, "Open with AeroFTP" on any file,
 * `aeroftp <path>`, a macOS open event.
 *
 * The backend parks each request and emits `open-request`; nothing is taken
 * until `ready` (vault boot done, app unlocked), then the queue is drained
 * once and again on every later event. The first launch used to emit the
 * path after a guessed 1.5 s delay instead, so a window still booting or
 * behind the lock screen had no listener yet and the file was dropped.
 */

import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { OpenRequest } from '../types';
import { useTauriListener } from './useTauriListener';
import { logger } from '../utils/logger';

export function useOpenRequests(ready: boolean, handle: (request: OpenRequest) => Promise<void> | void): void {
    const handleRef = useRef(handle);
    handleRef.current = handle;
    const [signal, setSignal] = useState(0);
    useTauriListener('open-request', () => setSignal(n => n + 1));
    useEffect(() => {
        if (!ready) return;
        void invoke<OpenRequest[]>('take_open_requests')
            .then(async (requests) => {
                // Taken entries have left the queue, so each one is handled
                // even if the app locks meanwhile: what it opens sits under
                // the lock screen.
                for (const request of requests) await handleRef.current(request);
            })
            .catch(err => logger.error('[OpenRequest] could not take the queued files', err));
    }, [ready, signal]);
}
