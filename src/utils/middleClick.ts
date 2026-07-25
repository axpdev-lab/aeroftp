// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { MouseEvent } from 'react';

/**
 * Event props that make a clickable element open "in the background" on a
 * middle-mouse click (Ehud #274), mirroring how a browser opens a link in a
 * background tab. `open` runs on the middle button only; the primary (left) and
 * secondary (right) buttons are left untouched, so normal clicks and context
 * menus still work. The middle-button autoscroll — which the browser arms on
 * mousedown — is suppressed so it never fires.
 *
 * Spread onto any element: `<button {...middleClickOpen(() => openInBg())} />`.
 */
export function middleClickOpen(open: () => void) {
    return {
        onAuxClick: (e: MouseEvent) => {
            if (e.button === 1) {
                e.preventDefault();
                open();
            }
        },
        onMouseDown: (e: MouseEvent) => {
            if (e.button === 1) e.preventDefault();
        },
    };
}
