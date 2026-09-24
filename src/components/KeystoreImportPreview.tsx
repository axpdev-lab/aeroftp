// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { AlertTriangle, KeyRound } from 'lucide-react';
import { useTranslation } from '../i18n';
import {
    decisionsFor,
    type ProfileChange,
    type ProfileDecision,
    type ProfilePreview,
} from '../utils/keystoreImportPreview';

interface KeystoreImportPreviewProps {
    preview: ProfilePreview;
    decisions: Record<string, ProfileDecision>;
    onDecide: (id: string, decision: ProfileDecision) => void;
}

/**
 * What a keystore import changes in the server profile list, one row per
 * profile, each with its own accept / reject / keep-both choice (#347).
 */
export const KeystoreImportPreview: React.FC<KeystoreImportPreviewProps> = ({ preview, decisions, onDecide }) => {
    const t = useTranslation();

    const kindLabel = (c: ProfileChange): string => {
        switch (c.kind) {
            case 'added': return t('settings.keystorePreviewAdded');
            case 'removed': return t('settings.keystorePreviewRemoved');
            case 'changed': return t('settings.keystorePreviewChanged');
        }
    };

    const decisionLabel = (c: ProfileChange, d: ProfileDecision): string => {
        if (d === 'both') return t('settings.keystoreDecisionKeepBoth');
        switch (c.kind) {
            case 'added': return d === 'accept' ? t('settings.keystoreDecisionAdd') : t('settings.keystoreDecisionSkip');
            case 'removed': return d === 'accept' ? t('settings.keystoreDecisionRemove') : t('settings.keystoreDecisionKeep');
            case 'changed': return d === 'accept' ? t('settings.keystoreDecisionUseBackup') : t('settings.keystoreDecisionKeepLocal');
        }
    };

    if (preview.changes.length === 0) {
        return (
            <div className="p-3 rounded-lg text-xs bg-gray-50 dark:bg-gray-800/60 border border-gray-200 dark:border-gray-700 text-gray-600 dark:text-gray-400">
                {t('settings.keystorePreviewNone')}
            </div>
        );
    }

    return (
        <div className="space-y-2">
            <div className="flex items-center justify-between text-xs font-medium text-gray-600 dark:text-gray-400">
                <span>{t('settings.keystorePreviewTitle')}</span>
                {preview.unchanged > 0 && (
                    <span className="font-normal">{t('settings.keystorePreviewUnchanged', { count: preview.unchanged })}</span>
                )}
            </div>
            {preview.localSource === 'vault' && (
                <div className="flex items-start gap-1.5 text-xs text-amber-700 dark:text-amber-400">
                    <AlertTriangle size={12} className="mt-0.5 shrink-0" />
                    <span>{t('settings.keystorePreviewLocalFromVault')}</span>
                </div>
            )}
            {preview.replacesList && preview.changes.some(c => c.kind === 'removed') && (
                <div className="flex items-start gap-1.5 text-xs text-amber-700 dark:text-amber-400">
                    <AlertTriangle size={12} className="mt-0.5 shrink-0" />
                    <span>{t('settings.keystorePreviewReplaces')}</span>
                </div>
            )}
            <ul className="max-h-64 overflow-y-auto space-y-1.5 pr-1">
                {preview.changes.map(c => {
                    const current = decisions[c.id] ?? c.defaultDecision;
                    const shown = c.fields.filter(f => !f.hidden);
                    const hidden = c.fields.filter(f => f.hidden).map(f => f.field);
                    return (
                        <li key={c.id} className="p-2 rounded-lg border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 text-xs space-y-1">
                            <div className="flex items-center justify-between gap-2">
                                <div className="min-w-0">
                                    <span className="font-medium text-gray-800 dark:text-gray-200 truncate">
                                        {c.backupName ?? c.localName ?? c.id}
                                    </span>
                                    <span className="ml-2 text-[10px] uppercase tracking-wide text-gray-400">{kindLabel(c)}</span>
                                    {c.protocol && <span className="ml-2 text-gray-400">{c.protocol}{c.host ? ` · ${c.host}` : ''}</span>}
                                </div>
                                <div className="flex shrink-0 rounded-md overflow-hidden border border-gray-200 dark:border-gray-600">
                                    {decisionsFor(c.kind).map(d => (
                                        <button
                                            key={d}
                                            type="button"
                                            onClick={() => onDecide(c.id, d)}
                                            className={`px-2 py-0.5 transition-colors ${
                                                current === d
                                                    ? 'bg-blue-600 text-white'
                                                    : 'bg-transparent text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700'
                                            }`}
                                        >
                                            {decisionLabel(c, d)}
                                        </button>
                                    ))}
                                </div>
                            </div>
                            {shown.map(f => (
                                <div key={f.field} className="font-mono text-[11px] text-gray-500 dark:text-gray-400 break-all">
                                    {f.field}: <span className="line-through opacity-70">{f.local ?? '∅'}</span> → <span className="text-gray-700 dark:text-gray-200">{f.backup ?? '∅'}</span>
                                </div>
                            ))}
                            {hidden.length > 0 && (
                                <div className="text-[11px] text-gray-500 dark:text-gray-400">
                                    {t('settings.keystorePreviewOtherFields', { fields: hidden.join(', ') })}
                                </div>
                            )}
                            {c.credentialsDiffer && (
                                <div className="flex items-center gap-1 text-[11px] text-amber-700 dark:text-amber-400">
                                    <KeyRound size={11} /> {t('settings.keystorePreviewCredentials')}
                                </div>
                            )}
                        </li>
                    );
                })}
            </ul>
        </div>
    );
};
