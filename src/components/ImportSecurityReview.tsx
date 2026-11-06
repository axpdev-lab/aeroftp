// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React from 'react';
import { ShieldAlert } from 'lucide-react';
import { useTranslation } from '../i18n';
import type { Relaxation } from '../utils/profileSecurity';

/** The security-relaxing options of one imported profile, as the backend reports them. */
export interface ImportedSecurity {
    fileIndex: number;
    name: string;
    reset: Relaxation[];
    kept: Relaxation[];
}

/** The i18n key that names each option of `src/shared/profile-security-relaxations.json`. */
const RELAXATION_LABELS: Record<string, string> = {
    verifyCert: 'settings.importSecurityVerifyCert',
    allowCleartextEndpoint: 'settings.importSecurityCleartextEndpoint',
    allowCleartextStorage: 'settings.importSecurityCleartextStorage',
    webdavScheme: 'settings.importSecurityPlainHttp',
    trustPasvAddress: 'settings.importSecurityPasvAddress',
    trustUnknownHosts: 'settings.importSecurityUnknownHostKeys',
};

/** The key a kept option travels under in the import call. */
export const keptRelaxationId = (fileIndex: number, key: string) => `${fileIndex}:${key}`;

interface ImportSecurityReviewProps {
    entries: ImportedSecurity[];
    kept: ReadonlySet<string>;
    onToggle: (id: string) => void;
}

/**
 * The options of an import that turn a security check off (#1506 Requested
 * 198), one checkbox each, unticked: an unticked option comes in its safe
 * state, a ticked one is kept as the file has it.
 */
export const ImportSecurityReview: React.FC<ImportSecurityReviewProps> = ({ entries, kept, onToggle }) => {
    const t = useTranslation();
    return (
        <div data-testid="import-security-review" className="rounded-lg border border-amber-300 dark:border-amber-700 bg-amber-50 dark:bg-amber-900/20 p-3 space-y-2">
            <div className="flex items-center gap-2 text-sm font-medium text-amber-800 dark:text-amber-300">
                <ShieldAlert size={16} className="flex-shrink-0" />
                {t('settings.importSecurityTitle')}
            </div>
            <p className="text-xs text-amber-700 dark:text-amber-400">{t('settings.importSecurityHelp')}</p>
            <ul className="space-y-1.5 max-h-48 overflow-y-auto">
                {entries.map(entry => (
                    <li key={entry.fileIndex} className="text-xs">
                        <div className="font-medium text-gray-800 dark:text-gray-200 truncate">{entry.name}</div>
                        {[...entry.reset, ...entry.kept].map(relaxation => {
                            const id = keptRelaxationId(entry.fileIndex, relaxation.key);
                            return (
                                <label key={id} className="flex items-center gap-2 pl-2 text-gray-700 dark:text-gray-300 cursor-pointer">
                                    <input
                                        type="checkbox"
                                        checked={kept.has(id)}
                                        onChange={() => onToggle(id)}
                                        data-testid={`import-security-keep-${id}`}
                                        className="rounded border-gray-300 dark:border-gray-600"
                                    />
                                    <span>{t(RELAXATION_LABELS[relaxation.key] ?? relaxation.key)}</span>
                                    <code className="text-[11px] text-gray-500 dark:text-gray-400">{relaxation.key}={relaxation.value}</code>
                                </label>
                            );
                        })}
                    </li>
                ))}
            </ul>
        </div>
    );
};
