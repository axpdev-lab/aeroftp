// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { X, Loader2 } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { VaultIcon } from './icons/VaultIcon';
import { useTranslation } from '../i18n';
import { useVaultState, VaultMode, securityLevels, IconProvider } from './vault/useVaultState';
import { VaultHome } from './vault/VaultHome';
import { VaultCreate } from './vault/VaultCreate';
import { VaultOpen } from './vault/VaultOpen';
import { VaultBrowse } from './vault/VaultBrowse';
import type { AeroVaultOverlaySession } from '../types';
import { useDraggableModal } from '../hooks/useDraggableModal';

interface VaultPanelProps {
    onClose: () => void;
    isConnected?: boolean;
    initialPath?: string;
    initialFiles?: string[];
    initialMode?: VaultMode;
    initialFolderPath?: string;
    iconProvider?: IconProvider;
    onOverlaySessionChange?: (session: AeroVaultOverlaySession | null) => void;
}

export type { VaultMode } from './vault/useVaultState';

export const VaultPanel: React.FC<VaultPanelProps> = ({ onClose, isConnected = false, initialPath, initialFiles, initialMode, initialFolderPath, iconProvider, onOverlaySessionChange }) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();

    const state = useVaultState({
        initialMode,
        initialPath,
        initialFiles,
        initialFolderPath,
        isConnected,
        onClose,
    });

    const vaultName = state.vaultPath.split(/[\\/]/).pop() || 'Vault';
    const currentLevelConfig = state.vaultSecurity ? securityLevels[state.vaultSecurity.level] : null;
    const LevelIcon = currentLevelConfig?.icon || VaultIcon;
    const overlaySessionRef = React.useRef<{ key: string; sessionId: string } | null>(null);

    React.useEffect(() => {
        if (!onOverlaySessionChange) return;

        let cancelled = false;

        const syncOverlay = async () => {
            if (state.mode !== 'browse' || !state.vaultPath || !state.password) {
                overlaySessionRef.current = null;
                onOverlaySessionChange(null);
                return;
            }

            const source = state.remoteLocalPath ? 'remote' : 'local';
            const key = [
                state.vaultPath,
                source,
                state.remoteVaultPath || '',
                state.remoteLocalPath || '',
            ].join('|');

            if (overlaySessionRef.current?.key === key) {
                onOverlaySessionChange({
                    sessionId: overlaySessionRef.current.sessionId,
                    mode: 'browse',
                    vaultPath: state.vaultPath,
                    source,
                    remoteVaultPath: state.remoteVaultPath || undefined,
                    remoteLocalPath: state.remoteLocalPath || undefined,
                    currentPath: state.currentDir ? `/${state.currentDir}` : '/',
                });
                return;
            }

            try {
                const unlocked = await invoke<{ session_id: string; current_path: string }>('aerovault_overlay_unlock', {
                    vaultPath: state.vaultPath,
                    password: state.password,
                    source,
                    remoteVaultPath: state.remoteVaultPath || null,
                    remoteLocalPath: state.remoteLocalPath || null,
                    idleTimeoutSeconds: 1800,
                });
                if (cancelled) return;

                overlaySessionRef.current = { key, sessionId: unlocked.session_id };
                onOverlaySessionChange({
                    sessionId: unlocked.session_id,
                    mode: 'browse',
                    vaultPath: state.vaultPath,
                    source,
                    remoteVaultPath: state.remoteVaultPath || undefined,
                    remoteLocalPath: state.remoteLocalPath || undefined,
                    currentPath: unlocked.current_path,
                });
            } catch {
                if (cancelled) return;
                overlaySessionRef.current = null;
                onOverlaySessionChange(null);
            }
        };

        void syncOverlay();

        return () => {
            cancelled = true;
        };
    }, [
        onOverlaySessionChange,
        state.mode,
        state.vaultPath,
        state.password,
        state.currentDir,
        state.remoteVaultPath,
        state.remoteLocalPath,
    ]);

    return (
        <div
            className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/60"
            role="dialog"
            aria-modal="true"
            aria-label="AeroVault"
            onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
        >
            <div
                {...modalDrag.panelProps}
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 w-full max-w-[700px] max-h-[85vh] flex flex-col animate-scale-in"
            >
                {/* Header: drag moves this modal, not the native app window. */}
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing"
                >
                    <div className="flex items-center gap-2 pointer-events-none">
                        <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width={18} height={18} fill="none" stroke="currentColor" className="text-emerald-400">
                            <path d="M12 21l.88-.38a11 11 0 006.63-9.26l.43-5.52a1 1 0 00-.76-1L12 3 4.82 4.8a1 1 0 00-.76 1l.43 5.52a11 11 0 006.63 9.26z" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
                            <rect x="9.25" y="11" width="5.5" height="4" rx="0.75" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
                            <path d="M10.25 11V9.5a1.75 1.75 0 013.5 0V11" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
                        </svg>
                        <span className="font-medium">
                            {state.mode === 'browse' ? vaultName : t('vault.title')}
                        </span>
                        {/* Security badge in browse mode */}
                        {state.mode === 'browse' && currentLevelConfig && (
                            <span className={`ml-2 px-2 py-0.5 rounded text-xs font-medium ${currentLevelConfig.bgColor} bg-opacity-20 ${currentLevelConfig.color}`}>
                                <LevelIcon size={10} className="inline mr-1" />
                                {currentLevelConfig.label}
                            </span>
                        )}
                    </div>
                    <button onClick={onClose} className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded" title={t('common.close')}><X size={18} /></button>
                </div>

                {/* Error / Success */}
                {state.error && <div className="px-4 py-2 bg-red-100 dark:bg-red-900/30 text-red-600 dark:text-red-400 text-sm">{state.error}</div>}
                {state.success && <div className="px-4 py-2 bg-green-100 dark:bg-green-900/30 text-green-600 dark:text-green-400 text-sm">{state.success}</div>}

                {/* Content */}
                {state.mode === 'home' && <VaultHome state={state} isConnected={isConnected} iconProvider={iconProvider} />}
                {state.mode === 'create' && <VaultCreate state={state} />}
                {state.mode === 'open' && <VaultOpen state={state} />}
                {state.mode === 'browse' && <VaultBrowse state={state} iconProvider={iconProvider} />}

                {/* Loading overlay */}
                {state.loading && state.mode === 'browse' && (
                    <div className="absolute inset-0 bg-black/30 flex items-center justify-center rounded-lg">
                        <Loader2 size={24} className="animate-spin text-blue-400" />
                    </div>
                )}
            </div>
        </div>
    );
};
