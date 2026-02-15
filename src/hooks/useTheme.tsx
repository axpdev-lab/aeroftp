/**
 * Theme Hook - Manages light/dark/tokyo/cyber/auto theme switching
 *
 * Unified theme system for the entire app including:
 * - Main app UI
 * - Activity Log Panel
 * - DevTools / Monaco Editor / AIChat
 */

import { useState, useEffect } from 'react';
import { useTranslation } from '../i18n';
import { Sun, Moon, Monitor } from 'lucide-react';

/** Tokyo Cherry Blossom icon (neon purple) */
const CherryBlossomIcon: React.FC<{ size?: number; className?: string }> = ({ size = 18, className }) => (
    <svg viewBox="0 0 32 32" width={size} height={size} fill="currentColor" className={className}>
        <path d="M30.43,12.124c-0.441-1.356-1.289-2.518-2.454-3.358c-1.157-0.835-2.514-1.276-3.924-1.276c-0.009,0-0.018,0-0.026,0c-0.44,0.001-0.898,0.055-1.368,0.158c-0.048-0.492-0.145-0.96-0.288-1.395c-0.442-1.345-1.287-2.498-2.442-3.335c-1.111-0.805-2.44-1.212-3.776-1.242C16.104,1.654,16.054,1.64,16,1.64c-0.054,0-0.105,0.014-0.151,0.036c-1.336,0.029-2.664,0.437-3.776,1.241c-1.155,0.837-2,1.991-2.442,3.335C9.488,6.686,9.392,7.154,9.343,7.648C8.859,7.542,8.415,7.462,7.926,7.491C6.511,7.496,5.153,7.942,4,8.783c-1.151,0.839-1.99,1.994-2.428,3.34s-0.437,2.774,0.001,4.129c0.439,1.358,1.275,2.518,2.417,3.353c0.369,0.271,0.785,0.507,1.239,0.706c-0.251,0.428-0.448,0.863-0.588,1.298c-0.432,1.349-0.427,2.778,0.016,4.135c0.443,1.354,1.282,2.51,2.427,3.341c1.145,0.832,2.503,1.272,3.927,1.275c0.003,0,0.007,0,0.01,0c1.422,0,2.78-0.437,3.926-1.263c0.371-0.268,0.724-0.589,1.053-0.96c0.319,0.36,0.659,0.673,1.013,0.932c1.145,0.839,2.509,1.285,3.946,1.291c0.007,0,0.014,0,0.021,0c1.428,0,2.789-0.441,3.938-1.275c1.153-0.838,1.995-2.004,2.435-3.37c0.439-1.368,0.438-2.804-0.007-4.152c-0.137-0.417-0.329-0.837-0.573-1.251c0.44-0.192,0.842-0.418,1.199-0.675c1.151-0.831,1.998-1.991,2.446-3.355C30.865,14.918,30.869,13.48,30.43,12.124z M16.698,22.101c0,0.385-0.313,0.698-0.698,0.698s-0.698-0.313-0.698-0.698s0.313-0.697,0.698-0.697S16.698,21.716,16.698,22.101z M15.302,8.348c0-0.385,0.313-0.698,0.698-0.698s0.698,0.313,0.698,0.698c0,0.385-0.313,0.698-0.698,0.698S15.302,8.732,15.302,8.348z"/>
    </svg>
);

/** Cybersecurity shield icon (neon green) */
const CyberShieldIcon: React.FC<{ size?: number; className?: string }> = ({ size = 18, className }) => (
    <svg viewBox="0 0 443 511.932" width={size} height={size} fill="currentColor" className={className}>
        <path d="M221.013 103.932c52.548 33.306 100.022 49.068 140.763 45.34 7.113 143.916-46.033 228.911-140.213 264.379-90.955-33.199-144.759-114.529-140.766-266.562 52.729 2.762 98.324-10.611 140.216-43.157zM443 178.202c0 8.926-5.323 16.608-12.967 20.046v30.991c0 2.767-1.13 5.284-2.957 7.108a10.018 10.018 0 01-7.101 2.947h-40.994a395.636 395.636 0 002.835-18.859h30.713v-21.97c-7.911-3.323-13.466-11.144-13.466-20.263 0-12.133 9.835-21.968 21.969-21.968 12.133 0 21.968 9.835 21.968 21.968zM116.38 49.944c8.925 0 16.606 5.323 20.044 12.967h30.993a10.03 10.03 0 017.108 2.957 10.023 10.023 0 012.947 7.101v33.021a225.476 225.476 0 01-18.86 8.201V80.415h-21.97c-3.324 7.911-11.144 13.466-20.262 13.466-12.134 0-21.969-9.835-21.969-21.969 0-12.132 9.835-21.968 21.969-21.968zm96.123 35.005V42.347c-8.062-3.254-13.75-11.151-13.75-20.378C198.753 9.836 208.588 0 220.721 0c12.134 0 21.968 9.836 21.968 21.969 0 8.806-5.181 16.4-12.662 19.903v42.422a540.733 540.733 0 01-9.092-5.638 274.673 274.673 0 01-8.432 6.293zm93.385-4.538l-21.969.004v31.771a363.038 363.038 0 01-18.86-8.621V72.969c0-2.768 1.128-5.281 2.947-7.101a10.03 10.03 0 017.108-2.957h30.992c3.438-7.645 11.119-12.967 20.045-12.967 12.133 0 21.969 9.835 21.969 21.968 0 12.134-9.836 21.969-21.969 21.969-9.121 0-16.942-5.557-20.263-13.47zM116.38 461.989c8.925 0 16.606-5.323 20.044-12.966h30.993c2.766 0 5.283-1.131 7.108-2.958a10.023 10.023 0 002.947-7.101v-20.988a224.772 224.772 0 01-18.86-12.691v26.233l-21.97.003c-3.322-7.913-11.143-13.469-20.262-13.469-12.134 0-21.969 9.835-21.969 21.969 0 12.133 9.835 21.968 21.969 21.968zm96.123 7.596v-34.154a271.6 271.6 0 009.064 3.496 282.389 282.389 0 008.46-3.353v34.486c7.481 3.504 12.662 11.097 12.662 19.904 0 12.134-9.834 21.968-21.968 21.968-12.133 0-21.968-9.834-21.968-21.968 0-9.228 5.689-17.125 13.75-20.379zm71.416-38.067h21.97c3.323-7.911 11.143-13.466 20.262-13.466 12.133 0 21.969 9.835 21.969 21.969 0 12.133-9.836 21.968-21.969 21.968-8.925 0-16.607-5.323-20.045-12.966h-30.992a10.026 10.026 0 01-7.108-2.958 10.023 10.023 0 01-2.947-7.101V417.97a236.442 236.442 0 0018.86-12.667v26.215zM0 338.667c0-8.926 5.323-16.608 12.967-20.045V287.63c0-2.767 1.13-5.283 2.957-7.108a10.023 10.023 0 017.101-2.947h51.186a313.221 313.221 0 006.206 18.859H30.471v21.971c7.911 3.323 13.466 11.143 13.466 20.262 0 12.133-9.835 21.969-21.969 21.969C9.835 360.636 0 350.8 0 338.667zm30.468-140.201l.003 21.969H62.24a429.26 429.26 0 002.978 18.859H23.025a10.018 10.018 0 01-7.101-2.947 10.023 10.023 0 01-2.957-7.108v-30.991C5.323 194.81 0 187.128 0 178.202c0-12.133 9.835-21.968 21.968-21.968 12.134 0 21.969 9.835 21.969 21.968 0 9.12-5.556 16.941-13.469 20.264zM443 338.667c0-8.926-5.323-16.608-12.967-20.045V287.63c0-2.767-1.13-5.283-2.957-7.108a10.023 10.023 0 00-7.101-2.947h-49.94a299.496 299.496 0 01-6.272 18.859h48.766l.003 21.97c-7.913 3.323-13.469 11.142-13.469 20.263 0 12.133 9.835 21.969 21.969 21.969 12.133 0 21.968-9.836 21.968-21.969zM177.326 221.593v-2.761c0-12.522 4.937-23.938 12.89-32.237 8.018-8.357 19.091-13.549 31.285-13.549 12.197 0 23.272 5.189 31.284 13.549 7.956 8.299 12.893 19.711 12.893 32.237v2.761h-13.654v-2.76c0-8.907-3.466-16.974-9.052-22.8-5.525-5.762-13.126-9.337-21.475-9.337-8.344 0-15.95 3.578-21.471 9.337-5.588 5.826-9.051 13.896-9.051 22.8v2.76h-13.649zm38.258 58.723l-7.779 20.372h27.386l-7.204-20.65c4.573-2.354 7.701-7.119 7.701-12.618 0-7.836-6.352-14.188-14.19-14.188-7.835 0-14.183 6.352-14.183 14.188-.001 5.722 3.388 10.652 8.269 12.896zm-43.142-50.173h98.117c3.965 0 7.208 3.239 7.208 7.204v76.172c0 3.965-3.243 7.208-7.208 7.208h-98.117c-3.966 0-7.208-3.243-7.208-7.208v-76.172c0-3.965 3.242-7.204 7.208-7.204z"/>
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
    const [isDark, setIsDark] = useState(() => {
        const saved = (localStorage.getItem('aeroftp-theme') as Theme) || 'auto';
        if (saved === 'auto') {
            return window.matchMedia('(prefers-color-scheme: dark)').matches;
        }
        return saved === 'dark' || saved === 'tokyo' || saved === 'cyber';
    });

    useEffect(() => {
        const updateDarkMode = () => {
            const nextIsDark =
                theme === 'auto'
                    ? window.matchMedia('(prefers-color-scheme: dark)').matches
                    : (theme === 'dark' || theme === 'tokyo' || theme === 'cyber');

            setIsDark(prev => (prev === nextIsDark ? prev : nextIsDark));
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
    const t = useTranslation();

    const nextTheme = (): Theme => {
        const order: Theme[] = ['light', 'dark', 'tokyo', 'cyber', 'auto'];
        return order[(order.indexOf(theme) + 1) % 5];
    };

    const getIcon = () => {
        switch (theme) {
            case 'light': return <Sun size={18} />;
            case 'dark': return <Moon size={18} />;
            case 'tokyo': return <CherryBlossomIcon size={18} className="text-purple-400" />;
            case 'cyber': return <CyberShieldIcon size={18} className="text-emerald-400" />;
            case 'auto': return <Monitor size={18} />;
        }
    };

    const getLabel = () => {
        switch (theme) {
            case 'light': return t('settings.themeLightLabel');
            case 'dark': return t('settings.themeDarkLabel');
            case 'tokyo': return t('settings.themeTokyoLabel');
            case 'cyber': return t('settings.themeCyberLabel');
            case 'auto': return t('settings.themeAutoLabel');
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
            title={`${t('settings.themeLabel')}: ${getLabel()}`}
        >
            {getIcon()}
        </button>
    );
};
