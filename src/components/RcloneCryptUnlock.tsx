// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Lock, Unlock, Loader2, X, Download, FileText } from 'lucide-react';
import { useTranslation } from '../i18n';
import type { CryptSecretForm } from '../types';
import { CryptSecretFormChoice } from './CryptSecretFormChoice';
import { pickFile, pickSave } from '../utils/pickPath';
import { PasswordInput } from './common/PasswordInput';
import { PasswordMatchHint } from './common/PasswordMatchHint';
import { InlinePasswordGenerator } from './common/InlinePasswordGenerator';
import { PasswordStrengthBar } from './vault/PasswordStrengthBar';

interface RcloneCryptUnlockProps {
    onClose: () => void;
    /**
     * Apply the overlay. Resolves once it is open and rejects with the reason it
     * was refused, which the dialog shows where the secrets were typed.
     */
    onUnlocked?: (details: {
        password: string;
        salt?: string | null;
        passwordForm: CryptSecretForm;
        saltForm: CryptSecretForm;
        filenameEncryption: string;
        directoryNameEncryption: boolean;
        /** The folder to open: the dialog's own, or the one its Create just made. */
        scope: string;
    }) => Promise<void>;
    onLocked?: () => void;
    activeVaultId?: string | null;
    /** Absolute folder the overlay is opened at, `/` for the remote root. */
    scope: string;
    /** Opened for a saved profile's binding: it decides the folder and the name encryption. */
    bound?: boolean;
}

interface RcloneCryptVaultInfo {
    vault_id: string;
    filename_encryption: string;
    directory_name_encryption: boolean;
}

interface RcloneCryptCreatedVault extends RcloneCryptVaultInfo {
    root: string;
}

export const RcloneCryptUnlock: React.FC<RcloneCryptUnlockProps> = ({ onClose, onUnlocked, onLocked, activeVaultId, scope, bound = false }) => {
    const t = useTranslation();
    const [mode, setMode] = useState<'open' | 'create'>('open');
    // The folder the overlay is applied at: the dialog's, then the one a Create
    // made under it.
    const [vaultRoot, setVaultRoot] = useState(scope);
    // The standalone copy of the keys behind the Decrypt name / file tools. The
    // overlay App applies holds its own copy in the provider; this one is
    // released when the dialog locks or closes, so it never outlives the tools.
    const [toolVaultId, setToolVaultId] = useState<string | null>(null);
    const toolVaultIdRef = useRef<string | null>(null);
    const [password, setPassword] = useState('');
    const [confirmPassword, setConfirmPassword] = useState('');
    const [salt, setSalt] = useState('');
    // How the typed values are written: as typed unless the user says they
    // were pasted from rclone.conf. Only asked when opening an existing remote.
    const [passwordForm, setPasswordForm] = useState<CryptSecretForm>('clear');
    const [saltForm, setSaltForm] = useState<CryptSecretForm>('clear');
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

    const releaseToolVault = useCallback(async () => {
        const id = toolVaultIdRef.current;
        toolVaultIdRef.current = null;
        setToolVaultId(null);
        if (id) await lockVault(id).catch(() => undefined);
    }, [lockVault]);

    // Closing the dialog drops the tools' keys with it.
    useEffect(() => () => { void releaseToolVault(); }, [releaseToolVault]);

    // Apply the overlay with the keys `standalone` holds for the tools (null when
    // there are none). They are kept only if the overlay really opened.
    const applyOverlay = async (root: string, standalone: RcloneCryptVaultInfo | null, forms: {
        passwordForm: CryptSecretForm;
        saltForm: CryptSecretForm;
    }) => {
        try {
            await onUnlocked?.({
                password,
                salt: salt || null,
                ...forms,
                filenameEncryption,
                directoryNameEncryption: dirNameEncryption,
                scope: root,
            });
        } catch (e) {
            if (standalone) await lockVault(standalone.vault_id).catch(() => undefined);
            throw e;
        }
        if (standalone) {
            await releaseToolVault();
            toolVaultIdRef.current = standalone.vault_id;
            setToolVaultId(standalone.vault_id);
        }
        setVaultInfo(standalone ?? {
            vault_id: activeVaultId || 'provider',
            filename_encryption: filenameEncryption,
            directory_name_encryption: dirNameEncryption,
        });
    };

    const handleUnlock = async () => {
        if (!password) return;
        setLoading(true);
        setError(null);
        try {
            // A bound dialog's name encryption is the binding's, not these
            // controls', so it gets no tools: keys built from the controls would
            // decrypt names differently from the panel.
            const standalone = bound ? null : await invoke<RcloneCryptVaultInfo>('rclone_crypt_unlock', {
                password,
                salt: salt || null,
                filenameEncryption,
                directoryNameEncryption: dirNameEncryption,
                passwordForm,
                saltForm,
            });
            await applyOverlay(vaultRoot, standalone, { passwordForm, saltForm });
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
            const created = await invoke<RcloneCryptCreatedVault>('rclone_crypt_provider_create_remote', {
                password,
                salt: salt || null,
                filenameEncryption,
                directoryNameEncryption: dirNameEncryption,
                basePath: vaultRoot,
                targetSubpath: createSubpath.trim() ? createSubpath.trim() : null,
                passwordForm: 'clear',
                saltForm: 'clear',
            });
            // From here on the dialog is about the new remote: if the apply
            // fails, Open retries it in its folder.
            setVaultRoot(created.root);
            setMode('open');
            // A remote created here is keyed from what was typed.
            await applyOverlay(created.root, created, { passwordForm: 'clear', saltForm: 'clear' });
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
        // The overlay itself is cleared by onLocked; this drops the tools' copy.
        await releaseToolVault();
        clearSensitiveState();
        onLocked?.();
    };

    const handleDecryptName = async () => {
        if (!toolVaultId || !testEncName || !testDirIv) return;
        setError(null);
        try {
            const name = await invoke<string>('rclone_crypt_decrypt_name', {
                vaultId: toolVaultId,
                dirIvBase64: testDirIv,
                encryptedName: testEncName,
            });
            setTestDecName(name);
        } catch (e) {
            setError(String(e));
        }
    };

    const handleDecryptFile = async () => {
        if (!toolVaultId) return;
        setError(null);

        const inputPath = await pickFile({ multiple: false });
        if (!inputPath || Array.isArray(inputPath)) return;

        const outputPath = await pickSave({ defaultPath: 'decrypted_file' });
        if (!outputPath) return;

        setLoading(true);
        try {
            await invoke<string>('rclone_crypt_decrypt_file_path', {
                vaultId: toolVaultId,
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

                    <p className="text-xs text-gray-500 dark:text-gray-400 break-all">
                        {t('aerocrypt.overlayFolder')}{' '}
                        <code className="font-mono text-gray-700 dark:text-gray-200">{vaultRoot}</code>
                    </p>

                    {!vaultInfo ? (
                        <>
                            {!bound && (
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
                            )}

                            <div>
                                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                    {t('aerocrypt.password')}
                                </label>
                                <div className="relative">
                                    <PasswordInput
                                        value={password}
                                        onChange={setPassword}
                                        onKeyDown={(e) => e.key === 'Enter' && (mode === 'open' ? handleUnlock() : handleCreate())}
                                        placeholder={t('aerocrypt.passwordPlaceholder')}
                                        ariaLabel={t('aerocrypt.password')}
                                        className={mode === 'create' ? 'w-full px-3 py-2 pr-20 border border-gray-300 dark:border-gray-600 rounded bg-white dark:bg-gray-700 text-gray-900 dark:text-white' : undefined}
                                        autoFocus
                                    />
                                    {mode === 'create' && <InlinePasswordGenerator onGenerated={value => { setPassword(value); setConfirmPassword(value); }} className="absolute right-9 top-1/2 -translate-y-1/2" />}
                                </div>
                                {mode === 'create' && password.length > 0 && (
                                    <div className="mt-2">
                                        <PasswordStrengthBar password={password} />
                                    </div>
                                )}
                                {mode === 'open' && (
                                    <CryptSecretFormChoice legend={t('aerocrypt.password')} value={passwordForm} onChange={setPasswordForm} />
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
                                {mode === 'open' && (
                                    <CryptSecretFormChoice legend={t('aerocrypt.salt')} value={saltForm} onChange={setSaltForm} />
                                )}
                            </div>

                            {!bound && (<>
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
                            </>)}

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

                            {toolVaultId && (<>
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
                            </>)}

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
