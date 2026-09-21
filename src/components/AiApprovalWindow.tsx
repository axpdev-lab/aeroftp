// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { AlertTriangle, Loader2, ShieldCheck, X } from 'lucide-react';
import { useTranslation } from '../i18n';

/** What `ai_approval_prompt` returns for this window. */
interface ApprovalPrompt {
    action: string;
    message: string;
    rememberForSession: boolean;
}

/**
 * The AeroAgent approval window. It shows one pending request and answers it
 * through `ai_approval_decide`, which the backend accepts only from a window it
 * opened for that request: the chat webview can ask for an approval but not
 * grant one. Closing the window, or Escape, is a refusal.
 */
const AiApprovalWindow: React.FC = () => {
    const t = useTranslation();
    const [prompt, setPrompt] = useState<ApprovalPrompt | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [sending, setSending] = useState(false);
    const cancelRef = useRef<HTMLButtonElement>(null);

    useEffect(() => {
        invoke<ApprovalPrompt>('ai_approval_prompt')
            .then(setPrompt)
            .catch((e) => setError(String(e)));
    }, []);

    // Cancel takes the initial focus, so Enter never approves by reflex.
    useEffect(() => {
        if (prompt) cancelRef.current?.focus();
    }, [prompt]);

    const decide = useCallback(async (approved: boolean) => {
        if (sending) return;
        setSending(true);
        try {
            await invoke('ai_approval_decide', { approved });
        } catch (e) {
            setError(String(e));
            setSending(false);
        }
    }, [sending]);

    useEffect(() => {
        const onKey = (event: KeyboardEvent) => {
            if (event.key === 'Escape') {
                event.preventDefault();
                void decide(false);
            }
        };
        window.addEventListener('keydown', onKey);
        return () => window.removeEventListener('keydown', onKey);
    }, [decide]);

    return (
        <div className="h-screen w-full bg-gray-50 dark:bg-gray-900 text-gray-900 dark:text-gray-100 flex flex-col">
            <div
                data-tauri-drag-region
                className="flex items-center justify-between h-9 px-3 shrink-0 select-none bg-gray-100 dark:bg-gray-800 border-b border-gray-200 dark:border-gray-700"
            >
                <div data-tauri-drag-region className="flex items-center gap-1.5 text-xs font-semibold text-gray-700 dark:text-gray-200">
                    <ShieldCheck size={14} className="text-emerald-500" />
                    {t('aiApproval.windowTitle')}
                </div>
                <button
                    onClick={() => void decide(false)}
                    className="text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 rounded p-0.5"
                    aria-label={t('common.cancel')}
                >
                    <X size={14} />
                </button>
            </div>

            <div className="flex-1 overflow-y-auto p-4 space-y-3">
                <div className="flex gap-2.5 rounded-md border border-emerald-500/30 bg-emerald-500/10 px-3 py-2 text-xs text-gray-700 dark:text-gray-300">
                    <ShieldCheck size={16} className="text-emerald-500 shrink-0 mt-0.5" />
                    <p>
                        <span className="font-semibold">{t('aiApproval.whyTitle')}</span>{' '}
                        {t('aiApproval.whyBody')}
                    </p>
                </div>

                {!prompt && !error && (
                    <div className="flex items-center gap-2 text-sm text-gray-500">
                        <Loader2 size={14} className="animate-spin" />
                    </div>
                )}

                {prompt && (
                    <>
                        <div className="flex items-start gap-2">
                            <AlertTriangle size={18} className="text-amber-500 shrink-0 mt-0.5" />
                            <div>
                                <p className="text-xs text-gray-500 dark:text-gray-400">{t('aiApproval.wantsTo')}</p>
                                <p className="text-base font-semibold">{prompt.action}</p>
                            </div>
                        </div>
                        {prompt.message && (
                            <pre className="whitespace-pre-wrap break-all rounded-md bg-gray-100 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 px-3 py-2 text-xs font-mono">
                                {prompt.message}
                            </pre>
                        )}
                        <p className="text-xs text-gray-500 dark:text-gray-400">
                            {prompt.rememberForSession ? t('aiApproval.scopeSession') : t('aiApproval.scopeOnce')}
                        </p>
                    </>
                )}

                {error && <p className="text-xs text-red-500 break-words">{error}</p>}
            </div>

            <div className="flex justify-end gap-2 px-4 py-3 shrink-0 border-t border-gray-200 dark:border-gray-700">
                <button
                    ref={cancelRef}
                    onClick={() => void decide(false)}
                    disabled={sending}
                    className="px-4 py-1.5 text-sm rounded-md text-gray-700 dark:text-gray-200 hover:bg-gray-200 dark:hover:bg-gray-800"
                >
                    {t('common.cancel')}
                </button>
                <button
                    onClick={() => void decide(true)}
                    disabled={!prompt || sending}
                    className="px-4 py-1.5 text-sm rounded-md text-white bg-amber-600 hover:bg-amber-700 disabled:opacity-50"
                >
                    {t('aiApproval.approve')}
                </button>
            </div>
        </div>
    );
};

export default AiApprovalWindow;
