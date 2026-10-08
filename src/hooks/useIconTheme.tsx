// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * useIconTheme: Context + hook for icon theme selection
 *
 * Provides global state for the selected icon theme (outline/filled/minimal).
 * Persisted in localStorage. Used by App.tsx, SettingsPanel, and any
 * component that renders file/folder icons.
 *
 * Default per app theme (when user has never chosen):
 *   light/dark (institutional) → filled, tokyo/cyber (special) → minimal
 */

import React, { useState, useCallback, createContext, useContext, useEffect } from 'react';
import type { IconTheme } from '../utils/iconThemes';
import type { EffectiveTheme } from './useTheme';
import { useAppearance } from './useAppearance';

const ICON_THEME_KEY = 'aeroftp-icon-theme';
const VALID_ICON_THEMES: IconTheme[] = ['outline', 'filled', 'minimal'];

/** Map app theme to default icon theme */
export const getDefaultIconTheme = (effectiveTheme: EffectiveTheme): IconTheme => {
    switch (effectiveTheme) {
        case 'tokyo':
        case 'cyber': return 'minimal';  // special themes: neon accent effect
        default: return 'filled';        // light, dark: institutional themes
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
    const { effectiveTheme } = useAppearance();
    const [chosenTheme, setIconThemeState] = useState<IconTheme | null>(() => {
        const saved = localStorage.getItem(ICON_THEME_KEY) as IconTheme | null;
        if (saved && VALID_ICON_THEMES.includes(saved)) return saved;
        return null;
    });
    const iconTheme = chosenTheme ?? getDefaultIconTheme(effectiveTheme);

    const setIconTheme = useCallback((theme: IconTheme) => {
        setIconThemeState(theme);
        localStorage.setItem(ICON_THEME_KEY, theme);
    }, []);

    // Re-read the icon theme when a keystore import restores it.
    // Same rationale as `useTheme`: initial value is captured once on
    // mount, so an imported value would not surface until refresh
    // (issue #214 C3 pt.E2).
    useEffect(() => {
        const reload = () => {
            const saved = localStorage.getItem(ICON_THEME_KEY) as IconTheme | null;
            setIconThemeState(saved && VALID_ICON_THEMES.includes(saved) ? saved : null);
        };
        window.addEventListener('aeroftp-localstorage-restored', reload);
        return () => window.removeEventListener('aeroftp-localstorage-restored', reload);
    }, []);

    return (
        <IconThemeContext.Provider value={{ iconTheme, setIconTheme }}>
            {children}
        </IconThemeContext.Provider>
    );
};

export const useIconTheme = () => useContext(IconThemeContext);
