// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { Lock, Unlock, KeyRound, Download, Loader2 } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { VaultState, securityLevels, IconProvider } from './useVaultState';

interface VaultHomeProps {
    state: VaultState;
    isConnected?: boolean;
    iconProvider?: IconProvider;
}

export const VaultHome: React.FC<VaultHomeProps> = ({ state, isConnected }) => {
    const t = useTranslation();

    return (
        <div className="p-6 flex flex-col items-center gap-5">
            {/* AeroVault icon */}
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width={56} height={56} fill="none" stroke="currentColor" className="text-emerald-400">
                <path d="M12 21l.88-.38a11 11 0 006.63-9.26l.43-5.52a1 1 0 00-.76-1L12 3 4.82 4.8a1 1 0 00-.76 1l.43 5.52a11 11 0 006.63 9.26z" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
                <rect x="9.25" y="11" width="5.5" height="4" rx="0.75" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
                <path d="M10.25 11V9.5a1.75 1.75 0 013.5 0V11" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
            </svg>

            <p className="text-gray-600 dark:text-gray-300 text-center text-sm max-w-md">
                {t('vault.description')}
            </p>

            {/* Security levels preview */}
            <div className="flex gap-2 text-xs">
                {Object.entries(securityLevels).map(([key, config]) => {
                    const Icon = config.icon;
                    return (
                        <div key={key} className={`flex items-center gap-1.5 px-2 py-1 rounded border ${config.borderColor} bg-opacity-10`}>
                            <Icon size={12} className={config.color} />
                            <span className={config.color}>{config.label}</span>
                        </div>
                    );
                })}
            </div>

            <div className="flex flex-wrap justify-center gap-3 mt-1">
                <button onClick={() => { state.resetState(); state.setMode('create'); }} className="flex items-center gap-2 px-4 py-2 bg-emerald-600 hover:bg-emerald-500 text-white rounded text-sm font-medium">
                    <Lock size={16} /> {t('vault.create')} .aerovault
                </button>
                <button
                    onClick={() => {
                        state.resetState();
                        state.setContainerKind('zip');
                        state.setVaultSecurity({ version: 3, cascadeMode: false, level: 'experimental', plaintext: true });
                        state.setSecurityLevel('experimental');
                        state.setCompressionProfile('archive');
                        state.setMode('create');
                    }}
                    className="flex items-center gap-2 px-4 py-2 bg-amber-600 hover:bg-amber-500 text-white rounded text-sm font-medium"
                >
                    <Unlock size={16} /> {t('vault.create')} .aerozip
                </button>
                <button onClick={state.handleOpen} className="flex items-center gap-2 px-4 py-2 bg-blue-600 hover:bg-blue-500 text-white rounded text-sm font-medium">
                    <KeyRound size={16} /> {t('vault.openExisting')}
                </button>
            </div>

            {/* Cronologia lives in the sibling [Recent] tab now (VaultRecentList),
                not inline here: this is the [Start] tab landing. */}

            {/* Remote Vault: only when connected to a server */}
            {isConnected && (
                <div className="w-full max-w-md mt-2 space-y-2">
                    {!state.showRemoteInput ? (
                        <button
                            onClick={() => state.setShowRemoteInput(true)}
                            className="flex items-center gap-2 px-4 py-2 rounded text-sm font-medium
                                bg-purple-600/20 text-purple-400 hover:bg-purple-600/30 transition-colors w-full justify-center"
                        >
                            <Download size={16} />
                            {t('vault.remote.open')}
                        </button>
                    ) : (
                        <div className="p-3 rounded-lg border border-purple-500/30 bg-purple-500/5 space-y-2">
                            <p className="text-xs text-purple-400">{t('vault.remote.title')}</p>
                            <input
                                type="text"
                                value={state.remoteVaultPath}
                                onChange={e => state.setRemoteVaultPath(e.target.value)}
                                placeholder="/path/to/vault.aerovault"
                                className="w-full px-3 py-1.5 rounded text-sm bg-gray-800 border border-gray-600 text-white placeholder:text-gray-500"
                            />
                            <div className="flex gap-2">
                                <button
                                    onClick={() => { state.setShowRemoteInput(false); state.setRemoteVaultPath(''); }}
                                    className="flex-1 py-1.5 rounded text-xs bg-gray-700 text-gray-300"
                                >
                                    {t('security.totp.back')}
                                </button>
                                <button
                                    onClick={state.handleOpenRemoteVault}
                                    disabled={state.remoteLoading || !state.remoteVaultPath.endsWith('.aerovault')}
                                    className="flex-1 py-1.5 rounded text-xs bg-purple-600 text-white disabled:opacity-50 flex items-center justify-center gap-1"
                                >
                                    {state.remoteLoading ? <Loader2 size={12} className="animate-spin" /> : <Download size={12} />}
                                    {state.remoteLoading ? t('vault.remote.downloading') : t('vault.remote.open')}
                                </button>
                            </div>
                        </div>
                    )}
                </div>
            )}
        </div>
    );
};
