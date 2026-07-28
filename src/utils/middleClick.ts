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

/**
 * Counterpart of {@link middleClickOpen} for a tab strip (Ehud #274): a middle
 * click anywhere on the tab closes it, the way a browser closes a tab. Spread
 * onto the tab container so the whole chip is a target, not only its ✖ — the
 * `auxclick` on the ✖ bubbles up to the container, so both spots work with one
 * handler. `close` is skipped when `enabled` is false (e.g. the last
 * non-closable tab, or a tab locked by a running transfer).
 *
 * The event is stopped as well as default-prevented: tab strips sit inside
 * panels that may carry their own middle-click behaviour, and closing a tab
 * should never double as "open this in the background".
 */
export function middleClickClose(close: () => void, enabled = true) {
    return {
        onAuxClick: (e: MouseEvent) => {
            if (e.button !== 1) return;
            e.preventDefault();
            e.stopPropagation();
            if (enabled) close();
        },
        onMouseDown: (e: MouseEvent) => {
            if (e.button === 1) e.preventDefault();
        },
    };
}
