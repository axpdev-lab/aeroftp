// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import type { ProviderOptions } from '../types';

export interface ProviderModeBadge {
    label: string;
    longLabel: string;
    className: string;
}

export const getMegaConnectionMode = (options?: ProviderOptions): 'native' | 'megacmd' => {
    return options?.mega_mode === 'megacmd' ? 'megacmd' : 'native';
};

export const getMegaConnectionBadge = (options?: ProviderOptions): ProviderModeBadge => {
    const mode = getMegaConnectionMode(options);
    if (mode === 'native') {
        return {
            label: 'API',
            longLabel: 'Native API',
            className: 'bg-blue-100 text-blue-700 dark:bg-blue-900/50 dark:text-blue-300',
        };
    }

    return {
        label: 'CMD',
        longLabel: 'MEGAcmd',
        className: 'bg-rose-100 text-rose-700 dark:bg-rose-900/50 dark:text-rose-300',
    };
};

export const getGitHubConnectionBadge = (options?: ProviderOptions): ProviderModeBadge | null => {
    const mode = options?.githubAuthMode;
    if (!mode) return null;

    if (mode === 'app') {
        return {
            label: 'APP',
            longLabel: 'GitHub App',
            className: 'bg-purple-100 text-purple-700 dark:bg-purple-900/50 dark:text-purple-300',
        };
    }

    if (mode === 'pat') {
        return {
            label: 'PAT',
            longLabel: 'Personal Access Token',
            className: 'bg-amber-100 text-amber-700 dark:bg-amber-900/50 dark:text-amber-300',
        };
    }

    return {
        label: 'OAuth',
        longLabel: 'Device Flow OAuth',
        className: 'bg-blue-100 text-blue-700 dark:bg-blue-900/50 dark:text-blue-300',
    };
};