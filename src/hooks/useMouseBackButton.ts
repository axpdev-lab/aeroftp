// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect } from 'react';

/**
 * Translate the mouse Back button (button code 3, common on gaming /
 * productivity mice) into a synthetic Escape keydown event so any
 * already-Esc-aware modal, dialog, dropdown or popover closes on the
 * Back click without each component having to opt in individually.
 *
 * Why a synthetic Escape: every modal in this codebase already wires Esc
 * (TwoFactorPromptDialog, HostKeyDialog, OverwriteDialog, SettingsPanel,
 * VaultPanel, AISettingsPanel, ConnectionScreen, etc.). Re-routing Back
 * through the same channel means we get correct stacked-modal behavior
 * (topmost closes first, since each handler typically self-removes) for
 * free, with zero per-component churn.
 *
 * Reference: HTML mouse buttons spec
 *   - event.button === 3 → 4th physical button = "Back" / "X1"
 *   - event.button === 4 → 5th physical button = "Forward" / "X2"
 * Most browsers fire `mouseup`/`mousedown` on these (no `auxclick` on
 * non-link targets in WebKit), so we listen to `mouseup` to match the
 * gesture timing the user expects when releasing the side button.
 *
 * preventDefault on `mousedown` AND `mouseup` is required to suppress
 * the native browser back-history navigation that otherwise fires.
 * Tauri webviews historically ignored history navigation, but recent
 * WebKitGTK builds began honoring it, so we silence both events.
 *
 * Reported by @EhudKirsh.
 */
export function useMouseBackButton(): void {
    useEffect(() => {
        const swallow = (e: MouseEvent) => {
            if (e.button === 3 || e.button === 4) {
                e.preventDefault();
                e.stopPropagation();
            }
        };
        const onUp = (e: MouseEvent) => {
            if (e.button !== 3) return;
            e.preventDefault();
            e.stopPropagation();
            // Synthesize an Escape keydown so any open modal / dropdown
            // closes via its existing handler. We dispatch on
            // document.activeElement when present so contenteditable /
            // input handlers also see it; otherwise on window.
            const target: EventTarget = document.activeElement ?? window;
            target.dispatchEvent(
                new KeyboardEvent('keydown', {
                    key: 'Escape',
                    code: 'Escape',
                    keyCode: 27,
                    which: 27,
                    bubbles: true,
                    cancelable: true,
                }),
            );
        };
        // Capture phase so we win against any inner element that swallows
        // mouseup before bubbling.
        document.addEventListener('mousedown', swallow, true);
        document.addEventListener('mouseup', onUp, true);
        // Some browsers also fire `auxclick` for the side buttons -
        // suppress it too to avoid double-firing on link targets.
        const onAux = (e: MouseEvent) => {
            if (e.button === 3 || e.button === 4) {
                e.preventDefault();
                e.stopPropagation();
            }
        };
        document.addEventListener('auxclick', onAux, true);
        return () => {
            document.removeEventListener('mousedown', swallow, true);
            document.removeEventListener('mouseup', onUp, true);
            document.removeEventListener('auxclick', onAux, true);
        };
    }, []);
}
