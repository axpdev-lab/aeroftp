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
import { SITE_REPLY_TIMEOUT_SECS, maskedEcho, mayChangeFiles, replyText, type SiteCommandReport } from './siteCommand';
import { clearSiteTranscript, recordSiteExchange, siteHistory, siteTranscript, type SiteTranscriptEntry } from './siteSessionMemory';

interface SiteCommandDialogProps {
    isOpen: boolean;
    /** The session whose transcript and history the dialog shows. */
    sessionId: string;
    /** Server shown under the title (host, never credentials). */
    sessionLabel: string;
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

/** Reasons that mean the line itself was refused: keep it in the field to fix it. */
const INPUT_REASONS = new Set(['empty', 'control_character', 'too_long']);

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
}

export const SiteCommandDialog: React.FC<SiteCommandDialogProps> = ({ isOpen, sessionId, sessionLabel, onClose, onActivity, onFilesChanged }) => {
    const t = useTranslation();
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
    const changedFiles = useRef(false);
    // Which opening is on screen: a command that completes after its dialog
    // closed, or after it moved to another session, must not write into the
    // dialog shown now.
    const opening = useRef<{ sessionId: string; generation: number } | null>(null);
    const generation = useRef(0);
    const callbacks = useRef({ onActivity, onFilesChanged });
    useLayoutEffect(() => { callbacks.current = { onActivity, onFilesChanged }; }, [onActivity, onFilesChanged]);
    const inputRef = useRef<HTMLInputElement>(null);
    const endRef = useRef<HTMLDivElement>(null);

    useEffect(() => {
        if (!isOpen) return;
        generation.current += 1;
        opening.current = { sessionId, generation: generation.current };
        setTranscript(siteTranscript(sessionId));
        changedFiles.current = false;
        setEncrypted(null);
        // A status that resolves after this opening ended belongs to another
        // session: it must not clear the warning of the current one.
        let current = true;
        invoke<SiteSessionStatus>('provider_site_session')
            .then(status => { if (current) setEncrypted(status.encrypted); })
            .catch(() => { if (current) setEncrypted(null); });
        const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
        document.documentElement.classList.add('modal-open');
        const focus = window.setTimeout(() => inputRef.current?.focus(), 0);
        return () => {
            current = false;
            opening.current = null;
            setSending(false);
            window.clearTimeout(focus);
            document.documentElement.classList.remove('modal-open');
            // A draft can hold a password (CHPASS alice ...): it does not
            // outlive the dialog, sent or not.
            setLine('');
            setHistoryIndex(null);
            opener?.focus();
        };
    }, [isOpen, sessionId]);

    useEffect(() => {
        endRef.current?.scrollIntoView({ block: 'end' });
    }, [transcript]);

    const close = useCallback(() => onClose(changedFiles.current), [onClose]);

    const flashCopied = useCallback((text: string) => {
        copyText(text).then(() => {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1500);
        }).catch(() => { /* nothing was copied: no confirmation shown */ });
    }, []);

    const send = useCallback(async () => {
        const typed = line.trim();
        if (!typed || sending) return;
        const sentFrom = opening.current;
        setSending(true);
        let report: SiteCommandReport;
        try {
            report = await invoke<SiteCommandReport>('provider_site_command', { args: typed, timeoutSecs: SITE_REPLY_TIMEOUT_SECS });
        } catch {
            report = noSession();
        }
        // The session the command went to keeps its record even if the dialog
        // moved on; a session closed meanwhile gets nothing back.
        const recorded = recordSiteExchange(sessionId, typed, maskedEcho(typed), report);
        if (recorded === null) return;
        callbacks.current.onActivity(report);
        const now = opening.current;
        const stillShown = sentFrom !== null && now !== null
            && now.sessionId === sentFrom.sessionId && now.generation === sentFrom.generation;
        if (!stillShown) {
            if (mayChangeFiles(report)) callbacks.current.onFilesChanged(sessionId);
            return;
        }
        setTranscript(recorded);
        if (mayChangeFiles(report)) changedFiles.current = true;
        if (!(report.outcome === 'not_sent' && report.reason && INPUT_REASONS.has(report.reason))) setLine('');
        setHistoryIndex(null);
        setSending(false);
        inputRef.current?.focus();
    }, [line, sending, sessionId]);

    const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
        if (e.key === 'Enter') {
            e.preventDefault();
            void send();
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
                className="rounded-lg shadow-2xl w-full max-w-3xl mx-4 border border-gray-200 dark:border-gray-700 flex flex-col max-h-[85vh] animate-scale-in"
                style={{ backgroundColor: 'var(--color-bg-secondary)' }}
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

                <div className="flex-1 min-h-[12rem] overflow-y-auto mx-5 px-3 rounded-md border border-gray-200 dark:border-gray-700" style={{ backgroundColor: 'var(--color-bg-primary)' }} aria-live="polite">
                    {transcript.length === 0 ? (
                        <p className="py-6 text-center text-xs text-gray-500 dark:text-gray-400">{t('siteCommand.empty')}</p>
                    ) : transcript.map(entry => (
                        <SiteReplyView key={entry.id} entry={entry} hideCodes={hideCodes} onCopy={flashCopied} />
                    ))}
                    <div ref={endRef} />
                </div>

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
                        className="flex-1 px-3 py-2 rounded-lg border border-gray-300 dark:border-gray-600 font-mono text-sm focus:ring-2 focus:ring-blue-500 outline-none"
                        style={{ backgroundColor: 'var(--color-bg-primary)', color: 'var(--color-text-primary)' }}
                    />
                    <button
                        onClick={() => void send()}
                        disabled={sending || line.trim() === ''}
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
