/**
 * useIconTheme — Context + hook for icon theme selection
 *
 * Provides global state for the selected icon theme (outline/filled/minimal).
 * Persisted in localStorage. Used by App.tsx, SettingsPanel, and any
 * component that renders file/folder icons.
 *
 * Default per app theme (when user has never chosen):
 *   light/dark (institutional) → filled, tokyo/cyber (special) → minimal
 */

import React, { useState, useCallback, createContext, useContext } from 'react';
import type { IconTheme } from '../utils/iconThemes';
import type { EffectiveTheme } from './useTheme';

const ICON_THEME_KEY = 'aeroftp-icon-theme';
const VALID_ICON_THEMES: IconTheme[] = ['outline', 'filled', 'minimal'];

/** Map app theme to default icon theme */
export const getDefaultIconTheme = (effectiveTheme: EffectiveTheme): IconTheme => {
    switch (effectiveTheme) {
        case 'tokyo':
        case 'cyber': return 'minimal';  // special themes — neon accent effect
        default: return 'filled';        // light, dark — institutional themes
    }
};

interface IconThemeContextValue {
    iconTheme: IconTheme;
    setIconTheme: (theme: IconTheme) => void;
}

const IconThemeContext = createContext<IconThemeContextValue>({
    iconTheme: 'filled',
    setIconTheme: () => {},
});

export const IconThemeProvider: React.FC<{ children: React.ReactNode }> = ({ children }) => {
    const [iconTheme, setIconThemeState] = useState<IconTheme>(() => {
        const saved = localStorage.getItem(ICON_THEME_KEY) as IconTheme | null;
        if (saved && VALID_ICON_THEMES.includes(saved)) return saved;
        // First load: derive from saved app theme
        const appTheme = localStorage.getItem('aeroftp-theme') || 'auto';
        const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
        const effective = appTheme === 'auto' ? (prefersDark ? 'dark' : 'light') : appTheme;
        return getDefaultIconTheme(effective as EffectiveTheme);
    });

    const setIconTheme = useCallback((theme: IconTheme) => {
        setIconThemeState(theme);
        localStorage.setItem(ICON_THEME_KEY, theme);
    }, []);

    return (
        <IconThemeContext.Provider value={{ iconTheme, setIconTheme }}>
            {children}
        </IconThemeContext.Provider>
    );
};

export const useIconTheme = () => useContext(IconThemeContext);
