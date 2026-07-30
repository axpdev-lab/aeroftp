// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useCallback, useEffect, useRef, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { invoke } from '@tauri-apps/api/core';
import { pickFile } from '../utils/pickPath';
import { Eye, EyeOff, FolderOpen, Loader2, Lock, CheckCircle2, XCircle, X } from 'lucide-react';
import { useTranslation } from '../i18n';
import {
    type ArchiveKind,
    type ExtractProbe,
    extractToDir,
    isWrongPasswordError,
    needsPasswordPrompt,
    probeArchive,
    resolveUniqueExtractDir,
} from '../utils/extractOrchestrator';
import { TransferToastContainer } from './Transfer/TransferToastContainer';

/** Payload injected by the Rust `open_extract_window` initialization script. */
interface ExtractPayload {
    mode: 'here' | 'to';
    path: string;
    /** Two-letter desktop language code, so the window matches the OS language. */
    lang?: string;
}

declare global {
    interface Window {
        __AEROFTP_EXTRACT__?: ExtractPayload;
    }
}

type Phase = 'probing' | 'choosing' | 'password' | 'extracting' | 'done' | 'error';

/** Split an absolute path into its parent directory and base name, tolerant of
 *  both POSIX and Windows separators (the path is canonicalized by the backend). */
function splitPath(p: string): { dir: string; name: string } {
    const idx = Math.max(p.lastIndexOf('/'), p.lastIndexOf('\\'));
    if (idx < 0) return { dir: '.', name: p };
    return { dir: p.slice(0, idx) || '/', name: p.slice(idx + 1) };
}

const AUTO_CLOSE_MS = 5000;

/**
 * The entire UI of the dedicated lightweight `extract` window. It renders ONLY
 * the dialog needed for the OS "Extract here / to folder" verbs: nothing of the
 * main app is mounted here (no vault unlock, no sync), which is what keeps the
 * window cheap (see PHASE1-startup-measurement). Extraction itself is driven
 * through the shared orchestrator + `runExtractWithToast`.
 */
const ExtractWindow: React.FC = () => {
    const t = useTranslation();
    const [phase, setPhase] = useState<Phase>('probing');
    const [archiveName, setArchiveName] = useState('');
    const [destDir, setDestDir] = useState('');
    const [error, setError] = useState<string | null>(null);
    const [password, setPassword] = useState('');
    const [showPassword, setShowPassword] = useState(false);
    const [count, setCount] = useState<number | null>(null);

    // Resolved context kept in refs so the password-submit handler can reuse it
    // without re-probing.
    const ctx = useRef<{ kind: ArchiveKind; path: string; name: string; bytes: number; dest: string } | null>(null);

    const closeWindow = useCallback(() => {
        getCurrentWindow().close().catch(() => { /* already closing */ });
    }, []);

    const runExtract = useCallback(async (pwd: string | null) => {
        const c = ctx.current;
        if (!c) return;
        setPhase('extracting');
        setError(null);
        try {
            const out = await extractToDir({
                kind: c.kind,
                archivePath: c.path,
                archiveName: c.name,
                archiveBytes: c.bytes,
                destDir: c.dest,
                password: pwd,
            });
            setCount(out.count ?? null);
            setPassword('');
            setPhase('done');
            window.setTimeout(closeWindow, AUTO_CLOSE_MS);
        } catch (e) {
            setError(String(e));
            // A wrong password is recoverable: drop back to the prompt so the user
            // can retry without relaunching. But ONLY when the error actually looks
            // like a bad password: a real failure (disk full, unwritable dest,
            // corrupt archive) must surface its true message in the error phase,
            // not be mislabelled "wrong password" and loop forever. (aerozip has no
            // interactive retry, so it always goes to the dead-end error.)
            const retryPassword =
                c.kind !== 'aerozip' && pwd !== null && isWrongPasswordError(e);
            setPhase(retryPassword ? 'password' : 'error');
        }
    }, [closeWindow]);

    // Drive the whole flow once on mount.
    useEffect(() => {
        let cancelled = false;
        const drive = async () => {
            const payload = window.__AEROFTP_EXTRACT__;
            if (!payload || !payload.path) {
                setError('Missing extract payload');
                setPhase('error');
                return;
            }
            const { mode, path } = payload;
            const { dir: archiveDir, name } = splitPath(path);
            setArchiveName(name);
            try {
                setPhase('probing');
                const probe: ExtractProbe = await probeArchive(path);
                if (cancelled) return;

                // Replicate the standard GNOME extract behavior exactly: both verbs
                // extract into a folder named after the archive, never loose files.
                // "Extract here" uses the archive's own directory as the root (no
                // picker); "Extract to folder" lets the user pick the destination
                // root first. Both never clobber an existing folder (stem, stem (2)).
                let dest: string;
                if (mode === 'to') {
                    setPhase('choosing');
                    const chosen = await pickFile({
                        directory: true,
                        multiple: false,
                        title: t('extractWindow.chooseFolder'),
                    });
                    if (cancelled) return;
                    if (!chosen || Array.isArray(chosen)) {
                        // pickFile returns null for both cancel and "no chooser".
                        // The main App listens for aeroftp-toast; this window does
                        // not, so a portal-less host would otherwise close silently
                        // (#510 / #515 residual). Re-check before treating as cancel.
                        let unavailable: string | null = null;
                        try {
                            unavailable = (await invoke<string | null>('chooser_unavailable')) ?? null;
                        } catch {
                            unavailable = null;
                        }
                        if (unavailable !== null) {
                            const key =
                                unavailable === 'portal-missing'
                                    ? 'picker.unavailable.portalMissing'
                                    : 'picker.unavailable.unknown';
                            setError(`${t('picker.unavailable.title')}: ${t(key)}`);
                            setPhase('error');
                            return;
                        }
                        // Genuine user cancel.
                        closeWindow();
                        return;
                    }
                    dest = await resolveUniqueExtractDir(chosen, name);
                } else {
                    dest = await resolveUniqueExtractDir(archiveDir, name);
                }
                if (cancelled) return;
                setDestDir(dest);
                ctx.current = { kind: probe.kind, path, name, bytes: probe.archive_bytes, dest };

                if (needsPasswordPrompt(probe)) {
                    setPhase('password');
                } else {
                    await runExtract(null);
                }
            } catch (e) {
                if (cancelled) return;
                setError(String(e));
                setPhase('error');
            }
        };
        void drive();
        return () => { cancelled = true; };
    }, [t, closeWindow, runExtract]);

    const onSubmitPassword = (e: React.FormEvent) => {
        e.preventDefault();
        if (!password) return;
        void runExtract(password);
    };

    return (
        <div className="min-h-screen w-full bg-gray-50 dark:bg-gray-900 text-gray-900 dark:text-gray-100 flex flex-col">
            {/* Draggable titlebar (the window is borderless): AeroFile brand + close. */}
            <div
                data-tauri-drag-region
                className="flex items-center justify-between h-9 px-3 shrink-0 select-none bg-gray-100 dark:bg-gray-800 border-b border-gray-200 dark:border-gray-700"
            >
                <div data-tauri-drag-region className="flex items-center gap-1.5 text-xs font-semibold text-gray-700 dark:text-gray-200">
                    <FolderOpen size={13} className="text-sky-500" />
                    AeroFile
                </div>
                <button
                    onClick={closeWindow}
                    aria-label={t('common.close')}
                    className="text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 rounded p-0.5"
                >
                    <X size={15} />
                </button>
            </div>
            <div className="flex-1 flex flex-col items-center justify-center px-6 py-5 select-none">
                <div className="w-full max-w-sm">
                    <div className="flex items-center gap-2 mb-3">
                        <FolderOpen size={18} className="text-sky-500" />
                        <h1 className="text-sm font-semibold truncate" title={archiveName}>
                            {archiveName || t('extractWindow.title')}
                        </h1>
                    </div>

                    {phase === 'probing' && (
                        <div className="flex items-center gap-2 text-sm text-gray-600 dark:text-gray-300">
                            <Loader2 size={16} className="animate-spin" />
                            {t('extractWindow.inspecting')}
                        </div>
                    )}

                    {phase === 'choosing' && (
                        <div className="flex items-center gap-2 text-sm text-gray-600 dark:text-gray-300">
                            <FolderOpen size={16} />
                            {t('extractWindow.chooseFolder')}
                        </div>
                    )}

                    {phase === 'password' && (
                        <form onSubmit={onSubmitPassword} className="space-y-3">
                            <p className="text-xs text-gray-600 dark:text-gray-400 flex items-center gap-1.5">
                                <Lock size={13} /> {t('extractWindow.passwordPrompt')}
                            </p>
                            <div className="relative">
                                <input
                                    type={showPassword ? 'text' : 'password'}
                                    value={password}
                                    autoFocus
                                    onChange={(e) => setPassword(e.target.value)}
                                    placeholder={t('contextMenu.enterArchivePassword')}
                                    className="w-full rounded-md border border-gray-300 dark:border-gray-700 bg-white dark:bg-gray-800 px-3 py-2 pr-9 text-sm outline-none focus:ring-2 focus:ring-sky-500"
                                />
                                <button
                                    type="button"
                                    onClick={() => setShowPassword((v) => !v)}
                                    className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-200"
                                    tabIndex={-1}
                                    aria-label={showPassword ? t('extractWindow.hidePassword') : t('extractWindow.showPassword')}
                                >
                                    {showPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                </button>
                            </div>
                            {error && <p className="text-xs text-red-500">{t('contextMenu.wrongPassword')}</p>}
                            <div className="flex gap-2 justify-end pt-1">
                                <button type="button" onClick={closeWindow}
                                    className="px-3 py-1.5 text-sm rounded-md text-gray-600 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-800">
                                    {t('common.cancel')}
                                </button>
                                <button type="submit" disabled={!password}
                                    className="px-3 py-1.5 text-sm rounded-md bg-sky-500 text-white hover:bg-sky-600 disabled:opacity-50">
                                    {t('contextMenu.extract')}
                                </button>
                            </div>
                        </form>
                    )}

                    {phase === 'extracting' && (
                        <div className="flex items-center gap-2 text-sm text-gray-600 dark:text-gray-300">
                            <Loader2 size={16} className="animate-spin" />
                            {t('contextMenu.extracting')}
                        </div>
                    )}

                    {phase === 'done' && (
                        <div className="space-y-2">
                            <div className="flex items-center gap-2 text-sm text-emerald-600 dark:text-emerald-400">
                                <CheckCircle2 size={16} />
                                {count !== null
                                    ? t('vault.extractedAll', { count: String(count), path: destDir })
                                    : t('toast.extractedTo', { dest: destDir })}
                            </div>
                        </div>
                    )}

                    {phase === 'error' && (
                        <div className="space-y-3">
                            <div className="flex items-start gap-2 text-sm text-red-500">
                                <XCircle size={16} className="mt-0.5 shrink-0" />
                                <span className="break-words">{error || t('contextMenu.extractionFailed')}</span>
                            </div>
                            <div className="flex justify-end">
                                <button onClick={closeWindow}
                                    className="px-3 py-1.5 text-sm rounded-md bg-gray-200 dark:bg-gray-800 hover:bg-gray-300 dark:hover:bg-gray-700">
                                    {t('common.close')}
                                </button>
                            </div>
                        </div>
                    )}
                </div>
            </div>
            <TransferToastContainer />
        </div>
    );
};

export default ExtractWindow;
