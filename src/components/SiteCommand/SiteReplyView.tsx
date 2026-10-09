// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React from 'react';
import { Copy, AlertTriangle } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { replyText } from './siteCommand';
import type { SiteTranscriptEntry } from './siteSessionMemory';

interface SiteReplyViewProps {
    entry: SiteTranscriptEntry;
    hideCodes: boolean;
    onCopy: (text: string) => void;
}

/**
 * One exchange of the transcript: the echoed command, a chip with the reply
 * code, and the reply exactly as received. The chip never turns green: a
 * reply code does not say whether the command worked (glFTPd answers 200 to
 * some refusals), so 1xx-3xx stay neutral and only 4xx/5xx are marked.
 */
export const SiteReplyView: React.FC<SiteReplyViewProps> = ({ entry, hideCodes, onCopy }) => {
    const t = useTranslation();
    const { report } = entry;
    const text = replyText(report.lines, hideCodes);

    let chip: { label: string; className: string };
    if (report.outcome === 'replied' && report.code !== null) {
        chip = {
            label: String(report.code),
            className: report.code >= 400
                ? 'bg-amber-100 text-amber-800 dark:bg-amber-900/40 dark:text-amber-300'
                : 'bg-gray-100 text-gray-700 dark:bg-gray-700 dark:text-gray-200',
        };
    } else if (report.outcome === 'not_sent') {
        chip = { label: t('siteCommand.chipNotSent'), className: 'bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-300' };
    } else {
        chip = { label: t('siteCommand.chipNoReply'), className: 'bg-red-100 text-red-700 dark:bg-red-900/40 dark:text-red-300' };
    }

    const notices: string[] = [];
    if (report.outcome === 'not_sent' && report.reason) {
        notices.push(t(`siteCommand.notSent.${report.reason}`));
    }
    if (report.outcome === 'unknown' && report.reason) {
        notices.push(`${t(`siteCommand.unknown.${report.reason}`)} ${t('siteCommand.unknownTail')}`);
        notices.push(report.session_reconnected ? t('siteCommand.sessionReconnected') : t('siteCommand.sessionNotReconnected'));
    }
    if (report.session_reset) notices.push(t('siteCommand.sessionReset'));
    if (report.outcome === 'replied' && report.verb_kind === 'session_state') notices.push(t('siteCommand.sessionStateNote'));

    return (
        <div className="border-b border-gray-100 dark:border-gray-700/60 py-2 last:border-b-0">
            <div className="flex items-center gap-2 text-xs">
                <span className="font-mono text-gray-500 dark:text-gray-400 truncate" title={entry.echo}>&gt; {entry.echo}</span>
                <span className={`px-1.5 py-0.5 rounded font-mono font-semibold shrink-0 ${chip.className}`}>{chip.label}</span>
                {report.elapsed_ms !== null && (
                    <span className="text-gray-400 shrink-0">{t('siteCommand.elapsed', { ms: report.elapsed_ms })}</span>
                )}
                {report.encoding === 'latin1' && (
                    <span className="text-gray-400 shrink-0">{t('siteCommand.latin1')}</span>
                )}
                <span className="flex-1" />
                {report.lines.length > 0 && (
                    <button
                        onClick={() => onCopy(text)}
                        className="p-1 rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-500 shrink-0"
                        title={t('siteCommand.copyReply')}
                        aria-label={t('siteCommand.copyReply')}
                    >
                        <Copy size={13} />
                    </button>
                )}
            </div>
            {report.lines.length > 0 && (
                <pre className="mt-1 text-xs font-mono whitespace-pre overflow-x-auto leading-snug text-gray-800 dark:text-gray-100 select-text">{text}</pre>
            )}
            {notices.map(notice => (
                <p key={notice} className="mt-1 text-xs text-amber-700 dark:text-amber-300 flex items-start gap-1">
                    <AlertTriangle size={12} className="mt-0.5 shrink-0" />
                    <span>{notice}</span>
                </p>
            ))}
        </div>
    );
};
