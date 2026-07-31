// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The one stacking order for the app's full-screen overlays.
 *
 * Every one of them is `position: fixed; inset: 0`, so what the user can see and
 * click is decided entirely by z-index and, at equal z-index, by the order the
 * elements appear in the DOM. Issue #537 is what that costs when the numbers are
 * scattered as literals: the app-wide `ConfirmDialog` and the Find Duplicates
 * modal were both `z-50`, and the confirm is mounted first in `App.tsx`, so the
 * modal painted over the very question it was waiting on and its own backdrop
 * swallowed every click aimed at the buttons underneath. The only way to answer
 * the confirm was to close the window that had raised it.
 *
 * Three rules follow from that, and all three are pinned in `modalLayers.test.ts`
 * against every `fixed inset-0` overlay in the source:
 *
 * 1. A module's modal sits at `modal`; a confirmation it raises for itself, from
 *    inside its own overlay, sits at `modalConfirm`.
 * 2. The app-wide confirm sits above *every* dialog. Not just above `modal`: the
 *    twelve `setConfirmDialog` call sites include the overwrite and delete
 *    prompts, and those fire from flows started inside the `elevatedModal` tier
 *    (AeroSync, the transfer plan, the trash managers). A confirm below them
 *    would repeat #537 there.
 * 3. The lock screens sit above everything. `isAppLocked` gates no rendering in
 *    `App.tsx` — the whole tree stays mounted behind the lock overlay — so any
 *    dialog left open when the idle probe fires would otherwise stay legible
 *    over the lock screen, which was true of the entire `elevatedModal` tier.
 */
export const MODAL_LAYER = {
    /** A module's own full-screen modal: Find Duplicates, Disk Usage, Properties. */
    modal: 50,
    /** A confirmation a modal raises inside its own overlay. */
    modalConfirm: 60,
    /** Dialogs that already opted above the pack: trash managers, AeroSync, hub. */
    elevatedModal: 9999,
    /** Confirmations those raise inside their own overlay. */
    elevatedConfirm: 10000,
    /** The app-wide `ConfirmDialog`. Above every dialog that can be waiting on it. */
    globalConfirm: 10050,
    /** The quit guard: it arbitrates leaving the app, so it outranks the confirm. */
    guardedClose: 10060,
    /** Lock screens. Nothing may cover them. */
    lock: 10100,
} as const;

/**
 * The same scale as Tailwind classes.
 *
 * These have to be literal strings. Tailwind generates utilities by scanning the
 * source for class names, so a computed `` `z-[${n}]` `` would produce a class
 * that never makes it into the stylesheet — the element would fall back to
 * `z-index: auto` and land right back in the DOM-order trap. The test checks
 * each string against its number so the two cannot drift apart.
 */
export const MODAL_Z = {
    modal: 'z-50',
    modalConfirm: 'z-[60]',
    elevatedModal: 'z-[9999]',
    elevatedConfirm: 'z-[10000]',
    globalConfirm: 'z-[10050]',
    guardedClose: 'z-[10060]',
    lock: 'z-[10100]',
} as const;

/** The z-index a `MODAL_Z` class resolves to, or null if it is not one of ours. */
export function modalZIndexOf(className: string): number | null {
    const arbitrary = className.match(/^z-\[(\d+)\]$/);
    if (arbitrary) return Number(arbitrary[1]);
    const scale = className.match(/^z-(\d+)$/);
    if (scale) return Number(scale[1]);
    return null;
}
