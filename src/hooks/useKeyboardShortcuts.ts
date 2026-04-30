// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import React, { useEffect } from 'react';

type KeyHandler = (e: KeyboardEvent) => void;

interface ShortcutConfig {
    [key: string]: KeyHandler;
}

export const useKeyboardShortcuts = (config: ShortcutConfig, deps: React.DependencyList = []) => {
    useEffect(() => {
        const handleKeyDown = (event: KeyboardEvent) => {
            // Ignore if input/textarea is active (unless it's a global shortcut like F-keys)
            // Actually, for F-keys we might want to allow it.
            // For now, let's just let the specific handlers decide, or block specific inputs.
            const target = event.target as HTMLElement;
            if (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable) {
                // Allow F-keys and Escape even in inputs
                if (!event.key.startsWith('F') && event.key !== 'Escape') {
                    return;
                }
            }

            // A11y: when an interactive element (button/link/role=button/contenteditable false but focusable)
            // has focus, Enter and Space must trigger its native click instead of being captured by a global
            // shortcut. Without this, focus-then-Enter does not activate the button (a wishlist regression
            // reported by users keyboard-navigating My Servers / Discover / Settings).
            if (event.key === 'Enter' || event.key === ' ') {
                const ae = document.activeElement as HTMLElement | null;
                if (ae) {
                    const tag = ae.tagName;
                    const role = ae.getAttribute('role');
                    const isInteractive =
                        tag === 'BUTTON' ||
                        tag === 'A' ||
                        tag === 'SELECT' ||
                        role === 'button' ||
                        role === 'link' ||
                        role === 'menuitem' ||
                        role === 'option' ||
                        role === 'tab' ||
                        role === 'checkbox' ||
                        role === 'radio' ||
                        role === 'switch';
                    if (isInteractive && !ae.hasAttribute('aria-disabled') && !(ae as HTMLButtonElement).disabled) {
                        return;
                    }
                }
            }

            const keys: string[] = [];
            // Normalize Meta (Cmd on macOS) to Ctrl for cross-platform shortcut matching
            if (event.ctrlKey || event.metaKey) keys.push('Ctrl');
            if (event.altKey) keys.push('Alt');
            if (event.shiftKey) keys.push('Shift');

            let key = event.key;

            // Ignore modifier key presses themselves
            if (['Control', 'Shift', 'Alt', 'Meta'].includes(key)) return;

            // Normalize common keys
            if (key === 'Escape') key = 'Escape'; // Keep consistent
            if (key === ' ') key = 'Space';
            if (key.length === 1) key = key.toUpperCase();

            keys.push(key);
            const combo = keys.join('+');

            // Debug
            // console.log('Key pressed:', combo);

            if (config[combo]) {
                event.preventDefault();
                config[combo](event);
            }
        };

        window.addEventListener('keydown', handleKeyDown);
        return () => window.removeEventListener('keydown', handleKeyDown);
    }, deps);
};
