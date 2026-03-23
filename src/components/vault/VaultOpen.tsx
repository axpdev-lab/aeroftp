// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { Eye, EyeOff, Loader2 } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { VaultState, securityLevels } from './useVaultState';

interface VaultOpenProps {
    state: VaultState;
}

export const VaultOpen: React.FC<VaultOpenProps> = ({ state }) => {
    const t = useTranslation();

    return (
        <div className="p-4 flex flex-col gap-3">
            <p className="text-sm text-gray-500 dark:text-gray-400 truncate">{state.vaultPath}</p>

            {/* Show detected version and security level */}
            {state.vaultSecurity && (() => {
                const levelConfig = securityLevels[state.vaultSecurity.level];
                const LevelIcon = levelConfig.icon;
                return (
                    <div className={`flex items-center gap-2 px-3 py-2 rounded border ${levelConfig.borderColor} bg-gray-100/50 dark:bg-gray-800/30`}>
                        <LevelIcon size={16} className={levelConfig.color} />
                        <span className={`text-sm ${levelConfig.color}`}>
                            AeroVault v{state.vaultSecurity.version} ({levelConfig.label})
                        </span>
                    </div>
                );
            })()}

            <label className="text-sm text-gray-500 dark:text-gray-400">{t('vault.password')}</label>
            <div className="relative">
                <input type={state.showPassword ? 'text' : 'password'} value={state.password}
                    onChange={e => state.setPassword(e.target.value)}
                    onKeyDown={e => e.key === 'Enter' && state.handleUnlock()}
                    className="w-full bg-gray-50 dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded px-3 py-1.5 text-sm pr-8" />
                <button tabIndex={-1} onClick={() => state.setShowPassword(!state.showPassword)} className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-500 dark:text-gray-400">
                    {state.showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                </button>
            </div>
            <div className="flex gap-2 justify-end mt-2">
                <button onClick={() => { state.resetState(); state.setMode('home'); }} className="px-3 py-1.5 text-sm hover:bg-gray-100 dark:hover:bg-gray-700 rounded">
                    {t('vault.cancel')}
                </button>
                <button onClick={state.handleUnlock} disabled={state.loading} className="flex items-center gap-2 px-4 py-1.5 bg-blue-600 hover:bg-blue-500 text-white rounded text-sm disabled:opacity-50">
                    {state.loading && <Loader2 size={14} className="animate-spin" />}
                    {t('vault.unlock')}
                </button>
            </div>
        </div>
    );
};
