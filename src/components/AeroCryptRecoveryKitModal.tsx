// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// On-demand Recovery Kit viewer for a saved headerless AeroCrypt profile.
// Opened from the saved-server context menu (never blocking), it fetches the
// PUBLIC kit (vault_id, salt, KDF params, no secrets) from the keystore config
// via `aerocrypt_profile_recovery_kit` and lets the user re-view, save or print
// it any time. Mirrors the kit rendering in AeroCryptUnlock, without any unlock.

import * as React from 'react';
import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { FileKey, Loader2, X } from 'lucide-react';
import { useTranslation } from '../i18n';

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

interface Props {
    profileId: string;
    profileName?: string;
    onClose: () => void;
}

export const AeroCryptRecoveryKitModal: React.FC<Props> = ({ profileId, profileName, onClose }) => {
    const t = useTranslation();
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [kit, setKit] = useState<AerocryptEmergencyKit | null>(null);

    useEffect(() => {
        let cancelled = false;
        (async () => {
            setLoading(true);
            setError(null);
            try {
                const k = await invoke<AerocryptEmergencyKit>('aerocrypt_profile_recovery_kit', { profileId });
                if (!cancelled) setKit(k);
            } catch (e) {
                if (!cancelled) setError(String(e));
            } finally {
                if (!cancelled) setLoading(false);
            }
        })();
        return () => { cancelled = true; };
    }, [profileId]);

    const saveKitToFile = () => {
        if (!kit) return;
        const blob = new Blob([kit.text], { type: 'text/plain;charset=utf-8' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = 'aerocrypt-recovery-kit.txt';
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
        URL.revokeObjectURL(url);
    };

    const printKit = () => {
        if (!kit) return;
        const w = window.open('', '_blank');
        if (w) {
            w.document.write('<pre style="font-family: monospace; white-space: pre-wrap;">' + kit.text.replace(/&/g, '&amp;').replace(/</g, '&lt;') + '</pre>');
            w.document.close();
            w.focus();
            w.print();
        }
    };

    return (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={onClose}>
            <div className="bg-white dark:bg-gray-800 rounded-lg shadow-xl w-full max-w-lg mx-4 max-h-[90vh] overflow-y-auto" onClick={(e) => e.stopPropagation()}>
                <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <FileKey className="w-5 h-5 text-emerald-600 dark:text-emerald-400" />
                        <span className="font-semibold text-gray-800 dark:text-gray-100">{t('aerocryptNative.recoveryKitTitle')}</span>
                        {profileName && <span className="text-xs text-gray-400 dark:text-gray-500 truncate max-w-[12rem]">{profileName}</span>}
                    </div>
                    <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500">
                        <X className="w-4 h-4" />
                    </button>
                </div>

                <div className="p-4 space-y-3">
                    {loading ? (
                        <div className="flex items-center gap-2 text-sm text-gray-500 py-6 justify-center">
                            <Loader2 className="w-4 h-4 animate-spin" /> {t('common.loading')}
                        </div>
                    ) : error ? (
                        <div className="p-3 bg-amber-50 dark:bg-amber-900/30 border border-amber-300 dark:border-amber-700 rounded text-sm text-amber-700 dark:text-amber-300">
                            {error}
                        </div>
                    ) : kit ? (
                        <>
                            <p className="text-xs text-gray-600 dark:text-gray-300">{t('aerocryptNative.recoveryKitIntro')}</p>
                            <div className="bg-gray-50 dark:bg-gray-900 rounded p-2 text-xs font-mono whitespace-pre-wrap break-all max-h-64 overflow-auto">
                                {kit.text}
                            </div>
                            <div className="flex gap-2">
                                <button onClick={saveKitToFile} className="flex-1 px-3 py-1.5 text-sm rounded bg-emerald-600 text-white hover:bg-emerald-700">{t('aerocryptNative.saveKit')}</button>
                                <button onClick={printKit} className="flex-1 px-3 py-1.5 text-sm rounded bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600">{t('aerocryptNative.printKit')}</button>
                                <button onClick={onClose} className="px-3 py-1.5 text-sm rounded bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600">{t('common.close')}</button>
                            </div>
                        </>
                    ) : (
                        <div className="text-sm text-gray-500">{t('aerocryptNative.kitUnavailable')}</div>
                    )}
                </div>
            </div>
        </div>
    );
};
