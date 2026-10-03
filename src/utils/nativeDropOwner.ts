// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Marks the root of a surface that owns the OS file drops landing on it.
 *
 * On Linux and macOS a drop from the file manager reaches the frontend only
 * through Tauri's webview-wide `onDragDropEvent`, which carries a position
 * and no target element, so every listener hears every drop. A modal can take
 * them all because it covers the app; a surface that sits next to others (the
 * Security Tools panel in AeroTools) marks its root instead, and listeners ask
 * what lies under the drop point before acting on it.
 */
export const NATIVE_DROP_OWNER_ATTR = 'data-native-drop-owner';

/** The marked surface under a native drag/drop position, if there is one. */
export function nativeDropOwnerAt(position: { x: number; y: number }): Element | null {
    // Used as CSS pixels on purpose, although the payload is typed
    // PhysicalPosition. Where this app keeps the native drop handler, wry
    // reports logical coordinates (GTK widget coordinates on Linux, AppKit
    // points on macOS) and tauri-runtime-wry wraps them unscaled (read in
    // wry 0.55.1 and tauri-runtime-wry 2.11.4). Windows, the backend that
    // reports device pixels, runs with the native handler disabled (lib.rs).
    // Dividing by devicePixelRatio would miss the target on every HiDPI screen.
    const hit = document.elementFromPoint(position.x, position.y);
    return hit?.closest(`[${NATIVE_DROP_OWNER_ATTR}]`) ?? null;
}
