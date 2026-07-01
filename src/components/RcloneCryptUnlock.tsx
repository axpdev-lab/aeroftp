// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Lock, Unlock, Loader2, X, Download, FileText } from 'lucide-react';
import { useTranslation } from '../i18n';
import { open, save } from '@tauri-apps/plugin-dialog';
import { PasswordInput } from './common/PasswordInput';
import { PasswordMatchHint } from './common/PasswordMatchHint';
import { PasswordStrengthBar } from './vault/PasswordStrengthBar';

interface RcloneCryptUnlockProps {
    onClose: () => void;
    onUnlocked?: (details: {
        vaultId: string;
        password: string;
        salt?: string | null;
        filenameEncryption: string;
        directoryNameEncryption: boolean;
        remoteScope?: string;
    }) => void;
    onLocked?: () => void;
    activeVaultId?: string | null;
}

interface RcloneCryptVaultInfo {
    vault_id: string;
    filename_encryption: string;
    directory_name_encryption: boolean;
}

export const RcloneCryptUnlock: React.FC<RcloneCryptUnlockProps> = ({ onClose, onUnlocked, onLocked, activeVaultId }) => {
    const t = useTranslation();
    const [mode, setMode] = useState<'open' | 'create'>('open');
    const [password, setPassword] = useState('');
    const [confirmPassword, setConfirmPassword] = useState('');
    const [salt, setSalt] = useState('');
    const [filenameEncryption, setFilenameEncryption] = useState('standard');
    const [dirNameEncryption, setDirNameEncryption] = useState(true);
    const [createSubpath, setCreateSubpath] = useState('');
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [vaultInfo, setVaultInfo] = useState<RcloneCryptVaultInfo | null>(null);
    const [success, setSuccess] = useState<string | null>(null);

    const [testDirIv, setTestDirIv] = useState('');
    const [testEncName, setTestEncName] = useState('');
    const [testDecName, setTestDecName] = useState<string | null>(null);
    const vaultInfoRef = useRef<RcloneCryptVaultInfo | null>(null);

    useEffect(() => {
        vaultInfoRef.current = vaultInfo;
    }, [vaultInfo]);

    useEffect(() => {
        if (!activeVaultId || vaultInfoRef.current?.vault_id === activeVaultId) return;
        setVaultInfo({
            vault_id: activeVaultId,
            filename_encryption: filenameEncryption,
            directory_name_encryption: dirNameEncryption,
        });
    }, [activeVaultId, dirNameEncryption, filenameEncryption]);

    const clearSensitiveState = useCallback(() => {
        setVaultInfo(null);
        setPassword('');
        setConfirmPassword('');
        setSalt('');
        setSuccess(null);
        setTestDecName(null);
    }, []);

    const lockVault = useCallback(async (vaultId: string) => {
        await invoke('rclone_crypt_lock', { vaultId });
    }, []);

    const handleUnlock = async () => {
        if (!password) return;
        setLoading(true);
        setError(null);
        try {
            const info = await invoke<RcloneCryptVaultInfo>('rclone_crypt_unlock', {
                password,
                salt: salt || null,
                filenameEncryption,
                directoryNameEncryption: dirNameEncryption,
            });
            setVaultInfo(info);
            onUnlocked?.({
                vaultId: info.vault_id,
                password,
                salt: salt || null,
                filenameEncryption,
                directoryNameEncryption: dirNameEncryption,
                remoteScope: '',
            });
            setPassword('');
            setSalt('');
            setSuccess(t('aerocrypt.unlocked'));
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
            const info = await invoke<RcloneCryptVaultInfo>('rclone_crypt_provider_create_remote', {
                password,
                salt: salt || null,
                filenameEncryption,
                directoryNameEncryption: dirNameEncryption,
                targetSubpath: createSubpath.trim() ? createSubpath.trim() : null,
            });
            setVaultInfo(info);
            onUnlocked?.({
                vaultId: info.vault_id,
                password,
                salt: salt || null,
                filenameEncryption,
                directoryNameEncryption: dirNameEncryption,
                remoteScope: '',
            });
            setPassword('');
            setConfirmPassword('');
            setSalt('');
            setCreateSubpath('');
            setSuccess(t('aerocrypt.initialised'));
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

    const handleDecryptName = async () => {
        if (!vaultInfo || !testEncName || !testDirIv) return;
        setError(null);
        try {
            const name = await invoke<string>('rclone_crypt_decrypt_name', {
                vaultId: vaultInfo.vault_id,
                dirIvBase64: testDirIv,
                encryptedName: testEncName,
            });
            setTestDecName(name);
        } catch (e) {
            setError(String(e));
        }
    };

    const handleDecryptFile = async () => {
        if (!vaultInfo) return;
        setError(null);

        const inputPath = await open({ multiple: false });
        if (!inputPath || Array.isArray(inputPath)) return;

        const outputPath = await save({ defaultPath: 'decrypted_file' });
        if (!outputPath) return;

        setLoading(true);
        try {
            await invoke<string>('rclone_crypt_decrypt_file_path', {
                vaultId: vaultInfo.vault_id,
                encryptedFilePath: inputPath,
                outputPath,
            });
            setSuccess(t('aerocrypt.fileDecryptedTo', { path: outputPath }));
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
                        <Lock size={20} className="text-gray-600 dark:text-gray-300" />
                        <h2 className="text-lg font-semibold text-gray-900 dark:text-white">
                            {t('aerocrypt.title')}
                        </h2>
                    </div>
                    <button onClick={onClose} className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded">
                        <X className="w-5 h-5 text-gray-500" />
                    </button>
                </div>

                <div className="p-4 space-y-4">
                    {!vaultInfo && (
                        <div className="text-xs leading-relaxed p-3 rounded border border-blue-400/30 bg-blue-500/10 text-gray-700 dark:text-gray-200">
                            <div className="font-semibold mb-1 text-blue-500 dark:text-blue-300">{t('aerocrypt.intro.heading')}</div>
                            <p className="mb-1" dangerouslySetInnerHTML={{ __html: t('aerocrypt.intro.p1') }} />
                            <p className="mb-1" dangerouslySetInnerHTML={{ __html: t('aerocrypt.intro.p2') }} />
                            <p dangerouslySetInnerHTML={{ __html: t('aerocrypt.intro.p3') }} />
                        </div>
                    )}
                    {error && (
                        <div className="p-3 bg-red-50 dark:bg-red-900/30 text-red-700 dark:text-red-300 rounded text-sm">
                            {error}
                        </div>
                    )}
                    {success && (
                        <div className="p-3 bg-green-50 dark:bg-green-900/30 text-green-700 dark:text-green-300 rounded text-sm">
                            {success}
                        </div>
                    )}

                    {!vaultInfo ? (
                        <>
                            <div className="flex gap-2">
                                <button
                                    type="button"
                                    onClick={() => { setMode('open'); setError(null); setConfirmPassword(''); }}
                                    className={`flex-1 px-3 py-1.5 rounded text-sm font-medium ${mode === 'open' ? 'bg-blue-600 text-white' : 'bg-gray-200 dark:bg-gray-700 text-gray-700 dark:text-gray-200 hover:bg-gray-300 dark:hover:bg-gray-600'}`}
                                >
                                    {t('aerocrypt.openExisting')}
                                </button>
                                <button
                                    type="button"
                                    onClick={() => { setMode('create'); setError(null); setConfirmPassword(''); }}
                                    className={`flex-1 px-3 py-1.5 rounded text-sm font-medium ${mode === 'create' ? 'bg-blue-600 text-white' : 'bg-gray-200 dark:bg-gray-700 text-gray-700 dark:text-gray-200 hover:bg-gray-300 dark:hover:bg-gray-600'}`}
                                >
                                    {t('aerocrypt.createNew')}
                                </button>
                            </div>

                            <div>
                                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                    {t('aerocrypt.password')}
                                </label>
                                <PasswordInput
                                    value={password}
                                    onChange={setPassword}
                                    onKeyDown={(e) => e.key === 'Enter' && (mode === 'open' ? handleUnlock() : handleCreate())}
                                    placeholder={t('aerocrypt.passwordPlaceholder')}
                                    ariaLabel={t('aerocrypt.password')}
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

                            <div>
                                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                    {t('aerocrypt.salt')}
                                </label>
                                <input
                                    type="password"
                                    value={salt}
                                    onChange={(e) => setSalt(e.target.value)}
                                    className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded bg-white dark:bg-gray-700 text-gray-900 dark:text-white"
                                    placeholder={t('aerocrypt.saltPlaceholder')}
                                />
                            </div>

                            <div>
                                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                    {t('aerocrypt.filenameEncryption')}
                                </label>
                                <select
                                    value={filenameEncryption}
                                    onChange={(e) => setFilenameEncryption(e.target.value)}
                                    className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded bg-white dark:bg-gray-700 text-gray-900 dark:text-white"
                                >
                                    <option value="standard">{t('aerocrypt.filenameEncOption.standard')}</option>
                                    <option value="obfuscate">{t('aerocrypt.filenameEncOption.obfuscate')}</option>
                                    <option value="off">{t('aerocrypt.filenameEncOption.off')}</option>
                                </select>
                            </div>

                            <div className="flex items-center gap-2">
                                <input
                                    type="checkbox"
                                    checked={dirNameEncryption}
                                    onChange={(e) => setDirNameEncryption(e.target.checked)}
                                    id="dir-name-enc"
                                    className="rounded"
                                />
                                <label htmlFor="dir-name-enc" className="text-sm text-gray-700 dark:text-gray-300">
                                    {t('aerocrypt.directoryNameEncryption')}
                                </label>
                            </div>

                            {mode === 'create' && (
                                <div>
                                    <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                        {t('aerocrypt.targetSubpath')}
                                    </label>
                                    <input
                                        type="text"
                                        value={createSubpath}
                                        onChange={(e) => setCreateSubpath(e.target.value)}
                                        className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded bg-white dark:bg-gray-700 text-gray-900 dark:text-white"
                                        placeholder={t('aerocrypt.targetSubpathPlaceholder')}
                                    />
                                    <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">
                                        {t('aerocrypt.targetSubpathHint')}
                                    </p>
                                </div>
                            )}

                            {mode === 'open' ? (
                                <button
                                    onClick={handleUnlock}
                                    disabled={!password || loading}
                                    className="w-full flex items-center justify-center gap-2 px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed"
                                >
                                    {loading ? <Loader2 className="w-4 h-4 animate-spin" /> : <Unlock className="w-4 h-4" />}
                                    {t('aerocrypt.unlock')}
                                </button>
                            ) : (
                                <button
                                    onClick={handleCreate}
                                    disabled={!password || password !== confirmPassword || loading}
                                    className="w-full flex items-center justify-center gap-2 px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed"
                                >
                                    {loading ? <Loader2 className="w-4 h-4 animate-spin" /> : <Lock className="w-4 h-4" />}
                                    {t('aerocrypt.createAndUnlock')}
                                </button>
                            )}
                        </>
                    ) : (
                        <>
                            <div className="flex items-center gap-2 p-3 bg-green-50 dark:bg-green-900/30 rounded">
                                <Unlock className="w-5 h-5 text-green-600 dark:text-green-400" />
                                <span className="text-sm text-green-700 dark:text-green-300">
                                    {t('aerocrypt.remoteUnlocked', { id: vaultInfo.vault_id.slice(0, 8) })}
                                </span>
                            </div>

                            <div className="border border-gray-200 dark:border-gray-700 rounded p-3 space-y-2">
                                <h3 className="text-sm font-medium text-gray-700 dark:text-gray-300 flex items-center gap-1">
                                    <FileText className="w-4 h-4" />
                                    {t('aerocrypt.decryptFilename')}
                                </h3>
                                <input
                                    type="text"
                                    value={testDirIv}
                                    onChange={(e) => setTestDirIv(e.target.value)}
                                    className="w-full px-3 py-1.5 text-sm border border-gray-300 dark:border-gray-600 rounded bg-white dark:bg-gray-700 text-gray-900 dark:text-white"
                                    placeholder={t('aerocrypt.dirIvPlaceholder')}
                                />
                                <input
                                    type="text"
                                    value={testEncName}
                                    onChange={(e) => setTestEncName(e.target.value)}
                                    className="w-full px-3 py-1.5 text-sm border border-gray-300 dark:border-gray-600 rounded bg-white dark:bg-gray-700 text-gray-900 dark:text-white"
                                    placeholder={t('aerocrypt.encryptedNamePlaceholder')}
                                />
                                <button
                                    onClick={handleDecryptName}
                                    disabled={!testDirIv || !testEncName}
                                    className="px-3 py-1.5 text-sm bg-gray-100 dark:bg-gray-700 rounded hover:bg-gray-200 dark:hover:bg-gray-600 disabled:opacity-50"
                                >
                                    {t('aerocrypt.decryptName')}
                                </button>
                                {testDecName && (
                                    <div className="text-sm text-green-600 dark:text-green-400 font-mono">
                                        {testDecName}
                                    </div>
                                )}
                            </div>

                            <button
                                onClick={handleDecryptFile}
                                disabled={loading}
                                className="w-full flex items-center justify-center gap-2 px-4 py-2 bg-gray-100 dark:bg-gray-700 rounded hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-900 dark:text-white"
                            >
                                {loading ? <Loader2 className="w-4 h-4 animate-spin" /> : <Download className="w-4 h-4" />}
                                {t('aerocrypt.decryptFileFromDisk')}
                            </button>

                            <button
                                onClick={handleLock}
                                className="w-full flex items-center justify-center gap-2 px-4 py-2 border border-red-300 dark:border-red-700 text-red-600 dark:text-red-400 rounded hover:bg-red-50 dark:hover:bg-red-900/30"
                            >
                                <Lock className="w-4 h-4" />
                                {t('aerocrypt.lock')}
                            </button>
                        </>
                    )}
                </div>
            </div>
        </div>
    );
};
