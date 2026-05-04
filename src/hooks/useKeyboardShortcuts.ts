// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useEffect, useRef } from 'react';

type KeyHandler = (e: KeyboardEvent) => void;

interface ShortcutConfig {
    [key: string]: KeyHandler;
}

const isTextEditingTarget = (element: HTMLElement | null): boolean => {
    if (!element) return false;
    const tag = element.tagName;
    if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || element.isContentEditable) {
        return true;
    }
    return !!element.closest('.monaco-editor, .xterm, [role="textbox"], [contenteditable="true"]');
};

export const useKeyboardShortcuts = (config: ShortcutConfig, deps: React.DependencyList = []) => {
    const configRef = useRef(config);

    useEffect(() => {
        configRef.current = config;
    });

    useEffect(() => {
        const handleKeyDown = (event: KeyboardEvent) => {
            const target = event.target instanceof HTMLElement ? event.target : null;
            const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
            if (isTextEditingTarget(target) || isTextEditingTarget(active)) {
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

            const handler = configRef.current[combo];
            if (handler) {
                event.preventDefault();
                handler(event);
            }
        };

        window.addEventListener('keydown', handleKeyDown);
        return () => window.removeEventListener('keydown', handleKeyDown);
    }, deps);
};
