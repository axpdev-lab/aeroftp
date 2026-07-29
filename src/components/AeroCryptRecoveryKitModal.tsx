// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// On-demand Recovery Kit viewer for a saved AeroCrypt profile.
// Opened from the saved-server context menu or crypt badge (never blocking).
// Fetches the PUBLIC kit from the keystore via `aerocrypt_profile_recovery_kit`
// and lets the user re-view, save, print or verify a saved kit any time.

import * as React from 'react';
import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { pickFile, pickSave } from '../utils/pickPath';
import { readTextFile, writeTextFile } from '@tauri-apps/plugin-fs';
import { FileKey, Loader2, X } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { QRCodeSVG } from 'qrcode.react';

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

interface KitVerifyReport {
    ok: boolean;
    vault_id_match: boolean;
    salt_match: boolean;
    version_match: boolean;
    kdf_match: boolean;
    candidate_kdf_explicit: boolean;
    candidate_kind: string;
    details: string[];
    expected: AerocryptEmergencyKit;
    observed: AerocryptEmergencyKit;
}

type KitNoticeKind = 'error' | 'info' | 'legacy';

interface Props {
    profileId: string;
    profileName?: string;
    onClose: () => void;
}

function classifyKitError(raw: string): { kind: KitNoticeKind; key: string; fallback: string } {
    const msg = raw.replace(/^Error:\s*/i, '').trim();
    if (/no recovery kit cached/i.test(msg)) {
        return { kind: 'info', key: 'aerocryptNative.kitNotCachedYet', fallback: msg };
    }
    if (/vault_id/i.test(msg) && /tier\s*1|requires/i.test(msg)) {
        return {
            kind: 'legacy',
            key: 'aerocryptNative.kitNeedsVaultIdUpgrade',
            fallback: msg,
        };
    }
    return { kind: 'error', key: '', fallback: msg };
}

export const AeroCryptRecoveryKitModal: React.FC<Props> = ({ profileId, profileName, onClose }) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [errorKind, setErrorKind] = useState<KitNoticeKind>('error');
    const [kit, setKit] = useState<AerocryptEmergencyKit | null>(null);
    const [qrLevel, setQrLevel] = useState<'L' | 'M' | 'Q' | 'H'>('H');
    const [verifying, setVerifying] = useState(false);
    const [verifyReport, setVerifyReport] = useState<KitVerifyReport | null>(null);
    const [verifyError, setVerifyError] = useState<string | null>(null);
    const [actionMsg, setActionMsg] = useState<string | null>(null);
    const [actionErr, setActionErr] = useState<string | null>(null);

    useEffect(() => {
        let cancelled = false;
        (async () => {
            setLoading(true);
            setError(null);
            setErrorKind('error');
            try {
                const k = await invoke<AerocryptEmergencyKit>('aerocrypt_profile_recovery_kit', { profileId });
                if (!cancelled) setKit(k);
            } catch (e) {
                if (!cancelled) {
                    const c = classifyKitError(String(e));
                    setErrorKind(c.kind);
                    setError(
                        c.key
                            ? t(c.key, { defaultValue: c.fallback } as never) || c.fallback
                            : c.fallback,
                    );
                    // Prefer i18n when key exists (t may return the key path if missing)
                    if (c.key) {
                        const translated = t(c.key);
                        if (translated && translated !== c.key) setError(translated);
                        else setError(c.fallback);
                    }
                }
            } finally {
                if (!cancelled) setLoading(false);
            }
        })();
        return () => { cancelled = true; };
    }, [profileId, t]);

    const kitDefaultFileName = () => {
        // Filesystem-safe profile slug: spaces → -, strip unsafe path chars.
        const raw = (profileName || profileId || 'profile').trim();
        const slug = raw
            .replace(/[\\/:*?"<>|]+/g, '')
            .replace(/\s+/g, '-')
            .replace(/-+/g, '-')
            .replace(/^-|-$/g, '')
            .slice(0, 80) || 'profile';
        return `aerocrypt-recovery-kit-${slug}.txt`;
    };

    const saveKitToFile = async () => {
        if (!kit) return;
        setActionMsg(null);
        setActionErr(null);
        try {
            const path = await pickSave({
                defaultPath: kitDefaultFileName(),
                filters: [{ name: 'Text', extensions: ['txt'] }],
            });
            if (!path) return;
            await writeTextFile(path, kit.text);
            setActionMsg(t('aerocryptNative.kitSaved', { path }));
        } catch (e) {
            setActionErr(String(e));
        }
    };

    const printKit = () => {
        if (!kit) return;
        setActionMsg(null);
        setActionErr(null);
        try {
            const qrNote = `\n[${t('aerocryptNative.qrPrintNote', { level: qrLevel })}]\n`;
            const html =
                '<!DOCTYPE html><html><head><meta charset="utf-8"><title>AeroCrypt Recovery Kit</title>' +
                '<style>body{font-family:ui-monospace,monospace;white-space:pre-wrap;padding:16px;font-size:12px}</style>' +
                '</head><body>' +
                (kit.text + qrNote)
                    .replace(/&/g, '&amp;')
                    .replace(/</g, '&lt;') +
                '</body></html>';
            // WebKitGTK often blocks window.open from the app webview. An
            // off-DOM iframe still owns a printable document and works offline.
            const iframe = document.createElement('iframe');
            iframe.setAttribute('aria-hidden', 'true');
            iframe.style.cssText = 'position:fixed;right:0;bottom:0;width:0;height:0;border:0;opacity:0;pointer-events:none';
            document.body.appendChild(iframe);
            const doc = iframe.contentDocument || iframe.contentWindow?.document;
            if (!doc) {
                document.body.removeChild(iframe);
                setActionErr(t('aerocryptNative.kitPrintFailed'));
                return;
            }
            doc.open();
            doc.write(html);
            doc.close();
            const win = iframe.contentWindow;
            if (!win) {
                document.body.removeChild(iframe);
                setActionErr(t('aerocryptNative.kitPrintFailed'));
                return;
            }
            const cleanup = () => {
                try { document.body.removeChild(iframe); } catch { /* already gone */ }
            };
            win.onafterprint = cleanup;
            // Fallback cleanup if the engine never fires afterprint.
            setTimeout(cleanup, 60_000);
            win.focus();
            win.print();
        } catch (e) {
            setActionErr(String(e));
        }
    };

    const verifyKitFromFile = async () => {
        setVerifyError(null);
        setVerifyReport(null);
        setActionMsg(null);
        setActionErr(null);
        try {
            const picked = await pickFile({
                multiple: false,
                filters: [
                    {
                        name: 'AeroCrypt kit / marker',
                        extensions: ['txt', 'tsv', 'json'],
                    },
                ],
            });
            if (typeof picked !== 'string' || !picked) return;
            setVerifying(true);
            const text = await readTextFile(picked);
            const report = await invoke<KitVerifyReport>('aerocrypt_verify_recovery_kit', {
                profileId,
                kitOrMarkerText: text,
            });
            setVerifyReport(report);
        } catch (e) {
            setVerifyError(String(e));
        } finally {
            setVerifying(false);
        }
    };

    const noticeClass =
        errorKind === 'info'
            ? 'p-3 bg-sky-50 dark:bg-sky-900/30 border border-sky-300 dark:border-sky-700 rounded text-sm text-sky-800 dark:text-sky-200'
            : errorKind === 'legacy'
              ? 'p-3 bg-amber-50 dark:bg-amber-900/30 border border-amber-300 dark:border-amber-700 rounded text-sm text-amber-800 dark:text-amber-200'
              : 'p-3 bg-red-50 dark:bg-red-900/30 border border-red-300 dark:border-red-700 rounded text-sm text-red-800 dark:text-red-200';

    return (
        // Backdrop is non-dismissive: only X / Close close the kit (accidental
        // outside clicks were losing the modal mid-save/verify).
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
            <div
                {...modalDrag.panelProps}
                className="bg-white dark:bg-gray-800 rounded-lg shadow-xl w-full max-w-lg mx-4 max-h-[90vh] overflow-y-auto"
            >
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing"
                >
                    <div className="flex items-center gap-2">
                        <FileKey className="w-5 h-5 text-emerald-600 dark:text-emerald-400" />
                        <span className="font-semibold text-gray-800 dark:text-gray-100">{t('aerocryptNative.recoveryKitTitle')}</span>
                        {profileName && <span className="text-xs text-gray-400 dark:text-gray-500 truncate max-w-[12rem]">{profileName}</span>}
                    </div>
                    <button type="button" onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500">
                        <X className="w-4 h-4" />
                    </button>
                </div>

                <div className="p-4 space-y-3">
                    {loading ? (
                        <div className="flex items-center gap-2 text-sm text-gray-500 py-6 justify-center">
                            <Loader2 className="w-4 h-4 animate-spin" /> {t('common.loading')}
                        </div>
                    ) : error ? (
                        <div className={noticeClass}>
                            {error}
                        </div>
                    ) : kit ? (
                        <>
                            <p className="text-xs text-gray-600 dark:text-gray-300">{t('aerocryptNative.recoveryKitIntro')}</p>
                            <div className="bg-gray-50 dark:bg-gray-900 rounded p-2 text-xs font-mono whitespace-pre-wrap break-all max-h-64 overflow-auto">
                                {kit.text}
                            </div>

                            <div className="flex flex-col sm:flex-row gap-3 items-start">
                                <QRCodeSVG value={`AEROCRYPT-KIT v1\n${kit.text}`} level={qrLevel} size={112} includeMargin />
                                <div className="text-xs space-y-1">
                                    {(['L', 'M', 'Q', 'H'] as const).map((lv) => (
                                        <label key={lv} className="flex items-center gap-1 cursor-pointer">
                                            <input type="radio" name="qrL" checked={qrLevel === lv} onChange={() => setQrLevel(lv)} />
                                            {lv}
                                            {lv === 'H' ? ` ${t('aerocryptNative.qrRecommended')}` : ''}
                                        </label>
                                    ))}
                                </div>
                            </div>

                            {verifyReport && (
                                <div
                                    className={
                                        verifyReport.ok
                                            ? 'p-3 rounded text-sm border bg-emerald-50 dark:bg-emerald-900/30 border-emerald-300 dark:border-emerald-700 text-emerald-800 dark:text-emerald-200'
                                            : 'p-3 rounded text-sm border bg-red-50 dark:bg-red-900/30 border-red-300 dark:border-red-700 text-red-800 dark:text-red-200'
                                    }
                                >
                                    <div className="font-medium">
                                        {verifyReport.ok
                                            ? t('aerocryptNative.verifyKitOk')
                                            : t('aerocryptNative.verifyKitMismatch')}
                                    </div>
                                    <ul className="mt-1 list-disc list-inside text-xs space-y-0.5">
                                        {verifyReport.details.map((d, i) => (
                                            <li key={i}>{d}</li>
                                        ))}
                                    </ul>
                                    <div className="mt-1 text-[11px] opacity-80">
                                        {t('aerocryptNative.verifyKitKind', { kind: verifyReport.candidate_kind })}
                                    </div>
                                </div>
                            )}
                            {verifyError && (
                                <div className="p-3 bg-amber-50 dark:bg-amber-900/30 border border-amber-300 dark:border-amber-700 rounded text-sm text-amber-700 dark:text-amber-300">
                                    {verifyError}
                                </div>
                            )}
                            {actionMsg && (
                                <div className="p-2 bg-emerald-50 dark:bg-emerald-900/30 border border-emerald-300 dark:border-emerald-700 rounded text-xs text-emerald-800 dark:text-emerald-200">
                                    {actionMsg}
                                </div>
                            )}
                            {actionErr && (
                                <div className="p-2 bg-red-50 dark:bg-red-900/30 border border-red-300 dark:border-red-700 rounded text-xs text-red-800 dark:text-red-200">
                                    {actionErr}
                                </div>
                            )}

                            <div className="flex flex-wrap gap-2">
                                <button
                                    type="button"
                                    onClick={() => void saveKitToFile()}
                                    className="flex-1 min-w-[6rem] px-3 py-1.5 text-sm rounded bg-emerald-600 text-white hover:bg-emerald-700"
                                >
                                    {t('aerocryptNative.saveKit')}
                                </button>
                                <button
                                    type="button"
                                    onClick={printKit}
                                    className="flex-1 min-w-[6rem] px-3 py-1.5 text-sm rounded bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600"
                                >
                                    {t('aerocryptNative.printKit')}
                                </button>
                                <button
                                    type="button"
                                    onClick={() => void verifyKitFromFile()}
                                    disabled={verifying}
                                    className="flex-1 min-w-[6rem] px-3 py-1.5 text-sm rounded bg-sky-600 text-white hover:bg-sky-700 disabled:opacity-60 flex items-center justify-center gap-1"
                                >
                                    {verifying && <Loader2 className="w-3.5 h-3.5 animate-spin" />}
                                    {t('aerocryptNative.verifyKit')}
                                </button>
                                <button
                                    type="button"
                                    onClick={onClose}
                                    className="px-3 py-1.5 text-sm rounded bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600"
                                >
                                    {t('common.close')}
                                </button>
                            </div>
                            <p className="text-[11px] text-gray-500 dark:text-gray-400">{t('aerocryptNative.verifyKitHint')}</p>
                        </>
                    ) : (
                        <div className="text-sm text-gray-500">{t('aerocryptNative.kitUnavailable')}</div>
                    )}
                </div>
            </div>
        </div>
    );
};
