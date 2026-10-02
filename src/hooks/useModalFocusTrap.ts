// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect, type RefObject } from 'react';

const FOCUSABLE_SELECTOR = [
    'a[href]',
    'button:not([disabled])',
    'input:not([disabled])',
    'select:not([disabled])',
    'textarea:not([disabled])',
    '[tabindex]:not([tabindex="-1"])',
].join(',');

/**
 * Holds keyboard focus inside a modal panel for as long as it is mounted.
 *
 * `aria-modal` tells assistive technology that the page behind is inert; it
 * does not make it so. Without this, Tab walks out of the dialog into the file
 * panel underneath. On mount focus moves to `initialFocusRef` (or the first
 * control of the panel), Tab and Shift+Tab wrap at its ends, and on unmount
 * focus goes back to the element that had it, the same contract as
 * `ConfirmOverlay`.
 *
 * Tab is also kept from the window-level shortcuts: the file manager binds it
 * to switching panels, and that handler would swallow the key and switch the
 * panel behind the dialog instead of moving focus.
 *
 * Mount-only by design: callers pass refs, so an inline `onClose` arrow from
 * the parent never re-runs it and never steals focus back mid-edit.
 */
export function useModalFocusTrap(
    panelRef: RefObject<HTMLElement | null>,
    initialFocusRef?: RefObject<HTMLElement | null>,
): void {
    useEffect(() => {
        const returnTo = document.activeElement as HTMLElement | null;
        const focusable = () => Array.from(panelRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR) ?? []);
        const onKey = (event: KeyboardEvent) => {
            if (event.key !== 'Tab') return;
            event.stopPropagation();
            const items = focusable();
            if (items.length === 0) {
                event.preventDefault();
                return;
            }
            const first = items[0];
            const last = items[items.length - 1];
            const active = document.activeElement;
            const inside = !!panelRef.current?.contains(active);
            if (event.shiftKey && (active === first || !inside)) {
                event.preventDefault();
                last.focus();
            } else if (!event.shiftKey && (active === last || !inside)) {
                event.preventDefault();
                first.focus();
            }
        };
        document.addEventListener('keydown', onKey, true);
        (initialFocusRef?.current ?? focusable()[0])?.focus();
        return () => {
            document.removeEventListener('keydown', onKey, true);
            returnTo?.focus?.();
        };
    }, [panelRef, initialFocusRef]);
}
