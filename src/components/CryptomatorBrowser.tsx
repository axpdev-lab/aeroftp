// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { pickFile, pickSave } from '../utils/pickPath';
import { Shield, Lock, Unlock, Folder, File, Download, Upload, ArrowLeft, X, Eye, EyeOff, Loader2, Key } from 'lucide-react';
import { useTranslation } from '../i18n';
import { formatSize } from '../utils/formatters';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { useArchiveProgress } from '../hooks/useArchiveProgress';
import { useGuardedClose } from '../hooks/useGuardedClose';
import { GuardedCloseConfirm } from './GuardedCloseConfirm';
import { TransferProgressBar } from './TransferProgressBar';
import { useModalFileView } from './modalview/useModalFileView';
import { ModalViewToolbar } from './modalview/ModalViewToolbar';
import { ModalFileGrid, ModalGridItem } from './modalview/ModalFileGrid';
import { SaveAllMenu, SaveAllTarget } from './common/SaveAllMenu';
import { MountVaultButton } from './common/MountVaultButton';

interface CryptomatorBrowserProps {
    onClose: () => void;
    initialVaultPath?: string;
}

interface CryptomatorEntry {
    name: string;
    isDir: boolean;
    size: number;
    dirId: string | null;
}

interface VaultInfo {
    vaultId: string;
    name: string;
    format: number;
}

interface BreadcrumbItem {
    name: string;
    dirId: string;
}

export const CryptomatorBrowser: React.FC<CryptomatorBrowserProps> = ({ onClose, initialVaultPath }) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const modalView = useModalFileView();
    const [vaultInfo, setVaultInfo] = useState<VaultInfo | null>(null);
    const [password, setPassword] = useState('');
    const [showPassword, setShowPassword] = useState(false);
    const [vaultPath, setVaultPath] = useState(initialVaultPath || '');
    const [entries, setEntries] = useState<CryptomatorEntry[]>([]);
    const [breadcrumb, setBreadcrumb] = useState<BreadcrumbItem[]>([{ name: t('cryptomator.root'), dirId: '' }]);
    const [loading, setLoading] = useState(false);
    const [decrypting, setDecrypting] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [success, setSuccess] = useState<string | null>(null);
    // Save-All (#322): live export of the whole decrypted tree to folder/zip/.aerozip.
    const passwordRef = useRef<HTMLInputElement>(null);
    const [savingAll, setSavingAll] = useState(false);
    const [saveProgress, setSaveProgress] = useState<{ percentage: number; transferred: number; total: number } | null>(null);
    // Real byte-true decrypt progress (>=10MB plaintext only).
    const progress = useArchiveProgress(decrypting);

    // Drive the Save-All bar from the backend's `vault_progress` events while a
    // bulk export runs (the single-file path uses `archive_progress` above).
    useEffect(() => {
        if (!savingAll) { setSaveProgress(null); return; }
        let un: (() => void) | undefined;
        let alive = true;
        listen<{ percentage: number; transferred: number; total: number }>('vault_progress', e => setSaveProgress(e.payload))
            .then(u => { if (alive) un = u; else u(); });
        return () => { alive = false; un?.(); };
    }, [savingAll]);
    // Lock the modal during a decrypt or bulk export so a reflexive X can't abandon it.
    const guarded = useGuardedClose({ guard: (decrypting || savingAll) ? 'busy' : null, onClose });

    const currentDirId = breadcrumb[breadcrumb.length - 1].dirId;

    // Focus the password field as soon as the unlock form is shown (e.g. when the
    // modal opens with a vault already selected), so the user can type straight
    // away without clicking it first.
    useEffect(() => {
        if (!vaultInfo && vaultPath) {
            const id = window.setTimeout(() => passwordRef.current?.focus(), 50);
            return () => window.clearTimeout(id);
        }
    }, [vaultInfo, vaultPath]);

    const handleSelectVault = async () => {
        const selected = await pickFile({ directory: true });
        if (selected) {
            setVaultPath(selected as string);
        }
    };

    const handleUnlock = async () => {
        if (!vaultPath || !password) return;
        setLoading(true);
        setError(null);
        try {
            const info = await invoke<VaultInfo>('cryptomator_unlock', { vaultPath, password });
            setVaultInfo(info);
            const list = await invoke<CryptomatorEntry[]>('cryptomator_list', { vaultId: info.vaultId, dirId: '' });
            setEntries(list);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleLock = async () => {
        if (!vaultInfo) return;
        try {
            await invoke('cryptomator_lock', { vaultId: vaultInfo.vaultId });
        } catch (_) { /* ignore */ }
        setVaultInfo(null);
        setEntries([]);
        setBreadcrumb([{ name: 'Root', dirId: '' }]);
        setPassword('');
    };

    const navigateToDir = async (name: string, dirId: string) => {
        if (!vaultInfo) return;
        setLoading(true);
        setError(null);
        try {
            const list = await invoke<CryptomatorEntry[]>('cryptomator_list', { vaultId: vaultInfo.vaultId, dirId });
            setEntries(list);
            setBreadcrumb(prev => [...prev, { name, dirId }]);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const navigateToBreadcrumb = async (index: number) => {
        if (!vaultInfo) return;
        const target = breadcrumb[index];
        setLoading(true);
        setError(null);
        try {
            const list = await invoke<CryptomatorEntry[]>('cryptomator_list', { vaultId: vaultInfo.vaultId, dirId: target.dirId });
            setEntries(list);
            setBreadcrumb(prev => prev.slice(0, index + 1));
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleDecrypt = async (entry: CryptomatorEntry) => {
        if (!vaultInfo) return;
        const savePath = await pickSave({ defaultPath: entry.name });
        if (!savePath) return;

        setLoading(true);
        setDecrypting(true);
        setError(null);
        try {
            await invoke('cryptomator_decrypt_file', {
                vaultId: vaultInfo.vaultId,
                dirId: currentDirId,
                filename: entry.name,
                outputPath: savePath,
            });
            setSuccess(t('cryptomator.decrypted', { name: entry.name }));
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
            setDecrypting(false);
        }
    };

    // Save-All (#322, Ehud idea #1): export the whole decrypted vault tree in one
    // shot. SECURITY: this writes PLAINTEXT to the chosen location; SaveAllMenu
    // confirms that intent (with a "not encrypted" note for the .zip target) first.
    const handleSaveAll = async (target: SaveAllTarget) => {
        if (!vaultInfo) return;
        const base = vaultInfo.name.replace(/\.[^.]+$/, '') || 'vault';
        let destPath: string | null;
        if (target === 'folder') {
            destPath = await pickFile({ directory: true }) as string | null;
        } else {
            destPath = await pickSave({
                defaultPath: target === 'zip' ? `${base}.zip` : `${base}.aerozip`,
                filters: target === 'zip'
                    ? [{ name: 'Zip', extensions: ['zip'] }]
                    : [{ name: 'AeroZip', extensions: ['aerozip'] }],
            });
        }
        if (!destPath) return;

        setLoading(true);
        setSavingAll(true);
        setError(null);
        try {
            const report = await invoke<{ files: number; dirs: number; skipped: string[] }>('cryptomator_save_all', {
                vaultId: vaultInfo.vaultId,
                destPath,
                target,
            });
            const skippedNote = report.skipped.length ? ` ${t('saveAll.skipped', { count: String(report.skipped.length) })}` : '';
            setSuccess(`${t('saveAll.done', { count: String(report.files), path: destPath })}${skippedNote}`);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
            setSavingAll(false);
        }
    };

    const handleEncrypt = async () => {
        if (!vaultInfo) return;
        const selected = await pickFile({ multiple: false });
        if (!selected) return;

        setLoading(true);
        setError(null);
        try {
            await invoke('cryptomator_encrypt_file', {
                vaultId: vaultInfo.vaultId,
                dirId: currentDirId,
                inputPath: selected as string,
            });
            // Refresh listing
            const list = await invoke<CryptomatorEntry[]>('cryptomator_list', { vaultId: vaultInfo.vaultId, dirId: currentDirId });
            setEntries(list);
            setSuccess(t('cryptomator.encrypted'));
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    // --- Grid (icon) view (shared file-manager view) ---
    const gridItems: ModalGridItem[] = entries.map(entry => ({
        key: entry.name,
        label: entry.name,
        isDir: entry.isDir,
        size: entry.isDir ? undefined : entry.size,
    }));
    const gridGetIcon = (item: ModalGridItem, px: number): React.ReactNode => (
        item.isDir
            ? <Folder size={px} className="text-yellow-500 dark:text-yellow-400" />
            : <File size={px} className="text-gray-500 dark:text-gray-400" />
    );
    const gridActivate = (item: ModalGridItem) => {
        const entry = entries.find(e => e.name === item.key);
        if (!entry) return;
        if (entry.isDir && entry.dirId) navigateToDir(entry.name, entry.dirId);
        else if (!entry.isDir) handleDecrypt(entry);
    };
    const gridActions = (item: ModalGridItem): React.ReactNode => {
        const entry = entries.find(e => e.name === item.key);
        if (!entry || entry.isDir) return null;
        return (
            <button
                onClick={(e) => { e.stopPropagation(); handleDecrypt(entry); }}
                className="p-1 rounded bg-white/80 dark:bg-gray-800/80 hover:bg-blue-100 dark:hover:bg-gray-600"
                title={t('cryptomator.decryptAndSave')}
            >
                <Download size={12} />
            </button>
        );
    };

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
            <div {...modalDrag.panelProps} className="bg-white dark:bg-gray-800 text-gray-900 dark:text-gray-100 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 w-[780px] max-h-[85vh] flex flex-col animate-scale-in">
                {/* Header */}
                <div {...modalDrag.dragHandleProps} className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing">
                    <div className="flex items-center gap-2">
                        <Shield size={18} className="text-emerald-400" />
                        <span className="font-medium">
                            {vaultInfo ? vaultInfo.name : t('cryptomator.title')}
                        </span>
                        {vaultInfo && <span className="text-xs text-gray-500 dark:text-gray-400">Format {vaultInfo.format}</span>}
                    </div>
                    <div className="flex items-center gap-1">
                        {vaultInfo && (
                            <button onClick={handleLock} className="flex items-center gap-1 px-2 py-1 text-xs bg-red-700 hover:bg-red-600 rounded">
                                <Lock size={12} /> {t('cryptomator.lock')}
                            </button>
                        )}
                        <button onClick={guarded.requestClose} className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded" title={t('common.close')}><X size={18} /></button>
                    </div>
                </div>

                {/* Error / Success */}
                {error && <div className="px-4 py-2 bg-red-100 dark:bg-red-900/30 text-red-700 dark:text-red-400 text-sm">{error}</div>}
                {success && <div className="px-4 py-2 bg-green-100 dark:bg-green-900/30 text-green-700 dark:text-green-400 text-sm">{success}</div>}
                {/* Real byte-true decrypt bar; appears only for >=10MB plaintext. */}
                {progress && (
                    <div className="px-4 py-2 border-t border-gray-200 dark:border-gray-700">
                        <TransferProgressBar
                            percentage={progress.percentage}
                            transferredBytes={progress.transferred}
                            totalBytes={progress.total}
                            speedBps={progress.speedBps}
                            etaSeconds={progress.etaSeconds}
                            filename={t('cryptomator.decrypting') || 'Decrypting'}
                            size="lg"
                        />
                    </div>
                )}
                {/* Save-All export bar (whole-tree decrypt to folder/zip/.aerozip). */}
                {savingAll && saveProgress && (
                    <div className="px-4 py-2 border-t border-gray-200 dark:border-gray-700">
                        <TransferProgressBar
                            percentage={saveProgress.percentage}
                            transferredBytes={saveProgress.transferred}
                            totalBytes={saveProgress.total}
                            filename={t('saveAll.button')}
                            size="lg"
                        />
                    </div>
                )}

                {/* Unlock form */}
                {!vaultInfo && (
                    <div className="p-6 flex flex-col items-center gap-5">
                        {/* Security badge */}
                        <div className="relative">
                            <Shield size={56} className="text-emerald-400" />
                            <div className="absolute -bottom-1 -right-1 bg-emerald-500 rounded-full p-1">
                                <Lock size={12} className="text-white" />
                            </div>
                        </div>

                        <div className="text-center">
                            <p className="text-gray-600 dark:text-gray-300 text-sm max-w-md">
                                {t('cryptomator.description')}
                            </p>
                            <p className="text-gray-500 dark:text-gray-500 text-xs mt-1">
                                {t('cryptomator.readOnly')}
                            </p>
                        </div>

                        {/* Security features */}
                        <div className="grid grid-cols-2 gap-2 text-xs text-gray-500 dark:text-gray-400 max-w-sm">
                            <div className="flex items-center gap-2 bg-gray-100 dark:bg-gray-800/50 rounded px-2 py-1.5">
                                <Lock size={12} className="text-emerald-500 dark:text-emerald-400" />
                                <span>AES-GCM content</span>
                            </div>
                            <div className="flex items-center gap-2 bg-gray-100 dark:bg-gray-800/50 rounded px-2 py-1.5">
                                <Shield size={12} className="text-emerald-500 dark:text-emerald-400" />
                                <span>scrypt KDF</span>
                            </div>
                            <div className="flex items-center gap-2 bg-gray-100 dark:bg-gray-800/50 rounded px-2 py-1.5">
                                <File size={12} className="text-emerald-500 dark:text-emerald-400" />
                                <span>AES-SIV names</span>
                            </div>
                            <div className="flex items-center gap-2 bg-gray-100 dark:bg-gray-800/50 rounded px-2 py-1.5">
                                <Key size={12} className="text-emerald-500 dark:text-emerald-400" />
                                <span>AES Key Wrap</span>
                            </div>
                        </div>

                        <p className="text-emerald-600 dark:text-emerald-400/70 text-xs flex items-center gap-1">
                            <Shield size={10} /> Compatible with Cryptomator app
                        </p>

                        <button onClick={handleSelectVault} className="px-4 py-2 bg-gray-200 hover:bg-gray-300 dark:bg-gray-700 dark:hover:bg-gray-600 rounded text-sm">
                            {vaultPath || t('cryptomator.selectFolder')}
                        </button>

                        {vaultPath && (
                            <>
                                <div className="w-full max-w-sm">
                                    <label className="text-xs text-gray-500 dark:text-gray-400 block mb-1">{t('cryptomator.password')}</label>
                                    <div className="relative">
                                        <input
                                            ref={passwordRef}
                                            type={showPassword ? 'text' : 'password'}
                                            value={password}
                                            onChange={e => setPassword(e.target.value)}
                                            onKeyDown={e => e.key === 'Enter' && handleUnlock()}
                                            className="w-full bg-gray-50 dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded px-3 py-1.5 text-sm pr-8"
                                        />
                                        <button tabIndex={-1} onClick={() => setShowPassword(!showPassword)} className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-500 dark:text-gray-400">
                                            {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                                        </button>
                                    </div>
                                </div>
                                <button onClick={handleUnlock} disabled={loading} className="flex items-center gap-2 px-4 py-2 bg-emerald-600 hover:bg-emerald-500 rounded text-sm disabled:opacity-50">
                                    {loading ? <Loader2 size={14} className="animate-spin" /> : <Unlock size={14} />}
                                    {t('cryptomator.unlock')}
                                </button>
                            </>
                        )}
                    </div>
                )}

                {/* Browsing view */}
                {vaultInfo && (
                    <>
                        {/* Breadcrumb + toolbar */}
                        <div className="flex items-center gap-2 px-4 py-2 border-b border-gray-200 dark:border-gray-700">
                            {breadcrumb.length > 1 && (
                                <button onClick={() => navigateToBreadcrumb(breadcrumb.length - 2)} className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded">
                                    <ArrowLeft size={14} />
                                </button>
                            )}
                            <div className="flex items-center gap-1 text-xs text-gray-500 dark:text-gray-400 flex-1 overflow-hidden">
                                {breadcrumb.map((item, i) => (
                                    <React.Fragment key={i}>
                                        {i > 0 && <span>/</span>}
                                        <button
                                            onClick={() => navigateToBreadcrumb(i)}
                                            className="hover:text-gray-900 dark:hover:text-white truncate max-w-[120px]"
                                        >
                                            {item.name}
                                        </button>
                                    </React.Fragment>
                                ))}
                            </div>
                            <button onClick={handleEncrypt} disabled={loading} className="flex items-center gap-1 px-2 py-1 text-xs bg-emerald-700 hover:bg-emerald-600 rounded">
                                <Upload size={12} /> {t('cryptomator.encrypt')}
                            </button>
                            <SaveAllMenu disabled={loading} onExport={handleSaveAll} />
                            {vaultInfo && (
                                <MountVaultButton
                                    kind="cryptomator"
                                    vaultKey={vaultInfo.vaultId}
                                    vaultPath={vaultPath}
                                    password={password}
                                    displayName={vaultInfo.name}
                                    disabled={loading}
                                />
                            )}
                            <ModalViewToolbar view={modalView} />
                        </div>

                        {/* File list */}
                        <div className="flex-1 overflow-auto">
                            {loading && (
                                <div className="flex items-center justify-center py-12">
                                    <Loader2 size={24} className="animate-spin text-emerald-400" />
                                </div>
                            )}
                            {!loading && entries.length === 0 && (
                                <div className="flex items-center justify-center py-12 text-gray-500 dark:text-gray-400 text-sm">
                                    {t('cryptomator.empty')}
                                </div>
                            )}
                            {!loading && entries.length > 0 && modalView.viewMode === 'grid' && (
                                <ModalFileGrid
                                    items={gridItems}
                                    gridSize={modalView.gridSize}
                                    getIcon={gridGetIcon}
                                    onActivate={gridActivate}
                                    renderActions={gridActions}
                                    formatSize={formatSize}
                                />
                            )}
                            {!loading && entries.length > 0 && modalView.viewMode === 'list' && (
                                <table className="w-full">
                                    <thead className="text-xs text-gray-500 dark:text-gray-400 border-b border-gray-200 dark:border-gray-700 sticky top-0 bg-gray-50 dark:bg-gray-800">
                                        <tr>
                                            <th className="py-2 px-3 text-left">{t('cryptomator.name')}</th>
                                            <th className="py-2 px-3 text-right w-24">{t('cryptomator.size')}</th>
                                            <th className="py-2 px-3 text-right w-20"></th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {entries.map(entry => (
                                            <tr
                                                key={entry.name}
                                                className="hover:bg-gray-100 dark:hover:bg-gray-700/30 text-sm cursor-pointer"
                                                onDoubleClick={() => entry.isDir && entry.dirId && navigateToDir(entry.name, entry.dirId)}
                                            >
                                                <td className="py-1.5 px-3 flex items-center gap-2">
                                                    {entry.isDir
                                                        ? <Folder size={14} className="text-yellow-500 dark:text-yellow-400 shrink-0" />
                                                        : <File size={14} className="text-gray-500 dark:text-gray-400 shrink-0" />}
                                                    <span className="truncate">{entry.name}</span>
                                                </td>
                                                <td className="py-1.5 px-3 text-right text-gray-500 dark:text-gray-400">{entry.isDir ? '' : formatSize(entry.size)}</td>
                                                <td className="py-1.5 px-3 text-right">
                                                    {!entry.isDir && (
                                                        <button onClick={() => handleDecrypt(entry)} className="p-1 hover:bg-gray-200 dark:hover:bg-gray-600 rounded" title={t('cryptomator.decryptAndSave')}>
                                                            <Download size={14} />
                                                        </button>
                                                    )}
                                                </td>
                                            </tr>
                                        ))}
                                    </tbody>
                                </table>
                            )}
                        </div>

                        {/* Footer */}
                        <div className="px-4 py-2 border-t border-gray-200 dark:border-gray-700 text-xs text-gray-500 dark:text-gray-400">
                            {entries.length} {t('cryptomator.items')}
                        </div>
                    </>
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
