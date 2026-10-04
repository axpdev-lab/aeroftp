// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';

/**
 * Stop the recursive compare the AeroSync dialog is waiting for as soon as it
 * stops waiting for it, whatever the reason: Stop, closing the dialog, a new
 * open (F4, the palette, the toolbar), a session teardown, an unmount. Tying
 * the cancel to two buttons left a scan running behind every other way out;
 * keying it on the running compare's progress id covers them all. A cancel
 * for a compare that already finished answers false and changes nothing.
 */
export function useCancelStaleCompare(runningProgressId: string | undefined): void {
    useEffect(() => {
        if (!runningProgressId) return undefined;
        return () => {
            invoke<boolean>('cancel_compare', { progressId: runningProgressId }).catch(() => { /* already finished */ });
        };
    }, [runningProgressId]);
}
