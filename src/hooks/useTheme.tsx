/**
 * Theme Hook - Manages light/dark/tokyo/cyber/auto theme switching
 *
 * Unified theme system for the entire app including:
 * - Main app UI
 * - Activity Log Panel
 * - DevTools / Monaco Editor / AIChat
 */

import { useState, useEffect } from 'react';
import { Sun, Moon, Monitor } from 'lucide-react';

/** Tokyo Cherry Blossom icon (neon purple) */
const CherryBlossomIcon: React.FC<{ size?: number; className?: string }> = ({ size = 18, className }) => (
    <svg viewBox="0 0 32 32" width={size} height={size} fill="currentColor" className={className}>
        <path d="M30.43,12.124c-0.441-1.356-1.289-2.518-2.454-3.358c-1.157-0.835-2.514-1.276-3.924-1.276c-0.009,0-0.018,0-0.026,0c-0.44,0.001-0.898,0.055-1.368,0.158c-0.048-0.492-0.145-0.96-0.288-1.395c-0.442-1.345-1.287-2.498-2.442-3.335c-1.111-0.805-2.44-1.212-3.776-1.242C16.104,1.654,16.054,1.64,16,1.64c-0.054,0-0.105,0.014-0.151,0.036c-1.336,0.029-2.664,0.437-3.776,1.241c-1.155,0.837-2,1.991-2.442,3.335C9.488,6.686,9.392,7.154,9.343,7.648C8.859,7.542,8.415,7.462,7.926,7.491C6.511,7.496,5.153,7.942,4,8.783c-1.151,0.839-1.99,1.994-2.428,3.34s-0.437,2.774,0.001,4.129c0.439,1.358,1.275,2.518,2.417,3.353c0.369,0.271,0.785,0.507,1.239,0.706c-0.251,0.428-0.448,0.863-0.588,1.298c-0.432,1.349-0.427,2.778,0.016,4.135c0.443,1.354,1.282,2.51,2.427,3.341c1.145,0.832,2.503,1.272,3.927,1.275c0.003,0,0.007,0,0.01,0c1.422,0,2.78-0.437,3.926-1.263c0.371-0.268,0.724-0.589,1.053-0.96c0.319,0.36,0.659,0.673,1.013,0.932c1.145,0.839,2.509,1.285,3.946,1.291c0.007,0,0.014,0,0.021,0c1.428,0,2.789-0.441,3.938-1.275c1.153-0.838,1.995-2.004,2.435-3.37c0.439-1.368,0.438-2.804-0.007-4.152c-0.137-0.417-0.329-0.837-0.573-1.251c0.44-0.192,0.842-0.418,1.199-0.675c1.151-0.831,1.998-1.991,2.446-3.355C30.865,14.918,30.869,13.48,30.43,12.124z M16.698,22.101c0,0.385-0.313,0.698-0.698,0.698s-0.698-0.313-0.698-0.698s0.313-0.697,0.698-0.697S16.698,21.716,16.698,22.101z M15.302,8.348c0-0.385,0.313-0.698,0.698-0.698s0.698,0.313,0.698,0.698c0,0.385-0.313,0.698-0.698,0.698S15.302,8.732,15.302,8.348z"/>
    </svg>
);

/** Mask Theater icon (neon green) */
const MaskTheaterIcon: React.FC<{ size?: number; className?: string }> = ({ size = 18, className }) => (
    <svg viewBox="0 0 512 512" width={size} height={size} fill="currentColor" className={className}>
        <path fillRule="evenodd" d="M37.728,49.312v186.48C37.728,413.104,256,512,256,512s218.272-98.896,218.272-276.208V49.312C474.272,49.312,396.128,0,256,0S37.728,49.312,37.728,49.312z M364.6,286.992c0,0-29.104,74.104-108.6,74.104s-108.6-74.104-108.6-74.104S256,361.096,364.6,286.992z M224.8,153.424c-16.072-37.664-59.632-55.168-97.296-39.096c-17.584,7.504-31.592,21.512-39.096,39.096H224.8z M423.592,153.424c-16.072-37.664-59.632-55.168-97.296-39.096c-17.584,7.504-31.592,21.512-39.096,39.096H423.592z"/>
    </svg>
);

export type Theme = 'light' | 'dark' | 'tokyo' | 'cyber' | 'auto';

/** Resolved theme (no 'auto') */
export type EffectiveTheme = 'light' | 'dark' | 'tokyo' | 'cyber';

/**
 * Get the effective theme (resolving 'auto' to actual theme)
 */
export const getEffectiveTheme = (theme: Theme, prefersDark: boolean): EffectiveTheme => {
    if (theme === 'auto') {
        return prefersDark ? 'dark' : 'light';
    }
    return theme;
};

/**
 * Map app theme to Monaco editor theme
 */
export const getMonacoTheme = (theme: Theme, prefersDark: boolean): 'vs' | 'vs-dark' | 'tokyo-night' | 'cyber' => {
    const effective = getEffectiveTheme(theme, prefersDark);
    switch (effective) {
        case 'light': return 'vs';
        case 'dark': return 'vs-dark';
        case 'tokyo': return 'tokyo-night';
        case 'cyber': return 'cyber';
        default: return 'vs-dark';
    }
};

/**
 * Map app theme to Activity Log theme
 */
export const getLogTheme = (theme: Theme, prefersDark: boolean): 'light' | 'dark' | 'tokyo' | 'cyber' => {
    const effective = getEffectiveTheme(theme, prefersDark);
    return effective;
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
                // dark, tokyo, and cyber all use dark mode styling
                setIsDark(theme === 'dark' || theme === 'tokyo' || theme === 'cyber');
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
        // Add theme-specific classes for CSS overrides
        document.documentElement.classList.toggle('tokyo', theme === 'tokyo');
        document.documentElement.classList.toggle('cyber', theme === 'cyber');
    }, [isDark, theme]);

    return { theme, setTheme, isDark };
};

/**
 * Theme Toggle Button Component
 * Cycles through: light -> dark -> tokyo -> cyber -> auto
 */
interface ThemeToggleProps {
    theme: Theme;
    setTheme: (t: Theme) => void;
}

export const ThemeToggle: React.FC<ThemeToggleProps> = ({ theme, setTheme }) => {
    const nextTheme = (): Theme => {
        const order: Theme[] = ['light', 'dark', 'tokyo', 'cyber', 'auto'];
        return order[(order.indexOf(theme) + 1) % 5];
    };

    const getIcon = () => {
        switch (theme) {
            case 'light': return <Sun size={18} />;
            case 'dark': return <Moon size={18} />;
            case 'tokyo': return <CherryBlossomIcon size={18} className="text-purple-400" />;
            case 'cyber': return <MaskTheaterIcon size={18} className="text-emerald-400" />;
            case 'auto': return <Monitor size={18} />;
        }
    };

    const getLabel = () => {
        switch (theme) {
            case 'light': return 'Light';
            case 'dark': return 'Dark';
            case 'tokyo': return 'Tokyo Night';
            case 'cyber': return 'Cyberpunk';
            case 'auto': return 'Auto';
        }
    };

    const getButtonStyle = () => {
        switch (theme) {
            case 'tokyo': return 'bg-purple-900/50 hover:bg-purple-800/50';
            case 'cyber': return 'bg-emerald-900/50 hover:bg-emerald-800/50';
            default: return 'bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600';
        }
    };

    return (
        <button
            onClick={() => setTheme(nextTheme())}
            className={`p-2 rounded-lg transition-colors ${getButtonStyle()}`}
            title={`Theme: ${getLabel()}`}
        >
            {getIcon()}
        </button>
    );
};
