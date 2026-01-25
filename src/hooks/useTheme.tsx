/**
 * Theme Hook - Manages light/dark/tokyo/auto theme switching
 *
 * Unified theme system for the entire app including:
 * - Main app UI
 * - Activity Log Panel
 * - DevTools / Monaco Editor
 */

import { useState, useEffect } from 'react';
import { Sun, Moon, Monitor, Sparkles } from 'lucide-react';

export type Theme = 'light' | 'dark' | 'tokyo' | 'auto';

/**
 * Get the effective theme (resolving 'auto' to actual theme)
 */
export const getEffectiveTheme = (theme: Theme, prefersDark: boolean): 'light' | 'dark' | 'tokyo' => {
    if (theme === 'auto') {
        return prefersDark ? 'dark' : 'light';
    }
    return theme;
};

/**
 * Map app theme to Monaco editor theme
 */
export const getMonacoTheme = (theme: Theme, prefersDark: boolean): 'vs' | 'vs-dark' | 'tokyo-night' => {
    const effective = getEffectiveTheme(theme, prefersDark);
    switch (effective) {
        case 'light': return 'vs';
        case 'dark': return 'vs-dark';
        case 'tokyo': return 'tokyo-night';
        default: return 'vs-dark';
    }
};

/**
 * Map app theme to Activity Log theme
 */
export const getLogTheme = (theme: Theme, prefersDark: boolean): 'light' | 'dark' | 'cyber' => {
    const effective = getEffectiveTheme(theme, prefersDark);
    switch (effective) {
        case 'light': return 'light';
        case 'dark': return 'dark';
        case 'tokyo': return 'cyber';
        default: return 'dark';
    }
};

/**
 * Custom hook for theme management
 * Persists theme preference to localStorage
 * Supports auto mode that follows system preference
 */
export const useTheme = () => {
    const [theme, setTheme] = useState<Theme>(() => {
        const saved = localStorage.getItem('aeroftp-theme') as Theme;
        return saved || 'auto';
    });
    const [isDark, setIsDark] = useState(false);

    useEffect(() => {
        const updateDarkMode = () => {
            if (theme === 'auto') {
                setIsDark(window.matchMedia('(prefers-color-scheme: dark)').matches);
            } else {
                // Both 'dark' and 'tokyo' use dark mode styling
                setIsDark(theme === 'dark' || theme === 'tokyo');
            }
        };
        updateDarkMode();
        localStorage.setItem('aeroftp-theme', theme);
        const mediaQuery = window.matchMedia('(prefers-color-scheme: dark)');
        mediaQuery.addEventListener('change', updateDarkMode);
        return () => mediaQuery.removeEventListener('change', updateDarkMode);
    }, [theme]);

    useEffect(() => {
        document.documentElement.classList.toggle('dark', isDark);
        // Add tokyo class for special tokyo night styling
        document.documentElement.classList.toggle('tokyo', theme === 'tokyo');
    }, [isDark, theme]);

    return { theme, setTheme, isDark };
};

/**
 * Theme Toggle Button Component
 * Cycles through: light -> dark -> tokyo -> auto
 */
interface ThemeToggleProps {
    theme: Theme;
    setTheme: (t: Theme) => void;
}

export const ThemeToggle: React.FC<ThemeToggleProps> = ({ theme, setTheme }) => {
    const nextTheme = (): Theme => {
        const order: Theme[] = ['light', 'dark', 'tokyo', 'auto'];
        return order[(order.indexOf(theme) + 1) % 4];
    };

    const getIcon = () => {
        switch (theme) {
            case 'light': return <Sun size={18} />;
            case 'dark': return <Moon size={18} />;
            case 'tokyo': return <Sparkles size={18} className="text-purple-400" />;
            case 'auto': return <Monitor size={18} />;
        }
    };

    const getLabel = () => {
        switch (theme) {
            case 'light': return 'Light';
            case 'dark': return 'Dark';
            case 'tokyo': return 'Tokyo Night';
            case 'auto': return 'Auto';
        }
    };

    return (
        <button
            onClick={() => setTheme(nextTheme())}
            className={`p-2 rounded-lg transition-colors ${
                theme === 'tokyo'
                    ? 'bg-purple-900/50 hover:bg-purple-800/50'
                    : 'bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600'
            }`}
            title={`Theme: ${getLabel()}`}
        >
            {getIcon()}
        </button>
    );
};
