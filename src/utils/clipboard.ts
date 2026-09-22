// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { invoke } from '@tauri-apps/api/core';

/**
 * Put text on the system clipboard. The one way the app copies.
 *
 * It goes through the Rust `copy_to_clipboard` command first: under WebKitGTK
 * the web clipboard API needs a secure context and rejects often enough, with
 * no error the user can see, that a copy button using it looked like it worked
 * and copied nothing. `navigator.clipboard` is only the fallback when the
 * native command itself fails.
 *
 * Rejects when neither worked, so a caller that shows "copied" can show it
 * only after this resolves.
 */
export async function copyText(text: string): Promise<void> {
    try {
        await invoke('copy_to_clipboard', { text });
    } catch (nativeError) {
        if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
            await navigator.clipboard.writeText(text);
            return;
        }
        throw nativeError;
    }
}
