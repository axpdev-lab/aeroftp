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
import { open } from '@tauri-apps/plugin-dialog';
import { Lock, Unlock, Loader2, X, FileKey } from 'lucide-react';
import { useTranslation } from '../i18n';
import { PasswordInput } from './common/PasswordInput';
import { PasswordMatchHint } from './common/PasswordMatchHint';
import { PasswordStrengthBar } from './vault/PasswordStrengthBar';

interface AeroCryptUnlockProps {
    onClose: () => void;
    onUnlocked?: (details: {
        vaultId: string;
        password: string;
        remoteScope?: string;
        /** AeroCrypt Tier 1 optional keyfile second factor (local path). */
        keyfilePath?: string;
    }) => void;
    onLocked?: () => void;
    activeVaultId?: string | null;
}

interface AeroCryptVaultInfo {
    vault_id: string;
    version: number;
    config_json: string;
}

interface AerocryptEmergencyKit {
    vault_id: string;
    version: number;
    salt: string;
    kdf_algorithm: string;
    kdf_mem_kib: number;
    kdf_time: number;
    kdf_lanes: number;
    text: string;
}

export const AeroCryptUnlock: React.FC<AeroCryptUnlockProps> = ({ onClose, onUnlocked, onLocked, activeVaultId }) => {
    const t = useTranslation();
    const [mode, setMode] = useState<'open' | 'create'>('open');
    const [password, setPassword] = useState('');
    const [confirmPassword, setConfirmPassword] = useState('');
    const [keyfilePath, setKeyfilePath] = useState('');
    const [createSubpath, setCreateSubpath] = useState('');
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [vaultInfo, setVaultInfo] = useState<AeroCryptVaultInfo | null>(null);
    const [success, setSuccess] = useState<string | null>(null);
    const [kitData, setKitData] = useState<AerocryptEmergencyKit | null>(null);
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
        setKitData(null);
    }, []);

    const lockVault = useCallback(async (vaultId: string) => {
        await invoke('aerocrypt_lock', { vaultId });
    }, []);

    const handleUnlock = async () => {
        // Tier 1: an empty password is legal when a keyfile is the (only) factor.
        if (!password && !keyfilePath) return;
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
                keyfilePath: keyfilePath || null,
            });
            setVaultInfo(info);
            onUnlocked?.({
                vaultId: info.vault_id,
                password,
                remoteScope: '',
                keyfilePath: keyfilePath || undefined,
            });
            setPassword('');
            setSuccess(t('aerocryptNative.unlocked'));
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleCreate = async () => {
        // Tier 1: keyfile-only vaults (empty password) are legal by design.
        if ((!password && !keyfilePath) || password !== confirmPassword) return;
        setLoading(true);
        setError(null);
        try {
            const info = await invoke<AeroCryptVaultInfo>('aerocrypt_provider_create_remote', {
                password,
                targetSubpath: createSubpath.trim() ? createSubpath.trim() : null,
                keyfilePath: keyfilePath || null,
            });
            // Non-blocking recovery model (owner decision, v4.1.4): creating a
            // headerless vault must be as frictionless as rclone-crypt. The vault
            // is usable immediately; the recovery kit is NOT forced here. It stays
            // available on demand via the "Recovery kit" button in the unlocked
            // view (rebuilt from the persisted config, re-viewable and re-savable
            // any time), so the create+connect flow is never interrupted.
            setVaultInfo(info);
            setCreateSubpath('');
            onUnlocked?.({
                vaultId: info.vault_id,
                password,
                remoteScope: '',
                keyfilePath: keyfilePath || undefined,
            });
            setPassword('');
            setConfirmPassword('');
            setSuccess(t('aerocryptNative.initialised'));
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    // On-demand recovery kit: rebuild the public kit from the vault's config at
    // any time (never forced). For a freshly created or opened vault the
    // config_json is already in vaultInfo; for a vault re-entered via
    // activeVaultId we read the live overlay config. Public-only (vault_id, salt,
    // KDF params), never secrets. Re-viewable and re-savable as often as wanted.
    const showRecoveryKit = async () => {
        setError(null);
        try {
            let configJson = vaultInfo?.config_json || '';
            if (!configJson) {
                configJson = (await invoke<string | null>('aerocrypt_provider_read_config', {})) || '';
            }
            if (!configJson) {
                setError(t('aerocryptNative.kitUnavailable'));
                return;
            }
            const kit = await invoke<AerocryptEmergencyKit>('aerocrypt_build_emergency_kit', {
                configJson,
            });
            setKitData(kit);
        } catch (e) {
            setError(String(e));
        }
    };

    const saveKitToFile = () => {
        if (!kitData) return;
        const blob = new Blob([kitData.text], { type: 'text/plain;charset=utf-8' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = 'aerocrypt-emergency-kit.txt';
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
        URL.revokeObjectURL(url);
    };

    const printKit = () => {
        if (!kitData) return;
        const w = window.open('', '_blank');
        if (w) {
            w.document.write('<pre style="font-family: monospace; white-space: pre-wrap;">' + kitData.text.replace(/&/g,'&amp;').replace(/</g,'&lt;') + '</pre>');
            w.document.close();
            w.focus();
            w.print();
        }
    };

    const chooseKeyfile = async () => {
        try {
            const picked = await open({ multiple: false });
            if (typeof picked === 'string' && picked) setKeyfilePath(picked);
        } catch (_) {
            // Dialog cancelled or unavailable: keep the current selection.
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

                            <div>
                                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                    {t('aerocryptNative.keyfileLabel')}
                                </label>
                                <div className="flex gap-2">
                                    <input
                                        type="text"
                                        value={keyfilePath}
                                        readOnly
                                        className="flex-1 px-3 py-2 border border-gray-300 dark:border-gray-600 rounded bg-gray-50 dark:bg-gray-700 text-gray-900 dark:text-white text-sm"
                                        placeholder={t('aerocryptNative.keyfilePlaceholder')}
                                        aria-label={t('aerocryptNative.keyfileLabel')}
                                    />
                                    <button
                                        type="button"
                                        onClick={chooseKeyfile}
                                        className="flex items-center gap-1 px-3 py-2 rounded text-sm font-medium bg-gray-200 dark:bg-gray-700 text-gray-700 dark:text-gray-200 hover:bg-gray-300 dark:hover:bg-gray-600"
                                    >
                                        <FileKey className="w-4 h-4" />
                                        {t('aerocryptNative.keyfileChoose')}
                                    </button>
                                    {keyfilePath && (
                                        <button
                                            type="button"
                                            onClick={() => setKeyfilePath('')}
                                            className="px-2 py-2 rounded text-sm bg-gray-200 dark:bg-gray-700 text-gray-500 dark:text-gray-400 hover:bg-gray-300 dark:hover:bg-gray-600"
                                            aria-label={t('aerocryptNative.keyfileClear')}
                                            title={t('aerocryptNative.keyfileClear')}
                                        >
                                            <X className="w-4 h-4" />
                                        </button>
                                    )}
                                </div>
                                <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">
                                    {t('aerocryptNative.keyfileHint')}
                                </p>
                            </div>

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
                                    disabled={(!password && !keyfilePath) || loading}
                                    className="w-full flex items-center justify-center gap-2 px-4 py-2 bg-emerald-600 text-white rounded hover:bg-emerald-700 disabled:opacity-50 disabled:cursor-not-allowed"
                                >
                                    {loading ? <Loader2 className="w-4 h-4 animate-spin" /> : <Unlock className="w-4 h-4" />}
                                    {t('aerocryptNative.unlock')}
                                </button>
                            ) : (
                                <button
                                    onClick={handleCreate}
                                    disabled={(!password && !keyfilePath) || password !== confirmPassword || loading}
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
                                    {t('aerocryptNative.remoteUnlocked', { id: (vaultInfo?.vault_id || '').slice(0, 8) })}
                                </span>
                                <span className="ml-auto text-[11px] text-gray-500 dark:text-gray-400">
                                    {t('aerocryptNative.versionLabel', { version: vaultInfo ? vaultInfo.version : (kitData?.version || 3) })}
                                </span>
                            </div>

                            {/* Recovery kit is OPTIONAL and non-blocking: shown only on
                                demand, re-viewable and re-savable any time, never gating use. */}
                            {kitData ? (
                                <div className="space-y-2">
                                    <div className="p-3 bg-amber-50 dark:bg-amber-900/30 border border-amber-300 dark:border-amber-700 rounded text-sm">
                                        <div className="font-semibold text-amber-700 dark:text-amber-300 mb-1">{t('aerocryptNative.recoveryKitTitle')}</div>
                                        <p className="text-gray-700 dark:text-gray-200 text-xs">
                                            {t('aerocryptNative.recoveryKitIntro')}
                                        </p>
                                    </div>
                                    <div className="bg-gray-50 dark:bg-gray-900 rounded p-2 text-xs font-mono whitespace-pre-wrap break-all max-h-48 overflow-auto">
                                        {kitData.text}
                                    </div>
                                    <div className="flex gap-2">
                                        <button onClick={saveKitToFile} className="flex-1 px-3 py-1.5 text-sm rounded bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600">{t('aerocryptNative.saveKit')}</button>
                                        <button onClick={printKit} className="flex-1 px-3 py-1.5 text-sm rounded bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600">{t('aerocryptNative.printKit')}</button>
                                        <button onClick={() => setKitData(null)} className="px-3 py-1.5 text-sm rounded bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600">{t('aerocryptNative.hideKit')}</button>
                                    </div>
                                </div>
                            ) : (
                                <button
                                    onClick={showRecoveryKit}
                                    className="w-full flex items-center justify-center gap-2 px-4 py-2 border border-amber-300 dark:border-amber-700 text-amber-700 dark:text-amber-300 rounded hover:bg-amber-50 dark:hover:bg-amber-900/30"
                                >
                                    <FileKey className="w-4 h-4" />
                                    {t('aerocryptNative.showKit')}
                                </button>
                            )}

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
