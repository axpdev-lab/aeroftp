import { useEffect } from 'react';

type KeyHandler = (e: KeyboardEvent) => void;

interface ShortcutConfig {
    [key: string]: KeyHandler;
}

export const useKeyboardShortcuts = (config: ShortcutConfig, deps: any[] = []) => {
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
