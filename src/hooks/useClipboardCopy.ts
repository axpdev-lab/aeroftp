// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

/**
 * Copy-to-clipboard with the "green tick for a moment" acknowledgement the app
 * uses everywhere (Ehud #369, #274).
 *
 * Copy goes through the Rust `copy_to_clipboard` command rather than
 * `navigator.clipboard`, which is what every other copy affordance in the app
 * does: under WebKitGTK the web clipboard API is only available in a secure
 * context and silently rejects often enough that the button would look broken.
 *
 * `copied` flips back to false after `resetMs`, and the pending timer is
 * dropped on unmount, since a dialog closed right after a copy would otherwise
 * set state on a gone component.
 */
export function useClipboardCopy(resetMs = 2000) {
    const [copied, setCopied] = useState(false);
    const resetRef = useRef<ReturnType<typeof setTimeout> | null>(null);

    const copy = useCallback(async (text: string) => {
        try {
            await invoke('copy_to_clipboard', { text });
            setCopied(true);
            if (resetRef.current) clearTimeout(resetRef.current);
            resetRef.current = setTimeout(() => setCopied(false), resetMs);
            return true;
        } catch {
            // Clipboard access is best-effort: never turn a failed copy into an
            // error dialog, the user can still select the text by hand.
            return false;
        }
    }, [resetMs]);

    useEffect(() => () => {
        if (resetRef.current) clearTimeout(resetRef.current);
    }, []);

    return { copied, copy };
}
