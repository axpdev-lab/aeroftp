/**
 * Icon Theme Registry
 *
 * Central entry point for the icon theme system.
 * getIconThemeProvider() returns the appropriate theme implementation
 * based on the user's icon theme preference and current app theme.
 */

import type { IconThemeProvider } from './types';
import type { EffectiveTheme } from './minimalTheme';
import { outlineTheme } from './outlineTheme';
import { filledTheme } from './filledTheme';
import { createMinimalTheme } from './minimalTheme';

export type IconTheme = 'outline' | 'filled' | 'minimal';

export type { FileIconResult, IconThemeProvider } from './types';
export type { EffectiveTheme } from './minimalTheme';

export const getIconThemeProvider = (
    iconTheme: IconTheme,
    effectiveTheme: EffectiveTheme
): IconThemeProvider => {
    switch (iconTheme) {
        case 'filled': return filledTheme;
        case 'minimal': return createMinimalTheme(effectiveTheme);
        case 'outline':
        default: return outlineTheme;
    }
};
