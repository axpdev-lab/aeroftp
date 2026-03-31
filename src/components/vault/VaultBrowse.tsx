// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { Plus, Trash2, Download, Key, FolderPlus, Eye, EyeOff, Loader2, File, Folder, Zap, ChevronRight, ArrowLeft, ArrowUpDown, Check } from 'lucide-react';
import { VaultIcon } from '../icons/VaultIcon';
import VaultSyncDialog from '../VaultSyncDialog';
import { useTranslation } from '../../i18n';
import { VaultState, securityLevels, IconProvider } from './useVaultState';
import { PasswordStrengthBar } from './PasswordStrengthBar';
import { formatSize } from '../../utils/formatters';

interface VaultBrowseProps {
    state: VaultState;
    iconProvider?: IconProvider;
}

export const VaultBrowse: React.FC<VaultBrowseProps> = ({ state, iconProvider }) => {
    const t = useTranslation();

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
            {/* Toolbar */}
            <div className="flex items-center gap-2 px-4 py-2 border-b border-gray-200 dark:border-gray-700">
                <button onClick={state.handleAddFiles} disabled={state.loading} className="flex items-center gap-1 px-2 py-1 text-xs bg-green-700 hover:bg-green-600 text-white rounded">
                    <Plus size={14} /> {t('vault.addFiles')}
                </button>
                {state.vaultSecurity?.version === 2 && (
                    <button onClick={() => { state.setShowNewDirDialog(true); state.setNewDirName(''); }} disabled={state.loading} className="flex items-center gap-1 px-2 py-1 text-xs bg-yellow-700 hover:bg-yellow-600 rounded">
                        <FolderPlus size={14} /> {t('vault.newFolder')}
                    </button>
                )}
                <button onClick={() => state.setChangingPassword(!state.changingPassword)} className="flex items-center gap-1 px-2 py-1 text-xs bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded">
                    <Key size={14} /> {t('vault.changePassword')}
                </button>
                {state.vaultSecurity?.version === 2 && (
                    <button onClick={() => state.setShowSyncDialog(true)} className="flex items-center gap-1 px-2 py-1 text-xs bg-blue-700 hover:bg-blue-600 rounded text-white">
                        <ArrowUpDown size={14} /> {t('vaultSync.title') || 'Sync'}
                    </button>
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
                        {state.vaultSecurity?.cascadeMode && (
                            <span className="flex items-center gap-0.5">
                                <Zap size={10} /> {t('vault.cascade')}
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
            {state.changingPassword && (<>
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

            {/* File list */}
            <div className="flex-1 overflow-auto relative">
                {/* Drag-and-drop overlay */}
                {state.dragOver && (
                    <div className="absolute inset-0 z-10 flex items-center justify-center bg-emerald-500/10 border-2 border-dashed border-emerald-500 rounded-lg pointer-events-none">
                        <div className="flex flex-col items-center gap-2 text-emerald-500">
                            <Plus size={32} />
                            <span className="text-sm font-medium">{t('vault.dropFiles')}</span>
                            {state.currentDir && (
                                <span className="text-xs opacity-70">/{state.currentDir}</span>
                            )}
                        </div>
                    </div>
                )}
                {sortedEntries.length === 0 ? (
                    <div className="flex flex-col items-center justify-center py-12 text-gray-500 dark:text-gray-400">
                        <VaultIcon size={32} className="mb-2 opacity-50" />
                        <p className="text-sm">{state.currentDir ? t('vault.dirEmpty') : t('vault.empty')}</p>
                        {!state.currentDir && (
                            <p className="text-xs mt-1 text-gray-400 dark:text-gray-500">
                                {t('vault.emptyHint') || 'Drag files here or click Add Files to get started'}
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
                            // Cleanup remote if needed
                            if (state.remoteLocalPath) {
                                state.handleCleanupRemote();
                            }
                        }}
                        className="flex items-center gap-1.5 px-3 py-1 bg-emerald-600 hover:bg-emerald-500 text-white rounded text-xs font-medium transition-colors"
                    >
                        <Check size={12} />
                        {t('vault.save') || 'Save'}
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
        </>
    );
};
