// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { X, Loader2, Archive, Lock, Unlock, Home, Files } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { VaultIcon } from './icons/VaultIcon';
import { useTranslation } from '../i18n';
import { useVaultState, VaultMode, VaultContainerKind, securityLevels, IconProvider } from './vault/useVaultState';
import { VaultHome } from './vault/VaultHome';
import { VaultCreate } from './vault/VaultCreate';
import { VaultOpen } from './vault/VaultOpen';
import { VaultBrowse } from './vault/VaultBrowse';
import { VaultViewTabs, VaultViewTab } from './vault/VaultViewTabs';
import { VaultRecentList } from './vault/VaultRecentList';
import { VaultCreateReceipt } from './vault/VaultCreateReceipt';
import { VaultReceipt } from './vault/VaultReceipt';
import { ZipCompressionReport } from './vault/ZipCompressionReport';
import type { AeroVaultOverlaySession } from '../types';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { useActivityLog } from '../hooks/useActivityLog';
import { useGuardedClose } from '../hooks/useGuardedClose';
import { GuardedCloseConfirm } from './GuardedCloseConfirm';

interface VaultPanelProps {
    onClose: () => void;
    isConnected?: boolean;
    initialPath?: string;
    initialFiles?: string[];
    initialMode?: VaultMode;
    initialContainerKind?: VaultContainerKind;
    initialFolderPath?: string;
    initialOutputDir?: string;
    iconProvider?: IconProvider;
    onOverlaySessionChange?: (session: AeroVaultOverlaySession | null) => void;
    onVaultCreated?: (createdDir: string) => void;
}

export type { VaultMode } from './vault/useVaultState';

export const VaultPanel: React.FC<VaultPanelProps> = ({ onClose, isConnected = false, initialPath, initialFiles, initialMode, initialContainerKind, initialFolderPath, initialOutputDir, iconProvider, onOverlaySessionChange, onVaultCreated }) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const { log } = useActivityLog();

    const state = useVaultState({
        initialMode,
        initialContainerKind,
        initialPath,
        initialFiles,
        initialFolderPath,
        initialOutputDir,
        isConnected,
        onClose,
        onVaultCreated,
        onActivityLog: (message, details) => log('SUCCESS', message, 'success', details),
    });

    // Lock the modal while a vault operation runs (create/add/extract): the backdrop
    // is inert and the X routes through a confirm, so a stray click or reflexive close
    // can never abandon an in-flight big-file op (the user would lose track of it).
    const guarded = useGuardedClose({ guard: state.loading ? 'busy' : null, onClose });

    // Unified tabbed shell (owner-locked layout, 2026-06-24): the standalone intro
    // modal AND the vault browser share one [primary]+[Recent] tab strip. The
    // primary tab is "Home" (the create/open landing) standalone, "Files" (the
    // browser/unlock) once a vault is open; [Recent] is the same reusable
    // cronologia in both. create + receipt stay tab-free + history-free. The active
    // tab resets to the primary on each mode change but the user can switch to
    // [Recent] within a mode.
    const isTabbedContext = state.mode === 'home' || state.mode === 'open' || state.mode === 'browse';
    const [activeTab, setActiveTab] = React.useState<VaultViewTab>('files');
    React.useEffect(() => {
        if (state.mode === 'home' || state.mode === 'open' || state.mode === 'browse') setActiveTab('files');
    }, [state.mode]);
    const primaryTabLabel = state.mode === 'home' ? t('localTabs.home') : t('vault.receipt.files');
    const PrimaryTabIcon = state.mode === 'home' ? Home : Files;

    const loggedReportRef = React.useRef<unknown>(null);
    React.useEffect(() => {
        const r = state.lastReport;
        if (!r || loggedReportRef.current === r) return;
        loggedReportRef.current = r;
        log(
            'SUCCESS',
            t('vault.receipt.activity', { op: r.operation, files: String(r.files), fmt: String(r.vault_format) }),
            'success',
            `pipeline=${r.algorithms.join(' -> ')} | plaintext=${r.plaintext_bytes} encrypted=${r.encrypted_bytes} ratio=${r.compression_ratio_pct.toFixed(1)}% ms=${r.ms_total}`,
        );
    }, [state.lastReport, log, t]);

    const vaultName = state.vaultPath.split(/[\\/]/).pop() || 'Vault';
    const currentLevelConfig = state.vaultSecurity ? securityLevels[state.vaultSecurity.level] : null;
    const LevelIcon = currentLevelConfig?.icon || VaultIcon;
    const overlaySessionRef = React.useRef<{ key: string; sessionId: string } | null>(null);

    React.useEffect(() => {
        if (!onOverlaySessionChange) return;

        let cancelled = false;

        const syncOverlay = async () => {
            if (state.isPlaintextZip || state.mode !== 'browse' || !state.vaultPath || !state.password || !state.vaultSecurity || state.vaultSecurity.version < 2) {
                const previousSessionId = overlaySessionRef.current?.sessionId;
                overlaySessionRef.current = null;
                onOverlaySessionChange(null);
                if (previousSessionId) {
                    try {
                        await invoke('aerovault_overlay_lock', { sessionId: previousSessionId });
                    } catch {
                        // Best effort cleanup.
                    }
                }
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
        state.isPlaintextZip,
        state.mode,
        state.vaultPath,
        state.password,
        state.vaultSecurity?.version,
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
            onClick={(e) => {
                // A click outside closes the modal ONLY on the home screen. Once the
                // user is opening, creating or browsing a vault (password entered /
                // unlocked session), the backdrop is inert so a stray click cannot
                // discard the work and force re-entering the password. The X (or ESC)
                // is the only way out of those states.
                if (e.target === e.currentTarget && state.mode === 'home') guarded.requestBackdropClose();
            }}
        >
            <div
                {...modalDrag.panelProps}
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 w-full max-w-[840px] max-h-[88vh] flex flex-col animate-scale-in"
            >
                {/* Header: drag moves this modal, not the native app window. */}
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing"
                >
                    <div className="flex items-center gap-2 pointer-events-none">
                        {state.isPlaintextZip
                            ? <Archive size={18} className="text-amber-500" />
                            : <VaultIcon variant="outline" size={18} className="text-gray-600 dark:text-gray-300" />}
                        <span className="font-medium">
                            {(state.mode === 'browse' || state.mode === 'receipt') ? vaultName : (state.isPlaintextZip ? t('vault.zipTitle') : t('vault.title'))}
                        </span>
                        {/* Encryption badge: amber open padlock (plaintext Zip) or
                            green closed padlock (encrypted vault). Informational,
                            not a warning; the full explanation is the tooltip.
                            pointer-events-auto so the tooltip shows; data-modal-drag-ignore
                            keeps a pointerdown on it from starting the modal drag. */}
                        <span
                            data-modal-drag-ignore
                            className={`pointer-events-auto ml-1 inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs font-medium ${state.isPlaintextZip
                                ? 'bg-amber-500/15 text-amber-600 dark:text-amber-400'
                                : 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400'}`}
                            title={state.isPlaintextZip ? t('vault.zipPlaintextDesc') : t('vault.encryptedTooltip')}
                        >
                            {state.isPlaintextZip ? <Unlock size={11} /> : <Lock size={11} />}
                            {state.isPlaintextZip ? t('vault.notEncryptedBadge') : t('vault.encryptedBadge')}
                        </span>
                        {/* Security badge in browse mode */}
                        {state.mode === 'browse' && currentLevelConfig && (
                            <span className={`ml-2 px-2 py-0.5 rounded text-xs font-medium ${currentLevelConfig.bgColor} bg-opacity-20 ${currentLevelConfig.color}`}>
                                <LevelIcon size={10} className="inline mr-1" />
                                {state.isPlaintextZip ? t('vault.zipBadge') : currentLevelConfig.label}
                            </span>
                        )}
                    </div>
                    <button onClick={guarded.requestClose} className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded" title={t('common.close')}><X size={18} /></button>
                </div>

                {/* Error / Success */}
                {state.error && <div className="px-4 py-2 bg-red-100 dark:bg-red-900/30 text-red-600 dark:text-red-400 text-sm">{state.error}</div>}
                {state.success && state.mode !== 'receipt' && <div className="px-4 py-2 bg-green-100 dark:bg-green-900/30 text-green-600 dark:text-green-400 text-sm">{state.success}</div>}
                {state.zipReport && state.mode === 'browse' && <ZipCompressionReport inputBytes={state.zipReport.inputBytes} outputBytes={state.zipReport.outputBytes} />}

                {/* Unified tab bar: standalone intro ([Home]+[Recent]) and the vault
                    browser ([Files]+[Recent]) share it. create + receipt are tab-free. */}
                {isTabbedContext && (
                    <VaultViewTabs
                        activeTab={activeTab}
                        onSelect={setActiveTab}
                        primaryLabel={primaryTabLabel}
                        primaryIcon={PrimaryTabIcon}
                    />
                )}

                {/* Content */}
                {state.mode === 'create' && <VaultCreate state={state} />}
                {state.mode === 'receipt' && <VaultCreateReceipt state={state} onClose={guarded.requestClose} />}

                {/* Primary tab: the intro landing (home), the unlock prompt (open) or
                    the browser (browse). */}
                {isTabbedContext && activeTab === 'files' && state.mode === 'home' && (
                    <VaultHome state={state} isConnected={isConnected} iconProvider={iconProvider} />
                )}
                {isTabbedContext && activeTab === 'files' && state.mode === 'open' && (
                    <VaultOpen state={state} />
                )}
                {isTabbedContext && activeTab === 'files' && state.mode === 'browse' && (
                    <VaultBrowse state={state} iconProvider={iconProvider} />
                )}

                {/* [Recent] tab: the reusable cronologia, identical standalone and in
                    the browser. */}
                {isTabbedContext && activeTab === 'recent' && (
                    <VaultRecentList
                        state={state}
                        containerClassName="p-4 w-full"
                        listClassName="space-y-1.5 overflow-y-auto max-h-[60vh]"
                    />
                )}

                {/* Loading overlay */}
                {state.loading && state.mode === 'browse' && (
                    <div className="absolute inset-0 bg-black/30 flex items-center justify-center rounded-lg">
                        <Loader2 size={24} className="animate-spin text-blue-400" />
                    </div>
                )}

                {/* Behind-the-scenes technical receipt for an ADD during browse.
                    After a create the terminal receipt (VaultCreateReceipt, mode
                    'receipt') owns the technical-receipt toggle, so this auto-overlay
                    is scoped to browse to avoid stacking two receipts. */}
                {state.lastReport && state.mode === 'browse' && (
                    <VaultReceipt report={state.lastReport} t={t} onClose={state.clearReport} />
                )}
            </div>
            {guarded.confirmOpen && guarded.confirmKind && (
                <GuardedCloseConfirm
                    kind={guarded.confirmKind}
                    onKeep={guarded.cancelConfirm}
                    onConfirm={guarded.confirmAndClose}
                />
            )}
        </div>
    );
};
