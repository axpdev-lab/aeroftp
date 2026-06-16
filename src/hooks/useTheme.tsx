// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

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
import { Sun, Moon, Monitor, MoonStar, Leaf, Snowflake, Flame } from 'lucide-react';

/** Tokyo Cherry Blossom icon (neon purple) */
const CherryBlossomIcon: React.FC<{ size?: number; className?: string }> = ({ size = 18, className }) => (
    <svg viewBox="0 0 32 32" width={size} height={size} fill="currentColor" className={className}>
        <path d="M30.43,12.124c-0.441-1.356-1.289-2.518-2.454-3.358c-1.157-0.835-2.514-1.276-3.924-1.276c-0.009,0-0.018,0-0.026,0c-0.44,0.001-0.898,0.055-1.368,0.158c-0.048-0.492-0.145-0.96-0.288-1.395c-0.442-1.345-1.287-2.498-2.442-3.335c-1.111-0.805-2.44-1.212-3.776-1.242C16.104,1.654,16.054,1.64,16,1.64c-0.054,0-0.105,0.014-0.151,0.036c-1.336,0.029-2.664,0.437-3.776,1.241c-1.155,0.837-2,1.991-2.442,3.335C9.488,6.686,9.392,7.154,9.343,7.648C8.859,7.542,8.415,7.462,7.926,7.491C6.511,7.496,5.153,7.942,4,8.783c-1.151,0.839-1.99,1.994-2.428,3.34s-0.437,2.774,0.001,4.129c0.439,1.358,1.275,2.518,2.417,3.353c0.369,0.271,0.785,0.507,1.239,0.706c-0.251,0.428-0.448,0.863-0.588,1.298c-0.432,1.349-0.427,2.778,0.016,4.135c0.443,1.354,1.282,2.51,2.427,3.341c1.145,0.832,2.503,1.272,3.927,1.275c0.003,0,0.007,0,0.01,0c1.422,0,2.78-0.437,3.926-1.263c0.371-0.268,0.724-0.589,1.053-0.96c0.319,0.36,0.659,0.673,1.013,0.932c1.145,0.839,2.509,1.285,3.946,1.291c0.007,0,0.014,0,0.021,0c1.428,0,2.789-0.441,3.938-1.275c1.153-0.838,1.995-2.004,2.435-3.37c0.439-1.368,0.438-2.804-0.007-4.152c-0.137-0.417-0.329-0.837-0.573-1.251c0.44-0.192,0.842-0.418,1.199-0.675c1.151-0.831,1.998-1.991,2.446-3.355C30.865,14.918,30.869,13.48,30.43,12.124z M16.698,22.101c0,0.385-0.313,0.698-0.698,0.698s-0.698-0.313-0.698-0.698s0.313-0.697,0.698-0.697S16.698,21.716,16.698,22.101z M15.302,8.348c0-0.385,0.313-0.698,0.698-0.698s0.698,0.313,0.698,0.698c0,0.385-0.313,0.698-0.698,0.698S15.302,8.732,15.302,8.348z"/>
    </svg>
);

/** Hacker icon (neon green): cyber theme */
const HackerIcon: React.FC<{ size?: number; className?: string }> = ({ size = 18, className }) => (
    <svg viewBox="0 0 100 100" width={size} height={size} fill="currentColor" className={className}>
        <path d="M73.142 41.007c0-.084.003-.166.003-.25 0-14.843-6.384-26.875-14.259-26.875-2.438 0-4.733 1.156-6.739 3.191a2.997 2.997 0 01-4.294 0c-2.007-2.035-4.301-3.191-6.739-3.191-7.875 0-14.26 12.032-14.26 26.875 0 .084.003.166.003.25C15.209 44.052 7.5 49.325 7.5 55.324 7.5 64.752 26.528 69.8 50 69.8s42.5-5.047 42.5-14.476c0-5.999-7.709-11.272-19.358-14.317z"/>
        <path d="M76.908 69.209c-17.939 3.926-35.878 3.926-53.817 0-1.505 0-2.611 1.508-2.249 3.068l2.776 10.722c.256 1.104 1.184 1.88 2.249 1.88l12.475 1.185c.665.063 1.337.083 1.999-.011 2.005-.285 3.887-1.279 5.227-2.917 1.309-1.601 2.82-2.517 4.431-2.517s3.122.916 4.431 2.517c1.34 1.639 3.222 2.632 5.227 2.917.662.094 1.334.075 1.999.011l12.475-1.185c1.065 0 1.994-.776 2.249-1.88l2.776-10.722c.363-1.56-.742-3.068-2.248-3.068zM42.99 79.172c-.299 2.048-3.989 3.134-8.243 2.427-4.254-.707-7.461-2.94-7.162-4.988.299-2.048 3.989-3.135 8.243-2.427s7.461 2.94 7.162 4.988zm22.263 2.427c-4.254.707-7.945-.38-8.243-2.427-.299-2.048 2.908-4.281 7.162-4.988s7.945.38 8.243 2.427c.298 2.048-2.908 4.281-7.162 4.988z"/>
    </svg>
);

/** Prancing Horse icon (Ferrari-style silhouette), source: docs/dev/img/horse.svg */
const HorseIcon: React.FC<{ size?: number; className?: string }> = ({ size = 18, className }) => (
    <svg viewBox="0 0 595.276 595.276" width={size} height={size} fill="currentColor" className={className}>
        <g transform="translate(-94.792221,-3.5161713)">
            <path d="m 600.556,344.384 c -1.567,-5.375 -5.729,-22.602 -6.758,-29.078 -0.669,-4.206 -2.632,-9.62 -5.049,-15.131 1.989,-0.37 3.875,-0.798 5.538,-1.291 9.383,-2.784 22.537,-3.939 31.137,-7.776 4.973,-2.219 8.814,-2.828 10.934,-1.925 2.267,0.965 2.776,2.796 5.983,5.56 1.979,1.705 -0.237,4.346 -0.448,9.872 -0.21,5.526 -3.263,16.812 -5.637,22.78 -2.374,5.969 -2.531,7.585 -5.607,10.712 -3.077,3.127 -3.156,7.377 -3.654,10.897 -0.452,3.198 -2.294,5.001 -4.146,7.268 -1.851,2.267 -3.543,5.238 -7.168,6.069 -2.982,0.683 -4.664,1.047 -8.235,0.864 -0.403,-0.021 -0.812,-0.065 -1.227,-0.122 -2.25,-6.031 -4.415,-14.417 -5.663,-18.699 z M 144.823,444.673 c -4.475,2.462 -16.559,10.965 -20.14,15.217 8.056,-5.371 16.112,-7.832 26.853,-7.832 10.741,0 24.839,-1.119 29.315,-16.336 -0.447,5.818 -4.923,14.993 -8.504,19.021 3.804,-0.671 9.399,-6.042 12.755,-10.518 3.357,-4.475 11.86,-15.44 18.126,-17.902 -3.133,2.685 -6.042,8.951 -7.832,12.755 -1.79,3.804 -5.371,8.727 -9.175,12.755 4.923,-2.014 9.846,-8.503 13.203,-11.86 3.357,-3.357 16.336,-10.741 21.93,-15.217 -2.685,5.595 -15.217,19.245 -22.74,24.258 17.902,-2.685 41.761,-28.957 49.146,-53.349 -1.343,14.769 -9.175,33.343 -14.322,40.727 16.559,-13.65 26.406,-43.86 28.196,-54.154 1.79,9.846 -4.923,28.867 -11.999,40.818 16.112,-14.322 22.378,-42.965 24.168,-54.602 1.732,-11.258 8.159,-26.017 26.079,-34.382 -0.203,1.854 -0.36,3.689 -0.445,5.468 -0.597,12.532 0.586,30.344 7.076,44.889 6.123,13.724 12.033,26.677 10.231,33.887 -2.626,10.505 -5.909,18.383 -9.192,26.262 -3.283,7.878 -1.313,13.131 4.596,18.383 5.909,5.252 19.576,22.107 26.141,29.329 6.566,7.222 19.693,22.154 25.063,33.119 2.647,5.404 7.385,10.741 8.727,16.783 1.493,6.717 5.072,6.645 8.239,9.812 3.166,3.167 1.475,5.414 1.909,10.5 0.314,3.689 2.853,5.304 6.55,6.228 3.697,0.924 10.845,1.396 16.73,0.941 5.152,-0.398 11.615,-2.801 10.754,-6.844 -0.917,-4.309 -2.812,-8.707 -5.214,-11.841 -2.499,-3.259 -4.172,-5.391 -8.794,-8.164 -4.622,-2.773 -5.048,-5.597 -8.215,-8.764 -3.167,-3.167 -10.666,-14.379 -15.918,-20.288 -5.252,-5.909 -20.035,-32.178 -25.287,-40.056 -5.252,-7.878 -7.087,-13.784 -3.804,-19.692 1.911,-3.44 4.966,-8.396 9.282,-13.46 7.819,3.508 13.088,5.396 18.262,7.983 7.879,3.939 24.771,6.716 35.363,13.728 10.949,7.248 20.289,10.294 24.765,15.068 2.336,2.492 5.967,3.58 8.802,3.431 2.772,-0.146 4.637,-0.163 5.677,2.437 1.04,2.6 3.07,4.884 2.184,7.988 -0.887,3.104 -2.217,5.321 0,9.312 2.217,3.991 7.538,9.756 11.973,12.416 4.435,2.661 5.321,4.434 7.095,0.444 1.774,-3.991 3.991,-11.53 5.321,-16.407 1.33,-4.878 -0.887,-10.642 -2.217,-14.19 -1.33,-3.548 -5.712,-4.172 -7.132,-8.116 -1.007,-2.797 -3.218,-5.265 -5.818,-6.825 -2.6,-1.56 -14.678,-5.45 -26.331,-11.375 -13.203,-6.713 -23.47,-13.767 -30.732,-17.156 -10.07,-4.699 -11.875,-7.323 -12.531,-14.546 -0.208,-2.283 0.677,-6.751 1.672,-11.562 0.508,-2.456 0.925,-4.864 1.161,-7.631 7.797,-8.295 16.092,-21.235 18.083,-32.848 6.968,-2.323 10.87,-5.003 16.59,-6.719 15.665,-4.699 38.455,-8.763 52.922,-9.208 21.567,-0.664 38.188,-3.978 49.935,-12.111 7.835,-5.424 13.77,-10.452 20.401,-18.148 0.838,2.218 2.317,4.67 3.423,6.847 2.617,5.155 10.412,16.495 14.937,22.896 4.524,6.401 11.595,23.895 14.969,29.001 3.375,5.106 2.84,10.922 1.742,15.102 -1.098,4.179 -10.621,21.535 -12.281,24.077 -1.661,2.542 -4.935,3.668 -6.839,7.139 -1.905,3.471 -3.116,5.76 -5.246,6.197 -2.102,0.431 -3.194,1.866 -3.94,3.359 -0.859,1.718 -3.663,3.45 -6.693,3.647 -3.03,0.197 -4.883,-0.965 -6.04,-3.149 -0.746,-1.41 -1.339,-3.131 -4.147,-4.977 -2.903,-1.908 -4.336,-3.415 -7.631,-3.982 -4.084,-0.702 -6.38,0.291 -10.729,3.617 -4.348,3.326 -8.941,7.58 -10.796,11.809 -1.855,4.229 -1.806,4.986 0.614,7.111 2.42,2.125 3.398,3.127 6.013,3.565 2.615,0.439 5.645,0.242 8.896,1.096 3.251,0.854 7.357,3.174 10.559,3.27 3.201,0.096 7.819,-1.421 13.781,-3.33 5.962,-1.909 10.728,-1.153 14.98,-1.278 4.252,-0.124 8.65,-2.692 10.041,-7.043 1.391,-4.351 6.103,-13.786 8.251,-18.186 2.148,-4.4 12.577,-19.532 15.532,-23.223 2.955,-3.691 6.131,-6.332 7.278,-9.754 1.058,-3.157 1.697,-8.102 2.13,-13.831 1.513,-1.356 3.177,-2.576 4.845,-3.801 3.337,-2.449 10.069,-5.47 15.508,-11.602 3.763,-4.243 7.068,-7.282 10.955,-9.249 4.487,-2.271 6.036,-8.021 6.346,-11.354 0.423,-4.544 2.223,-8.326 3.867,-15.181 1.644,-6.855 3.342,-17.62 4.281,-22.259 0.94,-4.639 3.965,-8.913 6.051,-13.605 2.087,-4.692 2.636,-10.348 0.186,-13.685 -2.449,-3.337 -4.848,-4.696 -6.958,-8.164 -2.11,-3.467 -6.149,-6.628 -9.573,-6.533 -4.375,0.121 -9.142,-0.278 -14.178,-1.1 -4.194,-0.684 -30.018,-5.595 -37.89,-6.848 0.759,-18.582 -5.553,-34.42 -15.029,-44.676 -7.479,-8.095 -14.323,-17.697 -15.075,-30.728 -0.815,-14.13 -5.851,-34.397 -6.125,-43.639 -0.274,-9.242 2.436,-24.14 6.661,-28.188 6.747,4.267 21.667,8.191 30.619,6.585 6.971,-1.251 15.941,2.381 19.527,2.962 3.586,0.58 3.974,1.933 6.739,5.159 2.765,3.225 4.345,5.069 9.14,4.357 4.795,-0.712 9.21,-3.669 9.21,-3.669 0,0 0.821,-2.645 -0.744,-6.272 5.349,0.256 8.186,-0.153 8.823,-5.021 0.512,-3.916 0.312,-5.314 -2.316,-9.007 -1.796,-2.524 -0.067,-5.52 -1.251,-8.056 -1.588,-3.4 -4.62,-5.098 -8.996,-6.6 -4.376,-1.502 -15.507,-6.38 -21.44,-12.4 -5.933,-6.021 -13.454,-12.993 -15.824,-15.758 -2.37,-2.765 -6.087,-2.186 -10.689,-9.767 -3.619,-5.961 -8.071,-7.742 -13.655,-7.952 -4.154,-3.129 -6.307,-13.316 -11.53,-17.668 -0.581,-0.484 -1.503,-0.478 -1.811,0.304 -2.005,5.088 -2.668,10.147 -0.044,14.49 -4.379,-2.625 -12.139,-7.409 -16.33,-11.034 -0.582,-0.504 -1.101,-0.359 -1.267,0.429 -1.317,6.254 2.159,11.326 4.935,15.055 -11.862,-3.401 -26.696,1.575 -33.848,6.982 -11.254,8.506 -15.985,15.782 -25.135,17.376 4.356,0.895 11.24,-0.484 13.61,-1.145 -3.07,2.18 -13.302,5.015 -17.128,6.562 -6.332,2.561 -11.698,6.483 -18.301,13.864 7.743,-5.455 12.526,-8.088 17.037,-9.822 3.651,-1.404 7.212,-2.138 16.201,-4.441 -10.867,3.935 -17.815,9.242 -22.134,16.043 -3.896,6.136 -12.252,19.139 -21.486,21.793 10.77,-1.456 16.951,-5.238 20.485,-8.479 -3.828,6.755 -13.187,13.089 -19.476,17.244 -6.032,3.986 -10.251,9.942 -12.503,17.165 4.682,-7.017 7.494,-9.942 13.642,-12.333 4.444,-1.729 5.69,-2.137 10.751,-4.651 -6.971,4.002 -10.671,6.129 -13.57,9.494 -2.112,2.452 -3.521,5.109 -4.858,9.322 -1.212,3.819 -3.284,8.882 -6.982,11.52 4.022,-0.272 7.493,-3.161 9.466,-5.2 2.28,-2.356 4.247,-4.815 9.708,-6.64 -4.428,2.925 -6.764,5.544 -8.151,7.31 -1.388,1.766 -7.02,8.002 -9.263,9.858 -4.977,4.12 -7.775,8.466 -9.383,18.872 2.492,-7.349 7.771,-9.921 10.288,-11.317 4.253,-2.359 5.849,-3.24 8.749,-5.387 -5.154,7.302 -10.814,20.667 -17.715,25.679 3.078,-1.091 10.006,-3.781 12.809,-5.935 -2.337,5.016 -7.389,10.579 -10.542,13.492 -3.154,2.914 -5.44,7.706 -6.709,11.676 2.807,-4.043 6.461,-5.551 9.315,-6.984 2.853,-1.433 5.119,-2.64 10.422,-6.496 -4.815,8.216 -12.995,16.836 -21.673,23.324 2.16,-0.205 5.469,-1.664 8.496,-3.423 -14.647,20.518 -31.999,31.474 -49.97,37.459 -14.818,4.935 -33.051,6.801 -46.673,11.723 -13.315,4.811 -23.722,14.057 -30.649,23.471 -3.713,5.046 -5.633,8.634 -7.98,15.045 -19.076,7.705 -32.087,4.644 -53.469,18.329 -10.715,6.858 -26.63,27.748 -38.266,29.539 7.832,0.149 15.142,-1.641 16.932,-2.238 -4.625,3.133 -16.559,8.504 -24.168,9.846 5.818,0.448 12.83,-0.783 21.11,-4.476 -10.294,6.042 -22.046,14.171 -37.371,17.455 -15.665,3.357 -22.825,11.413 -29.986,25.063 6.713,-7.832 15.358,-13.178 19.17,-14.993 4.699,-2.238 10.07,-2.536 13.949,-2.909 -7.385,1.566 -16.336,6.49 -19.469,9.399 -3.133,2.909 -14.322,16.261 -21.333,19.618 5.072,-2.089 7.534,-0.149 13.054,-4.401 -8.951,11.636 -22.98,17.787 -29.539,22.378 -8.951,6.266 -15.217,16.783 -17.902,27.525 2.685,-4.476 5.147,-7.385 8.28,-9.399 -2.909,4.476 -6.266,12.979 -8.728,18.126 5.146,-9.397 17.678,-16.558 26.181,-19.244 z"/>
        </g>
    </svg>
);

export type Theme = 'light' | 'dark' | 'truedark' | 'tokyo' | 'cyber' | 'green' | 'ice' | 'redhorse' | 'auto';

/** Resolved theme (no 'auto') */
export type EffectiveTheme = 'light' | 'dark' | 'truedark' | 'tokyo' | 'cyber' | 'green' | 'ice' | 'redhorse';

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
export const getMonacoTheme = (theme: Theme, prefersDark: boolean): 'vs' | 'vs-dark' | 'tokyo-night' | 'cyber' | 'truedark' | 'green' | 'ice' | 'redhorse' => {
    const effective = getEffectiveTheme(theme, prefersDark);
    switch (effective) {
        case 'light': return 'vs';
        case 'dark': return 'vs-dark';
        case 'truedark': return 'truedark';
        case 'tokyo': return 'tokyo-night';
        case 'cyber': return 'cyber';
        case 'green': return 'green';
        case 'ice': return 'ice';
        case 'redhorse': return 'redhorse';
        default: return 'vs-dark';
    }
};

/**
 * Map app theme to Activity Log theme
 */
export const getLogTheme = (theme: Theme, prefersDark: boolean): EffectiveTheme => {
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
        return saved === 'dark' || saved === 'truedark' || saved === 'tokyo' || saved === 'cyber' || saved === 'green' || saved === 'redhorse';
    });

    useEffect(() => {
        const updateDarkMode = () => {
            const nextIsDark =
                theme === 'auto'
                    ? window.matchMedia('(prefers-color-scheme: dark)').matches
                    : (theme === 'dark' || theme === 'truedark' || theme === 'tokyo' || theme === 'cyber' || theme === 'green' || theme === 'redhorse');

            setIsDark(prev => (prev === nextIsDark ? prev : nextIsDark));
        };
        updateDarkMode();
        localStorage.setItem('aeroftp-theme', theme);
        const mediaQuery = window.matchMedia('(prefers-color-scheme: dark)');
        mediaQuery.addEventListener('change', updateDarkMode);
        return () => mediaQuery.removeEventListener('change', updateDarkMode);
    }, [theme]);

    // Re-read theme from localStorage when a keystore import restores it.
    // The hook seeds state only once on mount, so a write into
    // localStorage by `applyLocalStorage` would otherwise stay invisible
    // until the next page reload (issue #214 C3 pt.E2).
    useEffect(() => {
        const reload = () => {
            const saved = localStorage.getItem('aeroftp-theme') as Theme | null;
            if (saved) {
                setTheme(prev => (prev === saved ? prev : saved));
            }
        };
        window.addEventListener('aeroftp-localstorage-restored', reload);
        return () => window.removeEventListener('aeroftp-localstorage-restored', reload);
    }, []);

    useEffect(() => {
        const html = document.documentElement;
        // Suppress transitions cascade-wide for one frame so WebKit can repaint
        // the new theme in a single pass instead of animating hundreds of color
        // properties. Without this guard, switching theme on a large file list
        // pegs the CPU and spins the fan.
        html.classList.add('is-changing-theme');
        html.classList.toggle('dark', isDark);
        html.classList.toggle('truedark', theme === 'truedark');
        html.classList.toggle('tokyo', theme === 'tokyo');
        html.classList.toggle('cyber', theme === 'cyber');
        html.classList.toggle('green', theme === 'green');
        html.classList.toggle('ice', theme === 'ice');
        html.classList.toggle('redhorse', theme === 'redhorse');
        // Force a layout flush so the no-transition class actually takes effect
        // before paint, then remove it on the next frame.
        // eslint-disable-next-line @typescript-eslint/no-unused-expressions
        html.offsetHeight;
        const raf = requestAnimationFrame(() => {
            html.classList.remove('is-changing-theme');
        });
        return () => cancelAnimationFrame(raf);
    }, [isDark, theme]);

    return { theme, setTheme, isDark };
};

/**
 * Theme Toggle Button Component
 * Cycles through: light -> dark -> truedark -> tokyo -> cyber -> green -> auto
 */
interface ThemeToggleProps {
    theme: Theme;
    setTheme: (t: Theme) => void;
}

export const ThemeToggle: React.FC<ThemeToggleProps> = ({ theme, setTheme }) => {
    const t = useTranslation();

    const nextTheme = (): Theme => {
        const order: Theme[] = ['light', 'dark', 'truedark', 'tokyo', 'cyber', 'green', 'ice', 'redhorse', 'auto'];
        return order[(order.indexOf(theme) + 1) % order.length];
    };

    const getIcon = () => {
        switch (theme) {
            case 'light': return <Sun size={14} />;
            case 'dark': return <Moon size={14} />;
            case 'truedark': return <MoonStar size={14} />;
            case 'tokyo': return <CherryBlossomIcon size={14} className="text-purple-400" />;
            case 'cyber': return <HackerIcon size={14} className="text-emerald-400" />;
            case 'green': return <Leaf size={14} className="text-emerald-500" />;
            case 'ice': return <Snowflake size={14} className="text-sky-400" />;
            case 'redhorse': return <HorseIcon size={14} className="text-white" />;
            case 'auto': return <Monitor size={14} />;
        }
    };

    const getLabel = () => {
        switch (theme) {
            case 'light': return t('settings.themeLightLabel');
            case 'dark': return t('settings.themeDarkLabel');
            case 'truedark': return t('settings.themeTruedarkLabel');
            case 'tokyo': return t('settings.themeTokyoLabel');
            case 'cyber': return t('settings.themeCyberLabel');
            case 'green': return t('settings.themeGreenLabel');
            case 'ice': return t('settings.themeIceLabel');
            case 'redhorse': return t('settings.themeRedlavaLabel'); // Kept key as redlava to avoid breaking translations
            case 'auto': return t('settings.themeAutoLabel');
        }
    };

    const getButtonStyle = () => {
        switch (theme) {
            case 'auto': return 'hover:bg-[var(--color-bg-tertiary)]';
            case 'light': return 'bg-amber-100 dark:bg-amber-900/40 hover:bg-amber-200 dark:hover:bg-amber-800/50';
            case 'dark': return 'bg-slate-600/40 hover:bg-slate-500/50';
            case 'truedark': return 'bg-zinc-800/60 hover:bg-zinc-700/70';
            case 'tokyo': return 'bg-purple-900/50 hover:bg-purple-800/50';
            case 'cyber': return 'bg-emerald-900/50 hover:bg-emerald-800/50';
            case 'green': return 'bg-green-900/50 hover:bg-green-800/50';
            case 'ice': return 'bg-sky-100 dark:bg-sky-900/40 hover:bg-sky-200 dark:hover:bg-sky-800/50';
            case 'redhorse': return 'bg-zinc-700/50 hover:bg-zinc-600/60';
        }
    };

    return (
        <button
            onClick={() => setTheme(nextTheme())}
            className={`w-7 h-7 flex items-center justify-center rounded-md transition-colors cursor-pointer ${getButtonStyle()}`}
            title={`${t('settings.themeLabel')}: ${getLabel()}`}
        >
            {getIcon()}
        </button>
    );
};
