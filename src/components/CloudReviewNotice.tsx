// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React from 'react';
import { useTranslation } from '../i18n';

export interface CloudSkippedEntry {
    path: string;
    reason: string;
    is_dir: boolean;
    needs_review?: boolean;
}

/** Keep the names and manual decision visible without treating a safe skip as a failed sync. */
export function CloudReviewNotice({ entries }: { entries: CloudSkippedEntry[] }) {
    const t = useTranslation();
    const files = entries.filter(entry => entry.needs_review);
    if (files.length === 0) return null;
    return (
        <div role="alert" className="mb-4 rounded-lg border border-amber-300 bg-amber-50 p-3 text-sm text-amber-900 dark:border-amber-700 dark:bg-amber-900/20 dark:text-amber-200">
            <p>{t('cloud.untrackedDestinationFiles', { count: files.length })}</p>
            <ul className="mt-2 max-h-24 overflow-y-auto break-all font-mono text-xs">
                {files.map((entry, index) => <li key={`${index}:${entry.path}`}>{entry.path}</li>)}
            </ul>
        </div>
    );
}
