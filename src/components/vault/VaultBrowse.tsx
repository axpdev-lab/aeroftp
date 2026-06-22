// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { Plus, Trash2, Download, Key, FolderPlus, Eye, EyeOff, Loader2, File, Folder, Zap, ChevronRight, ArrowLeft, ArrowUpDown, Check, Shield, Wrench, FileDown, Scissors, Copy, SlidersHorizontal } from 'lucide-react';
import { VaultIcon } from '../icons/VaultIcon';
import VaultSyncDialog from '../VaultSyncDialog';
import { useTranslation } from '../../i18n';
import { VaultState, securityLevels, IconProvider } from './useVaultState';
import { useDraggableModal } from '../../hooks/useDraggableModal';
import { PasswordStrengthBar } from './PasswordStrengthBar';
import { formatSize } from '../../utils/formatters';
import { TransferProgressBar } from '../TransferProgressBar';

interface VaultBrowseProps {
    state: VaultState;
    iconProvider?: IconProvider;
}

export const VaultBrowse: React.FC<VaultBrowseProps> = ({ state, iconProvider }) => {
    const t = useTranslation();
    const scrubDrag = useDraggableModal();
    const repairDrag = useDraggableModal();
    const isZip = state.isPlaintextZip;

    const currentLevelConfig = state.vaultSecurity ? securityLevels[state.vaultSecurity.level] : null;
    const LevelIcon = currentLevelConfig?.icon || VaultIcon;
    // Filter entries for the current directory
    const prefix = state.currentDir ? `${state.currentDir}/` : '';
    const visibleEntries = state.entries.filter(entry => {
        if (!prefix) {
            return !entry.name.includes('/');
        }
        if (!entry.name.startsWith(prefix)) return false;
        const rest = entry.name.slice(prefix.length);
        return rest.length > 0 && !rest.includes('/');
    });

    // Sort: directories first, then files
    const sortedEntries = [...visibleEntries].sort((a, b) => {
        if (a.isDir && !b.isDir) return -1;
        if (!a.isDir && b.isDir) return 1;
        return a.name.localeCompare(b.name);
    });

    // Breadcrumb parts
    const breadcrumbParts = state.currentDir ? state.currentDir.split('/') : [];

    // Display name: just the last segment of the path
    const displayName = (fullName: string) => fullName.split('/').pop() || fullName;

    return (
        <>
            {state.loading && state.vaultProgress && (
                <div className="px-4 pt-2">
                    <TransferProgressBar percentage={state.vaultProgress.percentage} size="lg" />
                    <div className="flex justify-between text-[11px] text-gray-500 dark:text-gray-400 tabular-nums mt-1">
                        <span>{formatSize(state.vaultProgress.transferred)} / {formatSize(state.vaultProgress.total)}</span>
                        <span>{state.vaultProgress.percentage}%</span>
                    </div>
                </div>
            )}
            {isZip && (
                <div className="px-4 py-2 border-b border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/20 text-xs text-amber-700 dark:text-amber-300">
                    <span className="font-medium">{t('vault.zipPlaintextTitle')}</span>
                    <span className="ml-2">{t('vault.zipBrowseNotice')}</span>
                </div>
            )}
            {/* Toolbar */}
            <div className="flex flex-wrap items-center gap-2 px-4 py-2 border-b border-gray-200 dark:border-gray-700">
                <button onClick={state.handleAddFiles} disabled={state.loading} className="flex items-center gap-1 px-2 py-1 text-xs bg-green-700 hover:bg-green-600 text-white rounded">
                    <Plus size={14} /> {t('vault.addFiles')}
                </button>
                {state.vaultSecurity && state.vaultSecurity.version >= 2 && (
                    <button onClick={state.handleAddFolder} disabled={state.loading} className="flex items-center gap-1 px-2 py-1 text-xs bg-green-700 hover:bg-green-600 text-white rounded" title={t('vault.addFolderHint')}>
                        <FolderPlus size={14} /> {t('vault.addFolder')}
                    </button>
                )}
                {state.vaultSecurity && state.vaultSecurity.version >= 2 && (
                    <button onClick={() => { state.setShowNewDirDialog(true); state.setNewDirName(''); }} disabled={state.loading} className="flex items-center gap-1 px-2 py-1 text-xs bg-yellow-700 hover:bg-yellow-600 rounded">
                        <FolderPlus size={14} /> {t('vault.newFolder')}
                    </button>
                )}
                {state.vaultSecurity?.version === 3 && state.entries.length > 0 && (
                    <button onClick={state.handleExtractAll} disabled={state.loading} className="flex items-center gap-1 px-2 py-1 text-xs bg-blue-700 hover:bg-blue-600 text-white rounded" title={t('vault.extractAllHint')}>
                        <Download size={14} /> {t('vault.extractAll')}
                    </button>
                )}
                {!isZip && (
                    <button onClick={() => state.setChangingPassword(!state.changingPassword)} className="flex items-center gap-1 px-2 py-1 text-xs bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded">
                        <Key size={14} /> {t('vault.changePassword')}
                    </button>
                )}
                {/* Change Mode (Ehud #2): re-pack the vault under a new security level
                    (v2<->v3 / cascade), compression profile and/or Error Correction
                    (same password). Next to Change Password; available for any open
                    vault. Opening the form seeds the selector with the current level. */}
                {state.vaultSecurity && !isZip && (
                    <button onClick={() => {
                        if (!state.changingMode && state.vaultSecurity) {
                            state.setNewModeSecurityLevel(state.vaultSecurity.level);
                        }
                        state.setChangingMode(!state.changingMode);
                    }} disabled={state.loading} title={t('vault.changeModeHint')} className="flex items-center gap-1 px-2 py-1 text-xs bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded">
                        <SlidersHorizontal size={14} /> {t('vault.changeMode')}
                    </button>
                )}
                {/* Ehud #2: export a complete technical vault report (metadata,
                    encryption, error-correction, chunk stats, full file list) as
                    .txt/.json, or copy it to the clipboard. */}
                <button onClick={() => state.handleExportVaultReport('txt')} disabled={state.loading} title={t('vault.exportReportHint')} className="flex items-center gap-1 px-2 py-1 text-xs bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded">
                    <FileDown size={14} /> {t('vault.receipt.exportTxt')}
                </button>
                <button onClick={() => state.handleExportVaultReport('json')} disabled={state.loading} title={t('vault.exportReportHint')} className="flex items-center gap-1 px-2 py-1 text-xs bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded">
                    <FileDown size={14} /> {t('vault.receipt.exportJson')}
                </button>
                <button onClick={state.handleCopyVaultReport} disabled={state.loading} title={t('vault.copyReportHint')} className="flex items-center gap-1 px-2 py-1 text-xs bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded">
                    <Copy size={14} /> {t('vault.copyReport')}
                </button>
                {state.vaultSecurity?.version === 2 && (
                    <button onClick={() => state.setShowSyncDialog(true)} className="flex items-center gap-1 px-2 py-1 text-xs bg-blue-700 hover:bg-blue-600 rounded text-white">
                        <ArrowUpDown size={14} /> {t('vaultSync.title') || 'Sync'}
                    </button>
                )}

                {/* P2 Error Correction actions: for experimental v3 vaults or any vault with
                    embedded/detached recovery. Scrub/repair auto-resolve the parity source. */}
                {(state.vaultSecurity?.level === 'experimental' || state.hasErrorCorrection || state.hasDetachedRecovery) && (
                    <>
                        <button
                            onClick={state.handleScrub}
                            disabled={state.loading}
                            className="flex items-center gap-1 px-2 py-1 text-xs bg-amber-700 hover:bg-amber-600 text-white rounded"
                            title={t('vault.scrubErrorCorrection') + ' (verify cipher hashes)'}
                        >
                            <Shield size={14} /> {t('vault.scrubErrorCorrection')}
                        </button>
                        <button
                            onClick={() => { state.setRepairDryRun(true); state.setShowRepairDialog(true); }}
                            disabled={state.loading}
                            className="flex items-center gap-1 px-2 py-1 text-xs bg-rose-700 hover:bg-rose-600 text-white rounded"
                            title={t('vault.repairErrorCorrection') + ' (dry-run preview available)'}
                        >
                            <Wrench size={14} /> {t('vault.repairErrorCorrection')}
                        </button>
                        {/* SIDECAR: export a detached .aerocorrect (add/refresh parity without rewriting the container). */}
                        {!isZip && (
                            <button
                                onClick={state.exportParity}
                                disabled={state.loading || state.isExportingParity}
                                className="flex items-center gap-1 px-2 py-1 text-xs bg-emerald-700 hover:bg-emerald-600 text-white rounded"
                                title={t('vault.exportParityHint')}
                            >
                                <FileDown size={14} /> {state.isExportingParity ? t('vault.exportingParity') : t('vault.exportParity')}
                            </button>
                        )}
                        {/* Strip embedded parity (only meaningful when an in-container copy exists). */}
                        {state.hasErrorCorrection && !isZip && (
                            <button
                                onClick={() => state.stripParity(false)}
                                disabled={state.loading || state.isStrippingParity}
                                className="flex items-center gap-1 px-2 py-1 text-xs bg-gray-600 hover:bg-gray-500 text-white rounded"
                                title={t('vault.stripParityHint')}
                            >
                                <Scissors size={14} /> {state.isStrippingParity ? t('vault.strippingParity') : t('vault.stripParity')}
                            </button>
                        )}
                    </>
                )}
                {/* Remote vault: Save & Close */}
                {state.remoteLocalPath && (
                    <button
                        onClick={state.handleSaveRemoteAndClose}
                        disabled={state.loading}
                        className="flex items-center gap-1 px-2 py-1 text-xs bg-purple-600 hover:bg-purple-500 rounded text-white"
                    >
                        <Download size={14} /> {t('vault.remote.saveAndClose')}
                    </button>
                )}
                {currentLevelConfig && (
                    <div className={`ml-auto flex items-center gap-1.5 px-2 py-1 rounded text-xs ${currentLevelConfig.color} bg-gray-100/50 dark:bg-gray-800/50`}>
                        <LevelIcon size={12} />
                        <span>v{state.vaultSecurity?.version}</span>
                        {isZip && (
                            <span className="ml-1 px-1 py-0.5 rounded text-[10px] bg-amber-500/20 text-amber-700 dark:text-amber-300 border border-amber-500/30">
                                {t('vault.zipBadge')}
                            </span>
                        )}
                        {state.vaultSecurity?.cascadeMode && (
                            <span className="flex items-center gap-0.5">
                                <Zap size={10} /> {t('vault.cascade')}
                            </span>
                        )}
                        {/* P2: Error Correction badge (theme-aware). Distinguishes detached
                            sidecar parity from embedded so the user knows the storage shape. */}
                        {(state.hasErrorCorrection || state.hasDetachedRecovery || state.errorCorrectionEnabled) && (
                            <span
                                className="ml-1 px-1 py-0.5 rounded text-[10px] bg-amber-500/20 text-amber-300 border border-amber-500/30"
                                title={state.hasDetachedRecovery && !state.hasErrorCorrection
                                    ? (state.hasDetachedHeaderRecovery
                                        ? `${t('vault.detachedStableStorageNote')} ${t('vault.detachedHeaderProtectedNote')}`
                                        : t('vault.detachedStableStorageNote'))
                                    : undefined}
                            >
                                {state.hasDetachedRecovery && !state.hasErrorCorrection
                                    ? t('vault.errorCorrectionDetachedBadge')
                                    : 'Error Correction'}
                            </span>
                        )}
                    </div>
                )}
            </div>

            {/* New folder dialog */}
            {state.showNewDirDialog && (
                <div className="px-4 py-2 border-b border-gray-200 dark:border-gray-700 flex gap-2 items-center">
                    <FolderPlus size={14} className="text-yellow-400 shrink-0" />
                    <input
                        autoFocus
                        value={state.newDirName}
                        onChange={e => state.setNewDirName(e.target.value)}
                        onKeyDown={e => { if (e.key === 'Enter') state.handleCreateDirectory(); if (e.key === 'Escape') state.setShowNewDirDialog(false); }}
                        placeholder={t('vault.folderName')}
                        className="flex-1 bg-gray-50 dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded px-2 py-1 text-xs"
                    />
                    <button onClick={state.handleCreateDirectory} disabled={state.loading || !state.newDirName.trim()} className="px-2 py-1 bg-yellow-700 hover:bg-yellow-600 rounded text-xs disabled:opacity-50">
                        {t('vault.create')}
                    </button>
                    <button onClick={() => state.setShowNewDirDialog(false)} className="px-2 py-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded text-xs">
                        {t('vault.cancel')}
                    </button>
                </div>
            )}

            {/* Breadcrumb navigation */}
            {state.currentDir && (
                <div className="flex items-center gap-1 px-4 py-1.5 border-b border-gray-200 dark:border-gray-700 text-xs">
                    <button onClick={() => state.setCurrentDir('')} className="hover:text-blue-400 text-gray-500 dark:text-gray-400 flex items-center gap-0.5">
                        <ArrowLeft size={12} />
                        <VaultIcon size={12} className="text-emerald-400" />
                    </button>
                    <ChevronRight size={10} className="text-gray-500" />
                    {breadcrumbParts.map((part, idx) => {
                        const path = breadcrumbParts.slice(0, idx + 1).join('/');
                        const isLast = idx === breadcrumbParts.length - 1;
                        return (
                            <React.Fragment key={path}>
                                {isLast ? (
                                    <span className="text-gray-800 dark:text-gray-200 font-medium">{part}</span>
                                ) : (
                                    <>
                                        <button onClick={() => state.setCurrentDir(path)} className="hover:text-blue-400 text-gray-500 dark:text-gray-400">
                                            {part}
                                        </button>
                                        <ChevronRight size={10} className="text-gray-500" />
                                    </>
                                )}
                            </React.Fragment>
                        );
                    })}
                </div>
            )}

            {/* Change password form */}
                {state.changingPassword && !isZip && (<>
                <div className="px-4 py-3 border-b border-gray-200 dark:border-gray-700 flex gap-2 items-end">
                    <div className="flex-1">
                        <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1">{t('vault.newPassword')}</label>
                        <div className="relative">
                            <input type={state.showPassword ? 'text' : 'password'} value={state.newPassword} onChange={e => state.setNewPassword(e.target.value)}
                                className="w-full bg-gray-50 dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded px-2 py-1 text-xs pr-7" />
                            <button type="button" tabIndex={-1} onClick={() => state.setShowPassword(!state.showPassword)} className="absolute right-1.5 top-1/2 -translate-y-1/2 text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-300">
                                {state.showPassword ? <EyeOff size={12} /> : <Eye size={12} />}
                            </button>
                        </div>
                    </div>
                    <div className="flex-1">
                        <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1">{t('vault.confirmNew')}</label>
                        <div className="relative">
                            <input type={state.showPassword ? 'text' : 'password'} value={state.confirmNewPassword} onChange={e => state.setConfirmNewPassword(e.target.value)}
                                className="w-full bg-gray-50 dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded px-2 py-1 text-xs pr-7" />
                            <button type="button" tabIndex={-1} onClick={() => state.setShowPassword(!state.showPassword)} className="absolute right-1.5 top-1/2 -translate-y-1/2 text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-300">
                                {state.showPassword ? <EyeOff size={12} /> : <Eye size={12} />}
                            </button>
                        </div>
                    </div>
                    <button onClick={state.handleChangePassword} disabled={state.loading} className="px-3 py-1 bg-blue-600 hover:bg-blue-500 text-white rounded text-xs shrink-0">
                        {t('vault.apply')}
                    </button>
                </div>
                <div className="px-4 pb-2">
                    <PasswordStrengthBar password={state.newPassword} />
                </div>
            </>)}

            {/* Change Mode form (Ehud #2): re-pack the vault under a new security level
                (v2<->v3 / cascade), compression profile and/or Error Correction,
                keeping the same password. The Archive (v3) level unlocks the
                compression profile and Error Correction; the v2 levels hide both.
                Moving across formats is a full re-encrypt, flagged with a warning. */}
            {state.changingMode && state.vaultSecurity && !isZip && (() => {
                const targetConfig = securityLevels[state.newModeSecurityLevel];
                const targetIsV3 = targetConfig.version === 3;
                const crossFormat = targetConfig.version !== state.vaultSecurity.version
                    || (!targetIsV3 && targetConfig.cascade !== state.vaultSecurity.cascadeMode);
                return (
                    <div className="px-4 py-3 border-b border-gray-200 dark:border-gray-700 flex flex-wrap gap-3 items-end">
                        <div>
                            <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1">{t('vault.securityLevel')}</label>
                            <select value={state.newModeSecurityLevel} onChange={e => state.setNewModeSecurityLevel(e.target.value as typeof state.newModeSecurityLevel)}
                                className="bg-gray-50 dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded px-2 py-1 text-xs">
                                <option value="standard">{securityLevels.standard.label}</option>
                                <option value="advanced">{securityLevels.advanced.label}</option>
                                <option value="paranoid">{securityLevels.paranoid.label}</option>
                                <option value="experimental">{securityLevels.experimental.label}</option>
                            </select>
                        </div>
                        {targetIsV3 && (
                            <div>
                                <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1">{t('vault.compressionProfile')}</label>
                                <select value={state.newModeProfile} onChange={e => state.setNewModeProfile(e.target.value as typeof state.newModeProfile)}
                                    className="bg-gray-50 dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded px-2 py-1 text-xs">
                                    <option value="fast">Fast (zstd -3)</option>
                                    <option value="balanced">Balanced (zstd -9)</option>
                                    <option value="archive">Archive (zstd -15)</option>
                                </select>
                            </div>
                        )}
                        {targetIsV3 && (
                            <label className="flex items-center gap-1.5 text-xs text-gray-600 dark:text-gray-300">
                                <input type="checkbox" checked={state.newModeErrorCorrection} onChange={e => state.setNewModeErrorCorrection(e.target.checked)} />
                                {t('vault.enableErrorCorrection')}
                            </label>
                        )}
                        <button onClick={state.handleChangeMode} disabled={state.loading} className="px-3 py-1 bg-blue-600 hover:bg-blue-500 text-white rounded text-xs shrink-0 flex items-center gap-1">
                            {state.loading ? <Loader2 size={12} className="animate-spin" /> : t('vault.apply')}
                        </button>
                        <span className="text-[11px] text-gray-400 basis-full">{t('vault.changeModeNote')}</span>
                        {crossFormat && (
                            <span className="text-[11px] text-amber-500 basis-full">{t('vault.changeModeCrossFormat')}</span>
                        )}
                    </div>
                );
            })()}

            {/* File list */}
            <div className="flex-1 overflow-auto relative">
                {/* Drag-and-drop overlay */}
                {state.dragOver && (
                    <div className="absolute inset-0 z-10 flex items-center justify-center bg-emerald-500/10 border-2 border-dashed border-emerald-500 rounded-lg pointer-events-none">
                        <div className="flex flex-col items-center gap-2 text-emerald-500">
                            <Plus size={32} />
                            <span className="text-sm font-medium">{isZip ? t('vault.dropFilesZip') : t('vault.dropFiles')}</span>
                            {state.currentDir && (
                                <span className="text-xs opacity-70">/{state.currentDir}</span>
                            )}
                        </div>
                    </div>
                )}
                {sortedEntries.length === 0 ? (
                    <div className="flex flex-col items-center justify-center py-12 text-gray-500 dark:text-gray-400">
                        <VaultIcon size={32} className="mb-2 opacity-50" />
                        <p className="text-sm">{state.currentDir ? t('vault.dirEmpty') : (isZip ? t('vault.zipEmpty') : t('vault.empty'))}</p>
                        {!state.currentDir && (
                            <p className="text-xs mt-1 text-gray-400 dark:text-gray-500">
                                {isZip ? t('vault.zipEmptyHint') : (t('vault.emptyHint') || 'Drag files here or click Add Files to get started')}
                            </p>
                        )}
                    </div>
                ) : (
                    <table className="w-full table-fixed">
                        <thead className="text-xs uppercase text-gray-500 dark:text-gray-400 border-b border-gray-200 dark:border-gray-700 sticky top-0 bg-white dark:bg-gray-800 z-10">
                            <tr>
                                <th className="py-2 px-4 text-left font-medium">{t('vault.fileName')}</th>
                                <th className="py-2 px-3 text-right font-medium w-28">{t('vault.fileSize')}</th>
                                <th className="py-2 px-3 text-right font-medium w-20 hidden sm:table-cell">{t('browser.folderType') || 'Type'}</th>
                                <th className="py-2 px-3 text-right font-medium w-32">{t('vault.fileActions')}</th>
                            </tr>
                        </thead>
                        <tbody className="divide-y divide-gray-100 dark:divide-gray-700/50">
                            {sortedEntries.map(entry => {
                                const fname = displayName(entry.name);
                                const ext = !entry.isDir && fname.includes('.') ? fname.split('.').pop()?.toUpperCase() : '\u2014';
                                const fileIcon = iconProvider
                                    ? (entry.isDir ? iconProvider.getFolderIcon(16).icon : iconProvider.getFileIcon(fname, 16).icon)
                                    : (entry.isDir ? <Folder size={16} className="text-yellow-400 shrink-0" /> : <File size={16} className="text-gray-500 dark:text-gray-400 shrink-0" />);
                                return (
                                    <tr
                                        key={entry.name}
                                        className="cursor-pointer transition-colors hover:bg-blue-50 dark:hover:bg-gray-700 text-sm group"
                                        onDoubleClick={() => { if (entry.isDir) state.setCurrentDir(entry.name); }}
                                    >
                                        <td className="px-4 py-2">
                                            <div className="flex items-center gap-2 min-w-0">
                                                <span className="shrink-0 flex items-center">{fileIcon}</span>
                                                <span className="truncate">{fname}</span>
                                            </div>
                                        </td>
                                        <td className="px-3 py-2 text-right text-xs text-gray-500 dark:text-gray-400 whitespace-nowrap">{entry.isDir ? '\u2014' : formatSize(entry.size)}</td>
                                        <td className="px-3 py-2 text-right text-xs text-gray-500 dark:text-gray-400 uppercase hidden sm:table-cell">{entry.isDir ? t('browser.folderType') || 'Folder' : ext}</td>
                                        <td className="px-3 py-2 text-right">
                                            <div className="flex gap-1 justify-end opacity-0 group-hover:opacity-100 transition-opacity">
                                                {!entry.isDir && (
                                                    <button onClick={(e) => { e.stopPropagation(); state.handleExtract(entry.name); }} className="p-1.5 hover:bg-blue-100 dark:hover:bg-gray-600 rounded-lg transition-colors" title={t('vault.extract')}>
                                                        <Download size={14} />
                                                    </button>
                                                )}
                                                <button onClick={(e) => { e.stopPropagation(); state.handleRemove(entry.name, entry.isDir); }} className="p-1.5 hover:bg-red-100 dark:hover:bg-red-900/30 rounded-lg transition-colors text-red-400" title={t('vault.remove')}>
                                                    <Trash2 size={14} />
                                                </button>
                                            </div>
                                        </td>
                                    </tr>
                                );
                            })}
                        </tbody>
                    </table>
                )}
            </div>

            {/* Footer */}
            <div className="px-4 py-2 border-t border-gray-200 dark:border-gray-700 text-xs text-gray-500 dark:text-gray-400 flex items-center justify-between">
                <span>{sortedEntries.length} {t('vault.items')}{state.currentDir ? ` in /${state.currentDir}` : ''}</span>
                <div className="flex items-center gap-3">
                    {state.meta && <span>v{state.meta.version} | {state.entries.length} {t('vault.totalItems')}</span>}
                    <button
                        onClick={() => {
                            // Local vaults persist on every backend operation
                            // (vault_v3_add_files / delete / move / etc. all call
                            // save_open_vault under the hood), so "Save" here is
                            // just a confirm-and-close: go back to the vault home
                            // screen. Remote vaults instead need an explicit
                            // upload of the temp file before close, which is what
                            // handleSaveRemoteAndClose does.
                            if (state.remoteLocalPath) {
                                state.handleCleanupRemote();
                            } else {
                                // Clear the live password (and other vault state)
                                // so it cannot pre-fill the next unlock field
                                // (CLAUDE-AV-022).
                                state.resetState();
                                state.setMode('home');
                            }
                        }}
                        className="flex items-center gap-1.5 px-3 py-1 bg-emerald-600 hover:bg-emerald-500 text-white rounded text-xs font-medium transition-colors"
                    >
                        <Check size={12} />
                        {isZip ? t('vault.close') : (t('vault.save') || 'Save')}
                    </button>
                </div>
            </div>

            {/* Vault Sync Dialog */}
            {state.showSyncDialog && (
                <VaultSyncDialog
                    vaultPath={state.vaultPath}
                    password={state.password}
                    onClose={() => state.setShowSyncDialog(false)}
                    onSynced={state.refreshVaultEntries}
                />
            )}

            {/* P2: Draggable Scrub modal (theme-aware, follows app modal template + useDraggableModal) */}
            {state.showScrubDialog && (
                <div className="fixed inset-0 z-[100] flex items-center justify-center bg-black/40" onClick={() => state.setShowScrubDialog(false)}>
                    <div
                        {...scrubDrag.panelProps}
                        className="w-[min(92vw,560px)] rounded-xl border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-900 shadow-2xl overflow-hidden"
                        onClick={e => e.stopPropagation()}
                    >
                        <div {...scrubDrag.dragHandleProps} className="px-4 py-2.5 border-b border-gray-200 dark:border-gray-700 flex items-center justify-between cursor-move bg-gray-50 dark:bg-gray-800/60">
                            <div className="flex items-center gap-2 text-sm font-medium text-gray-700 dark:text-gray-200">
                                <Shield size={16} className="text-amber-500" /> {t('vault.errorCorrectionScrubResult')}
                            </div>
                            <button onClick={() => state.setShowScrubDialog(false)} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700">✕</button>
                        </div>
                        <div className="p-4 text-sm max-h-[60vh] overflow-auto">
                            {!state.scrubResult || (state.scrubResult.count ?? 0) === 0 ? (
                                <div className="text-emerald-600 dark:text-emerald-400">{t('vault.noDamageDetected')} {t('vault.checkedParen', { count: String(state.scrubResult?.checked ?? state.scrubResult?.count ?? 0) })}</div>
                            ) : (
                                <div>
                                    <div className="mb-2 text-amber-600 dark:text-amber-400 font-medium">{t('vault.damagedChunksFound', { count: String(state.scrubResult.count), checked: String(state.scrubResult.checked ?? '?') })}</div>
                                    <ul className="space-y-1 text-xs">
                                        {(state.scrubResult.damaged || []).map((d: any, i: number) => (
                                            <li key={i} className="p-2 rounded bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700">
                                                {d.id} — offset {d.on_disk_start} (len {d.on_disk_len})
                                            </li>
                                        ))}
                                    </ul>
                                    <div className="mt-3 text-[11px] text-gray-500 dark:text-gray-400">{t('vault.useRepairHint')}</div>
                                </div>
                            )}
                        </div>
                        <div className="p-3 border-t border-gray-200 dark:border-gray-700 flex justify-end">
                            <button onClick={() => state.setShowScrubDialog(false)} className="px-3 py-1 text-sm rounded bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600">{t('vault.close')}</button>
                            <button onClick={() => { state.setShowScrubDialog(false); state.setShowRepairDialog(true); }} className="ml-2 px-3 py-1 text-sm rounded bg-rose-600 text-white hover:bg-rose-500">{t('vault.openRepair')}</button>
                        </div>
                    </div>
                </div>
            )}

            {/* P2: Draggable Repair modal (respects themes, draggable header via hook, template consistent with other app modals) */}
            {state.showRepairDialog && (
                <div className="fixed inset-0 z-[100] flex items-center justify-center bg-black/40" onClick={() => state.setShowRepairDialog(false)}>
                    <div
                        {...repairDrag.panelProps}
                        className="w-[min(92vw,620px)] rounded-xl border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-900 shadow-2xl overflow-hidden"
                        onClick={e => e.stopPropagation()}
                    >
                        <div {...repairDrag.dragHandleProps} className="px-4 py-2.5 border-b border-gray-200 dark:border-gray-700 flex items-center justify-between cursor-move bg-gray-50 dark:bg-gray-800/60">
                            <div className="flex items-center gap-2 text-sm font-medium text-gray-700 dark:text-gray-200">
                                <Wrench size={16} className="text-rose-500" /> {t('vault.errorCorrectionRepair')}
                            </div>
                            <button onClick={() => state.setShowRepairDialog(false)} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700">✕</button>
                        </div>
                        <div className="p-4 text-sm">
                            <div className="flex items-center gap-2 mb-3">
                                <input type="checkbox" checked={state.repairDryRun} onChange={e => state.setRepairDryRun(e.target.checked)} className="accent-rose-600" />
                                <span>{t('vault.dryRunLabel')}</span>
                            </div>

                            {/* Show damaged list from last scrub if available, to make it useful before repair */}
                            {state.scrubResult && state.scrubResult.damaged && state.scrubResult.damaged.length > 0 && (
                                <div className="mb-3">
                                    <div className="text-amber-600 dark:text-amber-400 font-medium mb-1 text-xs">{t('vault.damagedFromLastScrub')}</div>
                                    <ul className="text-xs max-h-24 overflow-auto border border-gray-200 dark:border-gray-700 rounded p-2 bg-gray-50 dark:bg-gray-800">
                                        {state.scrubResult.damaged.map((d: any, i: number) => (
                                            <li key={i}>{d.id} @ {d.on_disk_start} ({d.on_disk_len}B)</li>
                                        ))}
                                    </ul>
                                </div>
                            )}

                            {state.repairResult ? (
                                <div className="p-3 rounded bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700">
                                    {(() => {
                                        const r = state.repairResult.repaired ?? 0;
                                        const d = state.repairResult.damaged ?? 0;
                                        const dry = state.repairResult.dry_run;
                                        const prefix = dry ? 'Dry-run: ' : '';
                                        if (d === 0) return `${prefix}${t('vault.noDamageNothing')}`;
                                        if (r === 0) return `${prefix}${t('vault.couldNotRepairAny', { damaged: String(d) })}`;
                                        // all-or-nothing engine: r > 0 implies r === d (all verified + persisted)
                                        return `${prefix}${t('vault.successfullyRepaired', { repaired: String(r) })}`;
                                    })()}
                                    {state.repairResult.dry_run && ' (no changes written)'}
                                </div>
                            ) : (
                                <div className="text-gray-500 dark:text-gray-400 text-xs">{t('vault.runScrubHint')}</div>
                            )}
                        </div>
                        <div className="p-3 border-t border-gray-200 dark:border-gray-700 flex gap-2 justify-end">
                            <button onClick={() => state.setShowRepairDialog(false)} className="px-3 py-1 text-sm rounded bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600">{t('vault.close')}</button>
                            <button
                                onClick={state.handleRepair}
                                disabled={state.isRepairing || state.loading}
                                className="px-3 py-1 text-sm rounded bg-rose-600 text-white hover:bg-rose-500 disabled:opacity-50"
                            >
                                {state.isRepairing ? t('vault.repairing') : (state.repairDryRun ? t('vault.previewRepair') : t('vault.repairNow'))}
                            </button>
                        </div>
                    </div>
                </div>
            )}
        </>
    );
};
