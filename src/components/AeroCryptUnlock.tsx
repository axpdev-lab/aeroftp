// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Native AeroCrypt overlay modal (P2b). Mirrors RcloneCryptUnlock.tsx but drives
// the native aerocrypt_provider_* backend on our own audited codec. It is
// deliberately simpler than the rclone modal: the native overlay always encrypts
// names with AES-256-SIV (no filename-encryption / dir-name / dirIV options), and
// the salt is generated automatically and stored in .aeroftp-crypt.json on the
// remote, so there is no salt field. Opening an existing overlay reads that
// config from the provider's current directory first (aerocrypt_provider_read_config).
//
// i18n note: this modal uses the dedicated `aerocryptNative.*` namespace. The
// rclone modal still owns `aerocrypt.*`; the coordinated split/rename is P6.

import * as React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Lock, Unlock, Loader2, X, Folder, FolderUp, Upload, FileText } from 'lucide-react';
import { useTranslation } from '../i18n';
import { open, save } from '@tauri-apps/plugin-dialog';
import { PasswordInput } from './common/PasswordInput';
import { PasswordMatchHint } from './common/PasswordMatchHint';
import { PasswordStrengthBar } from './vault/PasswordStrengthBar';

interface AeroCryptUnlockProps {
    onClose: () => void;
    onUnlocked?: (vaultId: string) => void;
    onLocked?: () => void;
    activeVaultId?: string | null;
}

interface AeroCryptVaultInfo {
    vault_id: string;
    version: number;
    config_json: string;
}

interface AeroCryptBrowserEntry {
    name: string;
    path: string;
    is_dir: boolean;
    size: number;
    modified: string | null;
    permissions: string | null;
    decrypted_name: string;
    decrypt_ok: boolean;
}

interface AeroCryptBrowserListResponse {
    current_path: string;
    display_current_path: string;
    files: AeroCryptBrowserEntry[];
}

export const AeroCryptUnlock: React.FC<AeroCryptUnlockProps> = ({ onClose, onUnlocked, onLocked, activeVaultId }) => {
    const t = useTranslation();
    const [mode, setMode] = useState<'open' | 'create'>('open');
    const [password, setPassword] = useState('');
    const [confirmPassword, setConfirmPassword] = useState('');
    const [createSubpath, setCreateSubpath] = useState('');
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [vaultInfo, setVaultInfo] = useState<AeroCryptVaultInfo | null>(null);
    const [success, setSuccess] = useState<string | null>(null);

    const [browserPath, setBrowserPath] = useState('.');
    const [browserFiles, setBrowserFiles] = useState<AeroCryptBrowserEntry[]>([]);
    const [browserLoading, setBrowserLoading] = useState(false);
    const vaultInfoRef = useRef<AeroCryptVaultInfo | null>(null);

    useEffect(() => {
        vaultInfoRef.current = vaultInfo;
    }, [vaultInfo]);

    useEffect(() => {
        if (!activeVaultId || vaultInfoRef.current?.vault_id === activeVaultId) return;
        setVaultInfo({ vault_id: activeVaultId, version: 2, config_json: '' });
    }, [activeVaultId]);

    const clearSensitiveState = useCallback(() => {
        setVaultInfo(null);
        setPassword('');
        setConfirmPassword('');
        setSuccess(null);
    }, []);

    const lockVault = useCallback(async (vaultId: string) => {
        await invoke('aerocrypt_lock', { vaultId });
    }, []);

    const handleUnlock = async () => {
        if (!password) return;
        setLoading(true);
        setError(null);
        try {
            // The native overlay's salt lives in .aeroftp-crypt.json on the
            // remote: read it from the current directory before deriving the key.
            const configJson = await invoke<string | null>('aerocrypt_provider_read_config', {});
            if (!configJson) {
                setError(t('aerocryptNative.noOverlayFound'));
                return;
            }
            const info = await invoke<AeroCryptVaultInfo>('aerocrypt_unlock', {
                password,
                configJson,
            });
            setVaultInfo(info);
            onUnlocked?.(info.vault_id);
            setPassword('');
            setSuccess(t('aerocryptNative.unlocked'));
            setBrowserPath('.');
            setBrowserFiles([]);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleCreate = async () => {
        if (!password || password !== confirmPassword) return;
        setLoading(true);
        setError(null);
        try {
            const info = await invoke<AeroCryptVaultInfo>('aerocrypt_provider_create_remote', {
                password,
                targetSubpath: createSubpath.trim() ? createSubpath.trim() : null,
            });
            setVaultInfo(info);
            onUnlocked?.(info.vault_id);
            setPassword('');
            setConfirmPassword('');
            setCreateSubpath('');
            setSuccess(t('aerocryptNative.initialised'));
            setBrowserPath('.');
            setBrowserFiles([]);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleLock = async () => {
        if (!vaultInfo) return;
        try {
            await lockVault(vaultInfo.vault_id);
        } catch (_) {
            // Ignore lock errors, local state still needs cleanup.
        }
        clearSensitiveState();
        onLocked?.();
    };

    const loadBrowser = useCallback(async (targetPath?: string) => {
        const currentVault = vaultInfoRef.current;
        if (!currentVault) return;

        setBrowserLoading(true);
        setError(null);
        try {
            const result = await invoke<AeroCryptBrowserListResponse>('aerocrypt_provider_list', {
                vaultId: currentVault.vault_id,
                path: targetPath ?? null,
            });
            const sorted = [...result.files].sort((a, b) => {
                if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
                return a.decrypted_name.localeCompare(b.decrypted_name);
            });
            setBrowserPath(result.display_current_path || result.current_path);
            setBrowserFiles(sorted);
        } catch (e) {
            setError(String(e));
        } finally {
            setBrowserLoading(false);
        }
    }, []);

    useEffect(() => {
        if (vaultInfo) {
            void loadBrowser('.');
        }
    }, [vaultInfo, loadBrowser]);

    const handleDownloadRemoteFile = async (entry: AeroCryptBrowserEntry) => {
        const currentVault = vaultInfoRef.current;
        if (!currentVault || entry.is_dir) return;

        const outputPath = await save({ defaultPath: entry.decrypted_name || 'decrypted_file' });
        if (!outputPath) return;

        setLoading(true);
        setError(null);
        try {
            await invoke<string>('aerocrypt_provider_download_file', {
                vaultId: currentVault.vault_id,
                remoteEncryptedPath: entry.path,
                outputPath,
            });
            setSuccess(t('aerocryptNative.fileDecryptedTo', { path: outputPath }));
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleUploadToCurrentDir = async () => {
        const currentVault = vaultInfoRef.current;
        if (!currentVault) return;

        const inputPath = await open({ multiple: false });
        if (!inputPath || Array.isArray(inputPath)) return;

        const localName = inputPath.split(/[\\/]/).pop() || 'upload.bin';

        setLoading(true);
        setError(null);
        try {
            const remotePath = await invoke<string>('aerocrypt_provider_upload_file', {
                vaultId: currentVault.vault_id,
                localPlaintextPath: inputPath,
                remotePlainName: localName,
            });
            setSuccess(t('aerocryptNative.encryptedUploadCompleted', { path: remotePath }));
            await loadBrowser('.');
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    return (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
            <div className="bg-white dark:bg-gray-800 rounded-lg shadow-xl w-full max-w-lg mx-4 max-h-[90vh] overflow-y-auto">
                <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <Lock size={20} className="text-emerald-600 dark:text-emerald-400" />
                        <h2 className="text-lg font-semibold text-gray-900 dark:text-white">
                            {t('aerocryptNative.title')}
                        </h2>
                        <span className="px-1.5 py-0.5 text-[10px] font-semibold rounded bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-300">
                            {t('aerocryptNative.recommended')}
                        </span>
                    </div>
                    <button onClick={onClose} className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded">
                        <X className="w-5 h-5 text-gray-500" />
                    </button>
                </div>

                <div className="p-4 space-y-4">
                    {!vaultInfo && (
                        <div className="text-xs leading-relaxed p-3 rounded border border-emerald-400/30 bg-emerald-500/10 text-gray-700 dark:text-gray-200">
                            <div className="font-semibold mb-1 text-emerald-600 dark:text-emerald-300">{t('aerocryptNative.intro.heading')}</div>
                            <p className="mb-1" dangerouslySetInnerHTML={{ __html: t('aerocryptNative.intro.p1') }} />
                            <p className="mb-1" dangerouslySetInnerHTML={{ __html: t('aerocryptNative.intro.p2') }} />
                            <p dangerouslySetInnerHTML={{ __html: t('aerocryptNative.intro.p3') }} />
                        </div>
                    )}
                    {error && (
                        <div className="p-3 bg-red-50 dark:bg-red-900/30 text-red-700 dark:text-red-300 rounded text-sm break-words">
                            {error}
                        </div>
                    )}
                    {success && (
                        <div className="p-3 bg-green-50 dark:bg-green-900/30 text-green-700 dark:text-green-300 rounded text-sm break-words">
                            {success}
                        </div>
                    )}

                    {!vaultInfo ? (
                        <>
                            <div className="flex gap-2">
                                <button
                                    type="button"
                                    onClick={() => { setMode('open'); setError(null); setConfirmPassword(''); }}
                                    className={`flex-1 px-3 py-1.5 rounded text-sm font-medium ${mode === 'open' ? 'bg-emerald-600 text-white' : 'bg-gray-200 dark:bg-gray-700 text-gray-700 dark:text-gray-200 hover:bg-gray-300 dark:hover:bg-gray-600'}`}
                                >
                                    {t('aerocryptNative.openExisting')}
                                </button>
                                <button
                                    type="button"
                                    onClick={() => { setMode('create'); setError(null); setConfirmPassword(''); }}
                                    className={`flex-1 px-3 py-1.5 rounded text-sm font-medium ${mode === 'create' ? 'bg-emerald-600 text-white' : 'bg-gray-200 dark:bg-gray-700 text-gray-700 dark:text-gray-200 hover:bg-gray-300 dark:hover:bg-gray-600'}`}
                                >
                                    {t('aerocryptNative.createNew')}
                                </button>
                            </div>

                            <p className="text-xs text-gray-500 dark:text-gray-400">
                                {mode === 'open' ? t('aerocryptNative.openHint') : t('aerocryptNative.createHint')}
                            </p>

                            <div>
                                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                    {t('aerocryptNative.password')}
                                </label>
                                <PasswordInput
                                    value={password}
                                    onChange={setPassword}
                                    onKeyDown={(e) => e.key === 'Enter' && (mode === 'open' ? handleUnlock() : handleCreate())}
                                    placeholder={t('aerocryptNative.passwordPlaceholder')}
                                    ariaLabel={t('aerocryptNative.password')}
                                    autoFocus
                                />
                                {mode === 'create' && password.length > 0 && (
                                    <div className="mt-2">
                                        <PasswordStrengthBar password={password} />
                                    </div>
                                )}
                            </div>

                            {mode === 'create' && (
                                <div>
                                    <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                        {t('password.confirm')}
                                    </label>
                                    <PasswordInput
                                        value={confirmPassword}
                                        onChange={setConfirmPassword}
                                        onKeyDown={(e) => e.key === 'Enter' && handleCreate()}
                                        placeholder={t('password.confirmPlaceholder')}
                                        ariaLabel={t('password.confirm')}
                                    />
                                    <PasswordMatchHint password={password} confirm={confirmPassword} />
                                </div>
                            )}

                            {mode === 'create' && (
                                <div>
                                    <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                        {t('aerocryptNative.targetSubpath')}
                                    </label>
                                    <input
                                        type="text"
                                        value={createSubpath}
                                        onChange={(e) => setCreateSubpath(e.target.value)}
                                        className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded bg-white dark:bg-gray-700 text-gray-900 dark:text-white"
                                        placeholder={t('aerocryptNative.targetSubpathPlaceholder')}
                                    />
                                    <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">
                                        {t('aerocryptNative.targetSubpathHint')}
                                    </p>
                                </div>
                            )}

                            {mode === 'open' ? (
                                <button
                                    onClick={handleUnlock}
                                    disabled={!password || loading}
                                    className="w-full flex items-center justify-center gap-2 px-4 py-2 bg-emerald-600 text-white rounded hover:bg-emerald-700 disabled:opacity-50 disabled:cursor-not-allowed"
                                >
                                    {loading ? <Loader2 className="w-4 h-4 animate-spin" /> : <Unlock className="w-4 h-4" />}
                                    {t('aerocryptNative.unlock')}
                                </button>
                            ) : (
                                <button
                                    onClick={handleCreate}
                                    disabled={!password || password !== confirmPassword || loading}
                                    className="w-full flex items-center justify-center gap-2 px-4 py-2 bg-emerald-600 text-white rounded hover:bg-emerald-700 disabled:opacity-50 disabled:cursor-not-allowed"
                                >
                                    {loading ? <Loader2 className="w-4 h-4 animate-spin" /> : <Lock className="w-4 h-4" />}
                                    {t('aerocryptNative.createAndUnlock')}
                                </button>
                            )}
                        </>
                    ) : (
                        <>
                            <div className="flex items-center gap-2 p-3 bg-green-50 dark:bg-green-900/30 rounded">
                                <Unlock className="w-5 h-5 text-green-600 dark:text-green-400" />
                                <span className="text-sm text-green-700 dark:text-green-300">
                                    {t('aerocryptNative.remoteUnlocked', { id: vaultInfo.vault_id.slice(0, 8) })}
                                </span>
                                <span className="ml-auto text-[11px] text-gray-500 dark:text-gray-400">
                                    {t('aerocryptNative.versionLabel', { version: vaultInfo.version })}
                                </span>
                            </div>

                            <div className="border border-gray-200 dark:border-gray-700 rounded p-3 space-y-2">
                                <div className="flex items-center justify-between gap-2">
                                    <h3 className="text-sm font-medium text-gray-700 dark:text-gray-300 flex items-center gap-1">
                                        <Folder className="w-4 h-4" />
                                        {t('aerocryptNative.remoteBrowser')}
                                    </h3>
                                    <div className="flex items-center gap-1">
                                        <button
                                            onClick={() => void handleUploadToCurrentDir()}
                                            disabled={loading || browserLoading}
                                            className="px-2 py-1 text-xs bg-gray-100 dark:bg-gray-700 rounded hover:bg-gray-200 dark:hover:bg-gray-600 disabled:opacity-50"
                                        >
                                            <span className="inline-flex items-center gap-1"><Upload className="w-3 h-3" /> {t('aerocryptNative.upload')}</span>
                                        </button>
                                        <button
                                            onClick={() => void loadBrowser('..')}
                                            disabled={browserLoading}
                                            className="px-2 py-1 text-xs bg-gray-100 dark:bg-gray-700 rounded hover:bg-gray-200 dark:hover:bg-gray-600 disabled:opacity-50"
                                        >
                                            <span className="inline-flex items-center gap-1"><FolderUp className="w-3 h-3" /> {t('aerocryptNative.up')}</span>
                                        </button>
                                    </div>
                                </div>

                                <div className="text-xs text-gray-600 dark:text-gray-400 break-all">
                                    {t('aerocryptNative.path', { path: browserPath })}
                                </div>

                                <div className="max-h-56 overflow-y-auto border border-gray-200 dark:border-gray-700 rounded">
                                    {browserLoading ? (
                                        <div className="p-3 text-sm text-gray-600 dark:text-gray-400 flex items-center gap-2">
                                            <Loader2 className="w-4 h-4 animate-spin" /> {t('aerocryptNative.loading')}
                                        </div>
                                    ) : browserFiles.length === 0 ? (
                                        <div className="p-3 text-sm text-gray-600 dark:text-gray-400">{t('aerocryptNative.emptyFolder')}</div>
                                    ) : (
                                        browserFiles.map((entry) => (
                                            <div key={entry.path} className="flex items-center gap-2 px-2 py-1.5 border-b border-gray-100 dark:border-gray-800 last:border-b-0">
                                                {entry.is_dir ? <Folder className="w-4 h-4 text-blue-500" /> : <FileText className="w-4 h-4 text-gray-400" />}
                                                <button
                                                    onClick={() => entry.is_dir ? void loadBrowser(entry.path) : void handleDownloadRemoteFile(entry)}
                                                    className="flex-1 min-w-0 text-left text-sm text-gray-900 dark:text-white hover:underline"
                                                    title={entry.name}
                                                >
                                                    <span className="truncate block">{entry.decrypted_name || entry.name}</span>
                                                    {!entry.decrypt_ok && (
                                                        <span className="text-[11px] text-amber-600 dark:text-amber-300">{t('aerocryptNative.nameNotDecrypted')}</span>
                                                    )}
                                                </button>
                                                {!entry.is_dir && (
                                                    <button
                                                        onClick={() => void handleDownloadRemoteFile(entry)}
                                                        className="px-2 py-1 text-xs bg-gray-100 dark:bg-gray-700 rounded hover:bg-gray-200 dark:hover:bg-gray-600"
                                                    >
                                                        {t('aerocryptNative.download')}
                                                    </button>
                                                )}
                                            </div>
                                        ))
                                    )}
                                </div>
                            </div>

                            <button
                                onClick={handleLock}
                                className="w-full flex items-center justify-center gap-2 px-4 py-2 border border-red-300 dark:border-red-700 text-red-600 dark:text-red-400 rounded hover:bg-red-50 dark:hover:bg-red-900/30"
                            >
                                <Lock className="w-4 h-4" />
                                {t('aerocryptNative.lock')}
                            </button>
                        </>
                    )}
                </div>
            </div>
        </div>
    );
};
