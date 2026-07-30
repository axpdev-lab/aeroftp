// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Native AeroCrypt overlay modal (P2b). Mirrors RcloneCryptUnlock.tsx but drives
// the native aerocrypt_provider_* backend on our own audited codec. It is
// deliberately simpler than the rclone modal: the native overlay always encrypts
// names with AES-256-SIV (no filename-encryption / dir-name / dirIV options), and
// the salt is generated automatically and stored in the remote marker
// (.aerocrypt.tsv for new vaults, legacy .aeroftp-crypt.json still readable), so
// there is no salt field. Opening an existing overlay reads that config from the
// provider's current directory first (aerocrypt_provider_read_config).
//
// i18n note: this modal uses the dedicated `aerocryptNative.*` namespace. The
// rclone modal still owns `aerocrypt.*`; the coordinated split/rename is P6.

import * as React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { pickFile, pickSave } from '../utils/pickPath';
import { writeTextFile } from '@tauri-apps/plugin-fs';
import { Lock, Unlock, Loader2, X, FileKey } from 'lucide-react';
import { useTranslation } from '../i18n';
import { DefaultSaltDisclosure } from './common/DefaultSaltDisclosure';
import { PasswordInput } from './common/PasswordInput';
import { PasswordMatchHint } from './common/PasswordMatchHint';
import { InlinePasswordGenerator } from './common/InlinePasswordGenerator';
import { PasswordStrengthBar } from './vault/PasswordStrengthBar';
import { QRCodeSVG } from 'qrcode.react';

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
    /** Saved profile id (when connected via a crypt-bound profile). Enables offline kit + keystore backfill. */
    profileId?: string | null;
    /** Remote overlay scope for marker probe/migrate (profile remoteScope / initialPath). */
    remoteScope?: string | null;
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

interface AeroCryptMarkerStatus {
    hasCurrentMarker: boolean;
    hasLegacyMarker: boolean;
}

interface AeroCryptMarkerMigrationResult {
    changed: boolean;
    legacyDeleted: boolean;
    warning?: string | null;
}

export const AeroCryptUnlock: React.FC<AeroCryptUnlockProps> = ({
    onClose,
    onUnlocked,
    onLocked,
    activeVaultId,
    profileId,
    remoteScope,
}) => {
    const t = useTranslation();
    const [mode, setMode] = useState<'open' | 'create'>('open');
    const [password, setPassword] = useState('');
    const [confirmPassword, setConfirmPassword] = useState('');
    const [keyfilePath, setKeyfilePath] = useState('');
    const [createSubpath, setCreateSubpath] = useState('');
    const [aeroCryptDefaultSalt, setAeroCryptDefaultSalt] = useState(false);

    // Entropy gate (D1-D3, mirrors ConnectionScreen)
    const pwLen = password.length;
    const strengthLevel = React.useMemo(() => {
        if (!password) return 0;
        let sc = Math.min(pwLen * 4, 40);
        const variety = [/[a-z]/.test(password), /[A-Z]/.test(password), /[0-9]/.test(password), /[^a-zA-Z0-9]/.test(password)].filter(Boolean).length;
        sc += variety * 10;
        if (variety >= 3 && pwLen >= 12) sc += 10;
        if (variety >= 4 && pwLen >= 16) sc += 10;
        sc = Math.max(0, Math.min(100, sc));
        return sc < 20 ? 0 : sc < 40 ? 1 : sc < 60 ? 2 : sc < 80 ? 3 : 4;
    }, [password, pwLen]);
    // Single password floor, matching the backend gate. The 128/256 radios never
    // reached the backend and the attestation checkbox silently downgraded the
    // mode when left unticked, so both are gone (Ehud #369, #276).
    const requiredLen = 20;
    const meetsEntropy = strengthLevel === 4 && pwLen >= requiredLen;
    // The toggle IS the intent, with no second gate that can quietly overrule it.
    // ANDing entropy in here would rebuild the very bug this removes: the toggle
    // is only DISABLED when the password weakens, never unchecked, so ticking it
    // on a strong password and then weakening it would leave it visibly on while
    // the vault got a per-vault salt. Mismatch blocks the create instead (#276).
    const effectiveUseDefaultSalt = aeroCryptDefaultSalt;
    const canToggleDefaultSalt = meetsEntropy;
    const defaultSaltEntropyMismatch = aeroCryptDefaultSalt && !meetsEntropy;

    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [vaultInfo, setVaultInfo] = useState<AeroCryptVaultInfo | null>(null);
    const [success, setSuccess] = useState<string | null>(null);
    const [kitData, setKitData] = useState<AerocryptEmergencyKit | null>(null);
    const [kitQrLevel, setKitQrLevel] = useState<'L' | 'M' | 'Q' | 'H'>('H');
    const [markerStatus, setMarkerStatus] = useState<AeroCryptMarkerStatus | null>(null);
    const [markerMigrating, setMarkerMigrating] = useState(false);
    const vaultInfoRef = useRef<AeroCryptVaultInfo | null>(null);
    // True when the provider overlay is already live (auto-unlock / badge) so the
    // modal shows the unlocked panel without inventing a fake AECR v2 stub.
    const sessionUnlocked = !!(activeVaultId || vaultInfo);

    useEffect(() => {
        vaultInfoRef.current = vaultInfo;
    }, [vaultInfo]);

    const refreshMarkerStatus = useCallback(async () => {
        try {
            const status = await invoke<AeroCryptMarkerStatus>('aerocrypt_provider_marker_status', {
                basePath: remoteScope || null,
            });
            setMarkerStatus(status);
            return status;
        } catch {
            setMarkerStatus(null);
            return null;
        }
    }, [remoteScope]);

    // Hydrate the unlocked panel when the provider overlay is already live.
    // Never invent version:2 with empty config_json — that made Recovery kit fail
    // and hid the Convert marker action (legacy JSON still on the remote).
    useEffect(() => {
        if (!activeVaultId) return;
        let cancelled = false;
        (async () => {
            try {
                const status = await refreshMarkerStatus();
                let configJson =
                    (await invoke<string | null>('aerocrypt_provider_read_config', {
                        basePath: remoteScope || null,
                    }).catch(() => null)) || '';
                // Prefer keystore kit-ready blob when the remote is still pre-Tier-1.
                if (profileId) {
                    try {
                        const kit = await invoke<AerocryptEmergencyKit>('aerocrypt_profile_recovery_kit', {
                            profileId,
                        });
                        if (!cancelled && kit?.text) {
                            setKitData(kit);
                            setVaultInfo({
                                vault_id: kit.vault_id || 'provider',
                                version: kit.version || 3,
                                config_json: configJson,
                            });
                            return;
                        }
                    } catch {
                        // fall through to config-only hydration
                    }
                }
                if (cancelled) return;
                // Heuristic version: legacy-only remote is still AECR content v3 but
                // pre-Tier-1 marker; show v3 when we have config, else keep 3 as default.
                const version = configJson ? 3 : 3;
                setVaultInfo({
                    vault_id: 'provider',
                    version,
                    config_json: configJson,
                });
                if (status?.hasLegacyMarker && !status?.hasCurrentMarker) {
                    setSuccess(null);
                    setError(null);
                }
            } catch {
                if (!cancelled) {
                    setVaultInfo({ vault_id: 'provider', version: 3, config_json: '' });
                }
            }
        })();
        return () => {
            cancelled = true;
        };
    }, [activeVaultId, profileId, remoteScope, refreshMarkerStatus]);

    useEffect(() => {
        void refreshMarkerStatus();
    }, [refreshMarkerStatus, mode, sessionUnlocked]);

    const clearSensitiveState = useCallback(() => {
        setVaultInfo(null);
        setPassword('');
        setConfirmPassword('');
        setSuccess(null);
        setKitData(null);
        setAeroCryptDefaultSalt(false);
        setMarkerStatus(null);
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
            // The native overlay's salt lives in the remote marker: read it from
            // the current directory before deriving the key.
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
        // #276: never create the vault with default-salt intent the backend would
        // reject for entropy. Refusing is the point: the alternative is a vault
        // that silently got a per-vault salt while the toggle read as on.
        if (defaultSaltEntropyMismatch) return;
        setLoading(true);
        setError(null);
        try {
            const info = await invoke<AeroCryptVaultInfo>('aerocrypt_provider_create_remote', {
                password,
                targetSubpath: createSubpath.trim() ? createSubpath.trim() : null,
                keyfilePath: keyfilePath || null,
                useDefaultSalt: effectiveUseDefaultSalt || null,
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

    const handleMigrateLegacyMarker = async () => {
        // When the provider overlay is already unlocked (auto-unlock path), the
        // password may be empty here: re-use the stored profile password if any.
        let pw = password;
        let kf = keyfilePath;
        if ((!pw && !kf) && profileId) {
            try {
                pw = await invoke<string>('get_credential', {
                    account: `aerocrypt_overlay_pw_${profileId}`,
                }).catch(() => '');
                kf = await invoke<string>('get_credential', {
                    account: `aerocrypt_overlay_keyfile_path_${profileId}`,
                }).catch(() => '');
            } catch {
                /* leave empty */
            }
        }
        if (!pw && !kf) {
            setError(t('aerocryptNative.migrateNeedsFactors'));
            return;
        }
        setMarkerMigrating(true);
        setError(null);
        setSuccess(null);
        try {
            const result = await invoke<AeroCryptMarkerMigrationResult>('aerocrypt_provider_migrate_legacy_marker', {
                password: pw || '',
                keyfilePath: kf || null,
                basePath: remoteScope || null,
            });
            await refreshMarkerStatus();
            if (result.warning) {
                setError(result.warning);
            } else if (result.changed || result.legacyDeleted) {
                setSuccess(t('aerocryptNative.legacyMarkerMigrated'));
                // Publish the new TSV into the keystore so Recovery kit works
                // offline immediately (pre-Tier-1 vaults lacked vault_id).
                try {
                    const configJson =
                        (await invoke<string | null>('aerocrypt_provider_read_config', {
                            basePath: remoteScope || null,
                        })) || '';
                    if (configJson && profileId) {
                        // Parse salt from TSV/JSON for salt-of-record.
                        let saltB64 = '';
                        const saltLine = configJson.split('\n').find((l) => l.startsWith('salt\t'));
                        if (saltLine) saltB64 = saltLine.split('\t')[1]?.trim() || '';
                        if (!saltB64) {
                            try {
                                const j = JSON.parse(configJson);
                                saltB64 = j.salt || '';
                            } catch {
                                /* ignore */
                            }
                        }
                        await invoke('store_credential', {
                            account: `aerocrypt_overlay_config_${profileId}`,
                            password: configJson,
                        });
                        if (saltB64) {
                            await invoke('store_credential', {
                                account: `aerocrypt_overlay_salt_${profileId}`,
                                password: saltB64,
                            });
                        }
                        const kit = await invoke<AerocryptEmergencyKit>('aerocrypt_profile_recovery_kit', {
                            profileId,
                        });
                        setKitData(kit);
                        setVaultInfo({
                            vault_id: kit.vault_id || 'provider',
                            version: kit.version || 3,
                            config_json: configJson,
                        });
                    }
                } catch {
                    /* kit may warm on next connect */
                }
            } else {
                setSuccess(t('aerocryptNative.legacyMarkerAlreadyCurrent'));
            }
        } catch (e) {
            setError(String(e));
        } finally {
            setMarkerMigrating(false);
        }
    };

    // On-demand recovery kit: prefer the keystore kit (works offline after one
    // connect, includes vault_id backfill). Fall back to remote marker config.
    const showRecoveryKit = async () => {
        setError(null);
        try {
            if (profileId) {
                try {
                    const kit = await invoke<AerocryptEmergencyKit>('aerocrypt_profile_recovery_kit', {
                        profileId,
                    });
                    setKitData(kit);
                    return;
                } catch {
                    /* try live remote config */
                }
            }
            let configJson = vaultInfo?.config_json || '';
            if (!configJson) {
                configJson =
                    (await invoke<string | null>('aerocrypt_provider_read_config', {
                        basePath: remoteScope || null,
                    })) || '';
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
            const msg = String(e);
            if (/vault_id/i.test(msg)) {
                setError(t('aerocryptNative.kitNeedsVaultIdUpgrade'));
            } else {
                setError(msg);
            }
        }
    };

    const saveKitToFile = async () => {
        if (!kitData) return;
        try {
            const slug = (profileId || 'profile')
                .replace(/[\\/:*?"<>|]+/g, '')
                .replace(/\s+/g, '-')
                .replace(/-+/g, '-')
                .replace(/^-|-$/g, '')
                .slice(0, 80) || 'profile';
            const path = await pickSave({
                defaultPath: `aerocrypt-recovery-kit-${slug}.txt`,
                filters: [{ name: 'Text', extensions: ['txt'] }],
            });
            if (!path) return;
            await writeTextFile(path, kitData.text);
            setSuccess(t('aerocryptNative.kitSaved', { path }));
        } catch (e) {
            setError(String(e));
        }
    };

    const printKit = () => {
        if (!kitData) return;
        try {
            const qrNote = `\n[${t('aerocryptNative.qrPrintNote', { level: kitQrLevel })}]\n`;
            const html =
                '<!DOCTYPE html><html><head><meta charset="utf-8"><title>AeroCrypt Recovery Kit</title>' +
                '<style>body{font-family:ui-monospace,monospace;white-space:pre-wrap;padding:16px;font-size:12px}</style>' +
                '</head><body>' +
                (kitData.text + qrNote).replace(/&/g, '&amp;').replace(/</g, '&lt;') +
                '</body></html>';
            // WebKitGTK blocks window.open from the app webview; iframe print works.
            const iframe = document.createElement('iframe');
            iframe.setAttribute('aria-hidden', 'true');
            iframe.style.cssText =
                'position:fixed;right:0;bottom:0;width:0;height:0;border:0;opacity:0;pointer-events:none';
            document.body.appendChild(iframe);
            const doc = iframe.contentDocument || iframe.contentWindow?.document;
            if (!doc) {
                document.body.removeChild(iframe);
                setError(t('aerocryptNative.kitPrintFailed'));
                return;
            }
            doc.open();
            doc.write(html);
            doc.close();
            const win = iframe.contentWindow;
            if (!win) {
                document.body.removeChild(iframe);
                setError(t('aerocryptNative.kitPrintFailed'));
                return;
            }
            const cleanup = () => {
                try {
                    document.body.removeChild(iframe);
                } catch {
                    /* already gone */
                }
            };
            win.onafterprint = cleanup;
            setTimeout(cleanup, 60_000);
            win.focus();
            win.print();
        } catch (e) {
            setError(String(e));
        }
    };

    const chooseKeyfile = async () => {
        try {
            const picked = await pickFile({ multiple: false });
            if (typeof picked === 'string' && picked) setKeyfilePath(picked);
        } catch (_) {
            // Dialog cancelled or unavailable: keep the current selection.
        }
    };

    const handleLock = async () => {
        // Provider-path auto-unlock uses a sentinel vault id ("provider-overlay:…")
        // that is not an aerocrypt_unlock session — skip aerocrypt_lock and let
        // onLocked clear the live CryptOverlayProvider instead.
        if (vaultInfo?.vault_id && vaultInfo.vault_id !== 'provider' && !String(vaultInfo.vault_id).startsWith('provider-overlay:')) {
            try {
                await lockVault(vaultInfo.vault_id);
            } catch (_) {
                // Ignore lock errors, local state still needs cleanup.
            }
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
                                <div className="relative">
                                    <PasswordInput
                                        value={password}
                                        onChange={setPassword}
                                        onKeyDown={(e) => e.key === 'Enter' && (mode === 'open' ? handleUnlock() : handleCreate())}
                                        placeholder={t('aerocryptNative.passwordPlaceholder')}
                                        ariaLabel={t('aerocryptNative.password')}
                                        className={mode === 'create' ? 'pr-20' : undefined}
                                        autoFocus
                                    />
                                    {mode === 'create' && <InlinePasswordGenerator onGenerated={value => { setPassword(value); setConfirmPassword(value); }} className="absolute right-9 top-1/2 -translate-y-1/2" />}
                                </div>
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

                            {mode === 'create' && (
                                <div className="space-y-2 border border-gray-200 dark:border-gray-700 rounded p-2 text-xs">
                                    <label className="flex items-center gap-2">
                                        <input
                                            type="checkbox"
                                            checked={aeroCryptDefaultSalt}
                                            onChange={(e) => setAeroCryptDefaultSalt(e.target.checked)}
                                            disabled={!canToggleDefaultSalt}
                                        />
                                        <span>{t('aerocryptNative.defaultSaltLabel')}</span>
                                    </label>
                                    {defaultSaltEntropyMismatch && (
                                        <span className="ml-5 text-xs text-red-600 dark:text-red-400 leading-relaxed">
                                            {t('aerocryptProfile.defaultSaltNeedsStronger')} ({pwLen}/{requiredLen})
                                        </span>
                                    )}
                                    {aeroCryptDefaultSalt && (
                                        <>
                                            {/* Ehud #369: the salt is a single 32-byte public
                                                constant, printed here so it can be read, copied
                                                and backed up. */}
                                            <DefaultSaltDisclosure className="ml-5" />
                                        </>
                                    )}
                                </div>
                            )}

                            {mode === 'open' && markerStatus?.hasLegacyMarker && !markerStatus?.hasCurrentMarker && (
                                <div className="space-y-2 rounded border border-amber-300 dark:border-amber-700 bg-amber-50 dark:bg-amber-900/20 p-3 text-xs">
                                    <p className="text-amber-800 dark:text-amber-200">
                                        {t('aerocryptNative.legacyMarkerNotice')}
                                    </p>
                                    <button
                                        type="button"
                                        onClick={() => void handleMigrateLegacyMarker()}
                                        disabled={loading || markerMigrating}
                                        className="inline-flex items-center gap-2 px-3 py-1.5 rounded bg-amber-600 text-white hover:bg-amber-700 disabled:opacity-50 disabled:cursor-not-allowed"
                                    >
                                        {markerMigrating ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : <FileKey className="w-3.5 h-3.5" />}
                                        {t('aerocryptNative.migrateLegacyMarker')}
                                    </button>
                                    <p className="text-[11px] text-amber-700/80 dark:text-amber-300/80">
                                        {t('aerocryptNative.migrateUsesStoredFactors')}
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
                                    disabled={(!password && !keyfilePath) || password !== confirmPassword || loading || defaultSaltEntropyMismatch}
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
                                    {t('aerocryptNative.remoteUnlocked', {
                                        id: (vaultInfo?.vault_id && vaultInfo.vault_id !== 'provider'
                                            ? vaultInfo.vault_id
                                            : kitData?.vault_id || 'live'
                                        ).slice(0, 8),
                                    })}
                                </span>
                                <span className="ml-auto text-[11px] text-gray-500 dark:text-gray-400">
                                    {t('aerocryptNative.versionLabel', {
                                        version: kitData?.version || vaultInfo?.version || 3,
                                    })}
                                </span>
                            </div>

                            {/* Convert marker also available when already unlocked (auto-unlock path). */}
                            {markerStatus?.hasLegacyMarker && !markerStatus?.hasCurrentMarker && (
                                <div className="space-y-2 rounded border border-amber-300 dark:border-amber-700 bg-amber-50 dark:bg-amber-900/20 p-3 text-xs">
                                    <p className="text-amber-800 dark:text-amber-200">
                                        {t('aerocryptNative.legacyMarkerNotice')}
                                    </p>
                                    <button
                                        type="button"
                                        onClick={() => void handleMigrateLegacyMarker()}
                                        disabled={loading || markerMigrating}
                                        className="inline-flex items-center gap-2 px-3 py-1.5 rounded bg-amber-600 text-white hover:bg-amber-700 disabled:opacity-50 disabled:cursor-not-allowed"
                                    >
                                        {markerMigrating ? (
                                            <Loader2 className="w-3.5 h-3.5 animate-spin" />
                                        ) : (
                                            <FileKey className="w-3.5 h-3.5" />
                                        )}
                                        {t('aerocryptNative.migrateLegacyMarker')}
                                    </button>
                                </div>
                            )}

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

                                    {/* D4: QR code for the recovery kit (public fields only). Level selector defaults to H (max correction). */}
                                    <div className="flex flex-col sm:flex-row gap-3 items-start">
                                        <div>
                                            <div className="text-[10px] uppercase tracking-wide text-gray-500 dark:text-gray-400 mb-1">{t('aerocryptNative.recoveryQr')}</div>
                                            <QRCodeSVG
                                                value={`AEROCRYPT-KIT v1\n${kitData.text}`}
                                                level={kitQrLevel}
                                                size={128}
                                                includeMargin
                                            />
                                        </div>
                                        <div className="text-xs space-y-1">
                                            <div className="font-medium">{t('aerocryptNative.qrErrorCorrection')}</div>
                                            {(['L','M','Q','H'] as const).map(lv => (
                                                <label key={lv} className="flex items-center gap-1.5 cursor-pointer">
                                                    <input
                                                        type="radio"
                                                        name="kitQrLevel"
                                                        checked={kitQrLevel === lv}
                                                        onChange={() => setKitQrLevel(lv)}
                                                    />
                                                    <span>{lv} {lv === 'H' ? t('aerocryptNative.qrRecommended') : ''}</span>
                                                </label>
                                            ))}
                                            <p className="text-[10px] text-gray-500 dark:text-gray-400 mt-1">{t('aerocryptNative.qrHint')}</p>
                                        </div>
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
