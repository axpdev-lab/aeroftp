// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { Archive, Clock, X as XIcon } from 'lucide-react';
import { VaultIcon } from '../icons/VaultIcon';
import { useTranslation } from '../../i18n';
import { VaultState, securityLevels, SecurityLevel } from './useVaultState';

interface VaultRecentListProps {
    state: VaultState;
    /** Outer wrapper classes (intro uses a narrow card column, the browser tab fills the panel). */
    containerClassName?: string;
    /** Scroll-area classes for the list of rows. */
    listClassName?: string;
}

/** Format a timestamp as relative time (e.g. "2 hours ago") */
function relativeTime(timestamp: number): string {
    const now = Date.now();
    const diffMs = now - timestamp * 1000; // timestamp is seconds
    const diffMin = Math.floor(diffMs / 60000);
    if (diffMin < 1) return 'just now';
    if (diffMin < 60) return `${diffMin}m ago`;
    const diffH = Math.floor(diffMin / 60);
    if (diffH < 24) return `${diffH}h ago`;
    const diffD = Math.floor(diffH / 24);
    if (diffD < 30) return `${diffD}d ago`;
    const diffM = Math.floor(diffD / 30);
    return `${diffM}mo ago`;
}

/** Map security_level string to SecurityLevel type */
function toSecurityLevel(s: string): SecurityLevel {
    if (s === 'archive') return 'experimental';
    if (s === 'aerovault-zip' || s === 'plaintext-zip' || s === 'plaintext-archive' || s === 'zip') return 'experimental';
    if (s === 'standard' || s === 'advanced' || s === 'paranoid' || s === 'experimental') return s;
    return 'advanced';
}

function isAeroVaultZipHistoryLevel(s: string): boolean {
    return s === 'aerovault-zip' || s === 'plaintext-zip' || s === 'plaintext-archive' || s === 'zip';
}

/**
 * Recent Vaults (cronologia) list, extracted from VaultHome so it can be reused
 * in BOTH the standalone intro page AND the browser's `[Recent]` tab
 * (owner-locked layout, 2026-06-24). Clicking a row opens that vault (-> the
 * unlock/browser flow); the per-row X removes it, and Clear empties the history.
 */
export const VaultRecentList: React.FC<VaultRecentListProps> = ({
    state,
    containerClassName = 'w-full max-w-md mt-2',
    listClassName = 'space-y-1.5 max-h-[200px] overflow-y-auto',
}) => {
    const t = useTranslation();

    if (state.recentVaults.length === 0) {
        return (
            <div className={containerClassName}>
                <p className="text-xs text-gray-400 dark:text-gray-500 text-center">
                    {t('vault.noRecentVaults') || 'No recently opened vaults'}
                </p>
            </div>
        );
    }

    return (
        <div className={containerClassName}>
            <div className="flex items-center justify-between mb-2">
                <h3 className="text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wide flex items-center gap-1.5">
                    <Clock size={12} />
                    {t('vault.recentVaults') || 'Recent Vaults'}
                </h3>
                <button
                    onClick={state.clearHistory}
                    className="text-[10px] text-gray-400 hover:text-red-400 transition-colors"
                >
                    {t('vault.clearHistory') || 'Clear'}
                </button>
            </div>
            <div className={listClassName}>
                {state.recentVaults.map((vault) => {
                    const level = toSecurityLevel(vault.security_level);
                    const config = securityLevels[level];
                    const isZip = isAeroVaultZipHistoryLevel(vault.security_level);
                    return (
                        <div
                            key={vault.id}
                            className="group flex items-center gap-3 px-3 py-2 rounded-lg border border-gray-200 dark:border-gray-700 hover:border-emerald-500/50 hover:bg-gray-50 dark:hover:bg-gray-800/50 cursor-pointer transition-all"
                            onClick={async () => {
                                state.setVaultPath(vault.vault_path);
                                try {
                                    const sec = await state.detectVaultVersion(vault.vault_path);
                                    state.setContainerKind(sec.plaintext ? 'zip' : 'vault');
                                    state.setVaultSecurity(sec);
                                } catch { /* ignore: VaultOpen will re-detect */ }
                                state.setMode('open');
                            }}
                        >
                            {isZip
                                ? <Archive size={20} className="text-amber-500 shrink-0" />
                                : <VaultIcon size={20} className="text-emerald-400 shrink-0" />}
                            <div className="flex-1 min-w-0">
                                <div className="flex items-center gap-2">
                                    <span className="text-sm font-medium text-gray-800 dark:text-gray-200 truncate">
                                        {vault.vault_name}
                                    </span>
                                    <span className={`px-1.5 py-0.5 rounded text-[10px] font-medium ${config.bgColor} bg-opacity-20 ${config.color}`}>
                                        {isZip ? t('vault.zipTitle') : config.label}
                                    </span>
                                </div>
                                <div className="flex items-center gap-2 text-[10px] text-gray-400">
                                    <span className="truncate">{vault.vault_path}</span>
                                    <span className="shrink-0">{relativeTime(vault.last_opened_at)}</span>
                                    {vault.file_count > 0 && (
                                        <span className="shrink-0 px-1 py-0 bg-gray-200 dark:bg-gray-700 rounded">
                                            {vault.file_count} files
                                        </span>
                                    )}
                                </div>
                            </div>
                            <button
                                onClick={(e) => {
                                    e.stopPropagation();
                                    state.removeFromHistory(vault.vault_path);
                                }}
                                className="p-1 rounded opacity-0 group-hover:opacity-100 hover:bg-red-100 dark:hover:bg-red-900/30 text-gray-400 hover:text-red-400 transition-all"
                                title={t('vault.removeFromHistory')}
                            >
                                <XIcon size={12} />
                            </button>
                        </div>
                    );
                })}
            </div>
        </div>
    );
};
