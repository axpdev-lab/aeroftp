import { useEffect, useCallback } from 'react';

export interface ShortcutAction {
    key: string;
    ctrl: boolean;
    shift?: boolean;
    description: string;
    action: () => void;
}

/**
 * Hook to register keyboard shortcuts for the AI chat.
 * Only active when the chat panel is visible/focused.
 *
 * Shortcuts are ignored when the user is typing in an INPUT element
 * or a TEXTAREA that is not the AI chat input (identified by
 * the `data-aichat-input` attribute on a parent element).
 */
export function useKeyboardShortcuts(shortcuts: ShortcutAction[], enabled: boolean = true): void {
    useEffect(() => {
        if (!enabled) return;

        const handler = (e: KeyboardEvent) => {
            const target = e.target as HTMLElement;
            const isInput =
                target.tagName === 'INPUT' ||
                (target.tagName === 'TEXTAREA' && !target.closest('[data-aichat-input]')) ||
                target.isContentEditable;
            if (isInput) return;

            for (const shortcut of shortcuts) {
                const ctrlKey = e.ctrlKey || e.metaKey;
                if (
                    e.key.toLowerCase() === shortcut.key.toLowerCase() &&
                    ctrlKey === shortcut.ctrl &&
                    (!shortcut.shift || e.shiftKey)
                ) {
                    e.preventDefault();
                    shortcut.action();
                    return;
                }
            }
        };

        document.addEventListener('keydown', handler);
        return () => document.removeEventListener('keydown', handler);
    }, [shortcuts, enabled]);
}

/**
 * Get default AI chat shortcuts.
 * Actions are provided by the caller (AIChat component).
 */
export function getDefaultShortcuts(actions: {
    clearChat: () => void;
    newChat: () => void;
    exportChat: () => void;
    toggleSearch: () => void;
    focusInput: () => void;
}): ShortcutAction[] {
    return [
        { key: 'l', ctrl: true, description: 'Clear chat', action: actions.clearChat },
        { key: 'n', ctrl: true, shift: true, description: 'New chat', action: actions.newChat },
        { key: 'e', ctrl: true, shift: true, description: 'Export chat', action: actions.exportChat },
        { key: 'f', ctrl: true, description: 'Search in chat', action: actions.toggleSearch },
        { key: '/', ctrl: true, description: 'Focus input', action: actions.focusInput },
    ];
}
