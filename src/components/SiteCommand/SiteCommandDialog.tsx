// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { X, Terminal, Send, ShieldAlert, Trash2, Copy } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { Checkbox } from '../ui/Checkbox';
import { useDraggableModal } from '../../hooks/useDraggableModal';
import { copyText } from '../../utils/clipboard';
import { SiteReplyView } from './SiteReplyView';
import { SiteTemplateBar } from './SiteTemplateBar';
import { SITE_REPLY_TIMEOUT_SECS, maskedEcho, mayChangeFiles, replyText, type SiteCommandReport, type RunSiteCommand } from './siteCommand';
import { clearSiteTranscript, recordSiteExchange, siteHistory, siteTranscript, type SiteTranscriptEntry } from './siteSessionMemory';
import { accountChangeGeneration } from '../../utils/userPartitions';

interface SiteCommandDialogProps {
    isOpen: boolean;
    /** The session whose transcript and history the dialog shows. */
    sessionId: string;
    /** Server shown under the title (host, never credentials). */
    sessionLabel: string;
    /** Saved profile that owns SITE templates. Null on Quick Connect. */
    profileId: string | null;
    /** `changedFiles` is true when a command that may change files got through. */
    onClose: (changedFiles: boolean) => void;
    /** One activity-log entry per exchange: the report names the verb only. */
    onActivity: (report: SiteCommandReport) => void;
    /**
     * A command that may have changed files completed after this dialog was
     * closed or moved to another session: App refreshes that session's list
     * only if it is still the active one.
     */
    onFilesChanged: (sessionId: string) => void;
}

/** What the dialog reports when the backend had no session to try. */
const noSession = (): SiteCommandReport => ({
    outcome: 'not_sent', command: 'SITE', verb_kind: 'other', code: null, lines: [],
    encoding: null, elapsed_ms: null, session_reset: false, session_reconnected: null, reason: 'not_connected',
});

/**
 * Connection > SITE Command: send `SITE <arguments>` through the open FTP/FTPS
 * session and read the whole reply. One command at a time, as the control
 * connection allows. Nothing typed or received is saved anywhere; the
 * transcript and history live in memory for this session only.
 */
/** `provider_site_session`: whether SITE applies, and whether the control connection is encrypted. */
interface SiteSessionStatus {
    supported: boolean;
    /** `null` when the session could not be asked (busy with a transfer). */
    encrypted: boolean | null;
    generation: number | null;
}

export const SiteCommandDialog: React.FC<SiteCommandDialogProps> = ({ isOpen, sessionId, sessionLabel, profileId, onClose, onActivity, onFilesChanged }) => {
    const t = useTranslation();
    const accountEpoch = accountChangeGeneration();
    const modalDrag = useDraggableModal();
    const [line, setLine] = useState('');
    const [sending, setSending] = useState(false);
    const [hideCodes, setHideCodes] = useState(false);
    const [copied, setCopied] = useState(false);
    const [transcript, setTranscript] = useState<SiteTranscriptEntry[]>([]);
    const [historyIndex, setHistoryIndex] = useState<number | null>(null);
    // Asked of the backend: an explicit-if-available session may have fallen
    // back to clear text, which the connection settings alone cannot tell.
    const [encrypted, setEncrypted] = useState<boolean | null>(null);
    const [ready, setReady] = useState(false);
    const [checking, setChecking] = useState(false);
    const changedFiles = useRef(false);
    // Which opening is on screen: a command that completes after its dialog
    // closed, or after it moved to another session, must not write into the
    // dialog shown now.
    type Opening = { sessionId: string; profileId: string | null; account: number; connection: number | null };
    const opening = useRef<Opening | null>(null);
    const flight = useRef<object | null>(null);
    const statusRead = useRef(0);
    const callbacks = useRef({ onActivity, onFilesChanged });
    useLayoutEffect(() => { callbacks.current = { onActivity, onFilesChanged }; }, [onActivity, onFilesChanged]);
    const inputRef = useRef<HTMLInputElement>(null);
    const endRef = useRef<HTMLDivElement>(null);

    const refreshStatus = useCallback(async () => {
        const target = opening.current;
        if (!target || target.account !== accountChangeGeneration() || flight.current) return;
        const read = ++statusRead.current;
        target.connection = null;
        setReady(false);
        setChecking(true);
        try {
            const status = await invoke<SiteSessionStatus>('provider_site_session');
            if (opening.current !== target || read !== statusRead.current || target.account !== accountChangeGeneration()) return;
            target.connection = status.supported && typeof status.generation === 'number'
                && Number.isSafeInteger(status.generation) && status.generation >= 0 ? status.generation : null;
            setEncrypted(status.encrypted);
            setReady(typeof target.connection === 'number');
        } catch {
            if (opening.current === target && read === statusRead.current) setEncrypted(null);
        } finally {
            if (opening.current === target && read === statusRead.current) setChecking(false);
        }
    }, []);

    useLayoutEffect(() => {
        if (!isOpen) return;
        const target = { sessionId, profileId, account: accountChangeGeneration(), connection: null };
        opening.current = target;
        flight.current = null;
        setSending(false);
        setTranscript(siteTranscript(sessionId));
        changedFiles.current = false;
        setEncrypted(null);
        void refreshStatus();
        const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
        document.documentElement.classList.add('modal-open');
        const focus = window.setTimeout(() => inputRef.current?.focus(), 0);
        return () => {
            opening.current = null;
            flight.current = null;
            statusRead.current += 1;
            setReady(false);
            setSending(false);
            window.clearTimeout(focus);
            document.documentElement.classList.remove('modal-open');
            // A draft can hold a password (CHPASS alice ...): it does not
            // outlive the dialog, sent or not.
            setLine('');
            setHistoryIndex(null);
            opener?.focus();
        };
    }, [isOpen, sessionId, profileId, accountEpoch, refreshStatus]);

    useEffect(() => {
        endRef.current?.scrollIntoView({ block: 'end' });
    }, [transcript]);

    const close = useCallback(() => {
        // Retire synchronously, before React commits a closed dialog.
        opening.current = null;
        flight.current = null;
        onClose(changedFiles.current);
    }, [onClose]);

    const flashCopied = useCallback((text: string) => {
        copyText(text).then(() => {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1500);
        }).catch(() => { /* nothing was copied: no confirmation shown */ });
    }, []);

    const deliver = useCallback<RunSiteCommand>(async (prepare) => {
        const sentFrom = opening.current;
        if (!sentFrom || sentFrom.sessionId !== sessionId || sentFrom.profileId !== profileId
            || sentFrom.account !== accountChangeGeneration() || typeof sentFrom.connection !== 'number' || flight.current) return false;
        const operation = {};
        flight.current = operation;
        setSending(true);
        const current = () => opening.current === sentFrom && flight.current === operation
            && sentFrom.account === accountChangeGeneration();
        try {
            const { line: typed, echo, remember } = await prepare();
            // Rendering is part of this opening's flight. It cannot send after a
            // close, profile/account change or replacement opening.
            if (!current()) return false;
            const args = typed.trim();
            if (!args) return false;
            const clearDraft = args === line.trim();
            let report: SiteCommandReport;
            try {
                report = await invoke<SiteCommandReport>('provider_site_command', {
                    args, timeoutSecs: SITE_REPLY_TIMEOUT_SECS, generation: sentFrom.connection,
                });
            } catch {
                report = noSession();
            }
            // A locked or switched account must not receive the old activity or
            // session transcript, even if an already-sent command just finished.
            if (sentFrom.account !== accountChangeGeneration()) return false;
            // The session the command went to keeps its record even if the dialog
            // moved on; a session closed meanwhile gets nothing back.
            const recorded = recordSiteExchange(sessionId, args, echo, report, remember);
            if (recorded === null) return false;
            callbacks.current.onActivity(report);
            if (!current()) {
                if (mayChangeFiles(report)) callbacks.current.onFilesChanged(sessionId);
                return false;
            }
            if (report.reason === 'connection_changed' || report.reason === 'not_connected') {
                sentFrom.connection = null;
                setReady(false);
            }
            setTranscript(recorded);
            if (mayChangeFiles(report)) changedFiles.current = true;
            // A refusal sent nothing. Keep the draft for a deliberate retry.
            if (clearDraft && report.outcome !== 'not_sent') setLine('');
            setHistoryIndex(null);
            inputRef.current?.focus();
            return true;
        } finally {
            if (flight.current === operation) {
                flight.current = null;
                if (opening.current === sentFrom) setSending(false);
            }
        }
    }, [line, sessionId, profileId]);

    const sendDraft = () => deliver(async () => ({ line: line.trim(), echo: maskedEcho(line.trim()), remember: true }));

    const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
        if (e.key === 'Enter') {
            e.preventDefault();
            void sendDraft();
        } else if (e.key === 'Escape') {
            e.preventDefault();
            close();
        } else if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
            const history = siteHistory(sessionId);
            if (history.length === 0) return;
            e.preventDefault();
            const from = historyIndex ?? history.length;
            const next = e.key === 'ArrowUp' ? Math.max(0, from - 1) : from + 1;
            if (next >= history.length) {
                setHistoryIndex(null);
                setLine('');
            } else {
                setHistoryIndex(next);
                setLine(history[next]);
            }
        }
    };

    const copyAll = () => flashCopied(transcript
        .map(entry => [`> ${entry.echo}`, replyText(entry.report.lines, hideCodes)].filter(Boolean).join('\n'))
        .join('\n\n'));

    const clear = () => {
        clearSiteTranscript(sessionId);
        setTranscript([]);
    };

    if (!isOpen) return null;

    return (
        <div className="fixed inset-0 bg-black/50 backdrop-blur-sm flex items-center justify-center z-50 animate-fadeIn">
            <div
                {...modalDrag.panelProps}
                role="dialog"
                aria-modal="true"
                aria-labelledby="site-command-title"
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-full max-w-3xl mx-4 border border-gray-200 dark:border-gray-700 flex flex-col max-h-[85vh] animate-scale-in"
            >
                <div {...modalDrag.dragHandleProps} className="flex justify-between items-start px-5 pt-4 pb-3 cursor-grab active:cursor-grabbing">
                    <div className="min-w-0">
                        <h3 id="site-command-title" className="text-lg font-bold text-gray-900 dark:text-white flex items-center gap-2">
                            <Terminal className="text-blue-500" size={20} />
                            {t('siteCommand.title')}
                        </h3>
                        <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5 truncate">{sessionLabel}{encrypted === true ? ' · TLS' : ''}</p>
                    </div>
                    <button onClick={close} className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors" title={t('common.close')} aria-label={t('common.close')}>
                        <X size={18} className="text-gray-500" />
                    </button>
                </div>

                {encrypted === false && (
                    <div className="mx-5 mb-2 px-3 py-2 rounded-md text-xs flex items-start gap-2 bg-amber-50 text-amber-800 dark:bg-amber-900/30 dark:text-amber-200">
                        <ShieldAlert size={14} className="mt-0.5 shrink-0" />
                        <span>{t('siteCommand.plainFtpNotice')}</span>
                    </div>
                )}

                <div className="flex items-center gap-3 px-5 pb-2 text-xs">
                    {!ready && <button type="button" onClick={() => void refreshStatus()} disabled={checking || sending}
                        className="px-2 py-1 rounded hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-40">
                        {checking ? t('common.loading') : t('common.refresh')}
                    </button>}
                    <Checkbox checked={hideCodes} onChange={setHideCodes} label={t('siteCommand.hideCodes')} labelClassName="text-xs text-gray-600 dark:text-gray-300" />
                    <span className="flex-1" />
                    {copied && <span className="text-gray-500 dark:text-gray-400">{t('siteCommand.copied')}</span>}
                    <button onClick={copyAll} disabled={transcript.length === 0} className="flex items-center gap-1 px-2 py-1 rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-300 disabled:opacity-40">
                        <Copy size={13} /> {t('siteCommand.copyAll')}
                    </button>
                    <button onClick={clear} disabled={transcript.length === 0} className="flex items-center gap-1 px-2 py-1 rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-300 disabled:opacity-40">
                        <Trash2 size={13} /> {t('siteCommand.clear')}
                    </button>
                </div>

                <div className="flex-1 min-h-[12rem] overflow-y-auto mx-5 px-3 rounded-md border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-900" aria-live="polite">
                    {transcript.length === 0 ? (
                        <p className="py-6 text-center text-xs text-gray-500 dark:text-gray-400">{t('siteCommand.empty')}</p>
                    ) : transcript.map(entry => (
                        <SiteReplyView key={entry.id} entry={entry} hideCodes={hideCodes} onCopy={flashCopied} />
                    ))}
                    <div ref={endRef} />
                </div>

                <SiteTemplateBar
                    key={`${sessionId}:${profileId ?? ''}:${accountEpoch}`}
                    profileId={profileId}
                    draft={line}
                    disabled={sending || !ready}
                    onRun={deliver}
                />

                <div className="flex items-center gap-2 px-5 pt-3">
                    <span className="font-mono text-sm font-semibold text-gray-500 dark:text-gray-400">SITE</span>
                    <input
                        ref={inputRef}
                        type="text"
                        value={line}
                        onChange={e => { setLine(e.target.value); setHistoryIndex(null); }}
                        onKeyDown={onKeyDown}
                        placeholder={t('siteCommand.placeholder')}
                        aria-label={t('siteCommand.inputLabel')}
                        autoComplete="off"
                        autoCorrect="off"
                        autoCapitalize="off"
                        spellCheck={false}
                        className="flex-1 px-3 py-2 rounded-lg border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 font-mono text-sm focus:ring-2 focus:ring-blue-500 outline-none"
                    />
                    <button
                        onClick={() => void sendDraft()}
                        disabled={sending || !ready || line.trim() === ''}
                        className="px-4 py-2 bg-blue-500 hover:bg-blue-600 disabled:opacity-50 text-white rounded-lg text-sm font-medium flex items-center gap-1.5"
                    >
                        <Send size={14} /> {sending ? t('siteCommand.sending') : t('siteCommand.send')}
                    </button>
                </div>

                <p className="px-5 pt-2 pb-4 text-[11px] text-gray-500 dark:text-gray-400">
                    {t('siteCommand.codeNotice')} {t('siteCommand.privacyNotice')}
                </p>
            </div>
        </div>
    );
};
