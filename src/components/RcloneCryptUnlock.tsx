// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Shield, Lock, Unlock, Eye, EyeOff, Loader2, X, Download, FileText } from 'lucide-react';
import { useTranslation } from '../i18n';
import { open, save } from '@tauri-apps/plugin-dialog';

interface RcloneCryptUnlockProps {
    onClose: () => void;
}

interface RcloneCryptVaultInfo {
    vault_id: string;
    filename_encryption: string;
    directory_name_encryption: boolean;
}

export const RcloneCryptUnlock: React.FC<RcloneCryptUnlockProps> = ({ onClose }) => {
    const t = useTranslation();
    const [password, setPassword] = useState('');
    const [salt, setSalt] = useState('');
    const [showPassword, setShowPassword] = useState(false);
    const [filenameEncryption, setFilenameEncryption] = useState('standard');
    const [dirNameEncryption, setDirNameEncryption] = useState(true);
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

    const clearSensitiveState = useCallback(() => {
        setVaultInfo(null);
        setPassword('');
        setSalt('');
        setSuccess(null);
        setTestDecName(null);
    }, []);

    const lockVault = useCallback(async (vaultId: string) => {
        await invoke('rclone_crypt_lock', { vaultId });
    }, []);

    const closeAndLockIfNeeded = useCallback(async () => {
        const currentVault = vaultInfoRef.current;
        if (currentVault) {
            try {
                await lockVault(currentVault.vault_id);
            } catch (_) {
                // Ignore lock errors during shutdown/close flow.
            }
        }
        clearSensitiveState();
        onClose();
    }, [clearSensitiveState, lockVault, onClose]);

    useEffect(() => {
        return () => {
            const currentVault = vaultInfoRef.current;
            if (currentVault) {
                void lockVault(currentVault.vault_id);
            }
        };
    }, [lockVault]);

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
            setPassword('');
            setSalt('');
            setSuccess('Rclone crypt remote unlocked');
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
            setSuccess(`File decrypted to ${outputPath}`);
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
                        <Shield className="w-5 h-5 text-blue-500" />
                        <h2 className="text-lg font-semibold text-gray-900 dark:text-white">
                            Rclone Crypt
                        </h2>
                    </div>
                    <button onClick={() => void closeAndLockIfNeeded()} className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded">
                        <X className="w-5 h-5 text-gray-500" />
                    </button>
                </div>

                <div className="p-4 space-y-4">
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
                            <div>
                                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                    Password
                                </label>
                                <div className="relative">
                                    <input
                                        type={showPassword ? 'text' : 'password'}
                                        value={password}
                                        onChange={(e) => setPassword(e.target.value)}
                                        onKeyDown={(e) => e.key === 'Enter' && handleUnlock()}
                                        className="w-full px-3 py-2 pr-10 border border-gray-300 dark:border-gray-600 rounded bg-white dark:bg-gray-700 text-gray-900 dark:text-white"
                                        placeholder="Rclone crypt password"
                                        autoFocus
                                    />
                                    <button
                                        onClick={() => setShowPassword(!showPassword)}
                                        className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600"
                                    >
                                        {showPassword ? <EyeOff className="w-4 h-4" /> : <Eye className="w-4 h-4" />}
                                    </button>
                                </div>
                            </div>

                            <div>
                                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                    Salt (password2, optional)
                                </label>
                                <input
                                    type="password"
                                    value={salt}
                                    onChange={(e) => setSalt(e.target.value)}
                                    className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded bg-white dark:bg-gray-700 text-gray-900 dark:text-white"
                                    placeholder="Optional salt password"
                                />
                            </div>

                            <div>
                                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                    Filename encryption
                                </label>
                                <select
                                    value={filenameEncryption}
                                    onChange={(e) => setFilenameEncryption(e.target.value)}
                                    className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded bg-white dark:bg-gray-700 text-gray-900 dark:text-white"
                                >
                                    <option value="standard">Standard (EME)</option>
                                    <option value="off">Off</option>
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
                                    Directory name encryption
                                </label>
                            </div>

                            <button
                                onClick={handleUnlock}
                                disabled={!password || loading}
                                className="w-full flex items-center justify-center gap-2 px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed"
                            >
                                {loading ? <Loader2 className="w-4 h-4 animate-spin" /> : <Unlock className="w-4 h-4" />}
                                Unlock
                            </button>
                        </>
                    ) : (
                        <>
                            <div className="flex items-center gap-2 p-3 bg-green-50 dark:bg-green-900/30 rounded">
                                <Unlock className="w-5 h-5 text-green-600 dark:text-green-400" />
                                <span className="text-sm text-green-700 dark:text-green-300">
                                    Remote unlocked (ID: {vaultInfo.vault_id.slice(0, 8)}...)
                                </span>
                            </div>

                            <div className="border border-gray-200 dark:border-gray-700 rounded p-3 space-y-2">
                                <h3 className="text-sm font-medium text-gray-700 dark:text-gray-300 flex items-center gap-1">
                                    <FileText className="w-4 h-4" />
                                    Decrypt filename
                                </h3>
                                <input
                                    type="text"
                                    value={testDirIv}
                                    onChange={(e) => setTestDirIv(e.target.value)}
                                    className="w-full px-3 py-1.5 text-sm border border-gray-300 dark:border-gray-600 rounded bg-white dark:bg-gray-700 text-gray-900 dark:text-white"
                                    placeholder="dirIV (base64)"
                                />
                                <input
                                    type="text"
                                    value={testEncName}
                                    onChange={(e) => setTestEncName(e.target.value)}
                                    className="w-full px-3 py-1.5 text-sm border border-gray-300 dark:border-gray-600 rounded bg-white dark:bg-gray-700 text-gray-900 dark:text-white"
                                    placeholder="Encrypted filename (Base32hex)"
                                />
                                <button
                                    onClick={handleDecryptName}
                                    disabled={!testDirIv || !testEncName}
                                    className="px-3 py-1.5 text-sm bg-gray-100 dark:bg-gray-700 rounded hover:bg-gray-200 dark:hover:bg-gray-600 disabled:opacity-50"
                                >
                                    Decrypt name
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
                                Decrypt file from disk
                            </button>

                            <button
                                onClick={handleLock}
                                className="w-full flex items-center justify-center gap-2 px-4 py-2 border border-red-300 dark:border-red-700 text-red-600 dark:text-red-400 rounded hover:bg-red-50 dark:hover:bg-red-900/30"
                            >
                                <Lock className="w-4 h-4" />
                                Lock
                            </button>
                        </>
                    )}
                </div>
            </div>
        </div>
    );
};
