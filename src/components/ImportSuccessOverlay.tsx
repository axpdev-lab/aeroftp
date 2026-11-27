// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useCallback, useEffect, useRef } from 'react';
import { CheckCircle2 } from 'lucide-react';
import { useTranslation } from '../i18n';

/** The real outcome counts of a finished import, shown in the success overlay. */
export interface ImportCounts {
    imported: number;
    skipped: number;
    overwritten: number;
}

/**
 * How long the success overlay stays before the modal closes itself. A
 * light pause long enough to read the counts, short enough to feel instant.
 */
export const IMPORT_SUCCESS_CLOSE_DELAY_MS = 1400;

/**
 * The brief success pause shown over the import modal once an import stored
 * its profiles: a centered green check with the real counts (imported /
 * skipped / overwritten). After `delayMs` it calls `onDone` (the modal then
 * closes); a click on the overlay or Escape calls it at once. Unmounting
 * clears the timer and the key listener, so a closed modal never fires late.
 */
export const ImportSuccessOverlay: React.FC<{
    counts: ImportCounts;
    onDone: () => void;
    delayMs?: number;
}> = ({ counts, onDone, delayMs = IMPORT_SUCCESS_CLOSE_DELAY_MS }) => {
    const t = useTranslation();
    // Fires at most once: the timer, a click and Escape all lead here, and
    // only the first one counts.
    const doneRef = useRef(false);
    const onDoneRef = useRef(onDone);
    useEffect(() => {
        onDoneRef.current = onDone;
    }, [onDone]);
    const fire = useCallback(() => {
        if (doneRef.current) return;
        doneRef.current = true;
        onDoneRef.current();
    }, []);
    useEffect(() => {
        const timer = setTimeout(fire, delayMs);
        const onKey = (e: KeyboardEvent) => {
            if (e.key === 'Escape') fire();
        };
        window.addEventListener('keydown', onKey);
        return () => {
            clearTimeout(timer);
            window.removeEventListener('keydown', onKey);
        };
    }, [fire, delayMs]);

    // Existing keys only, so every locale already translates the summary:
    // zero counts are left out, as in the dialog's success line.
    const parts = [t('settings.importSuccess', { count: counts.imported })];
    if (counts.skipped > 0) parts.push(t('settings.bridgeSkippedCount', { count: counts.skipped }));
    if (counts.overwritten > 0) parts.push(t('settings.overwrittenCount', { count: counts.overwritten }));

    return (
        <div
            data-testid="import-success-overlay"
            role="status"
            onClick={fire}
            className="fixed inset-0 z-[60] flex items-center justify-center bg-black/40 animate-fade-in cursor-pointer"
        >
            <div className="flex flex-col items-center gap-3 rounded-2xl bg-white dark:bg-gray-800 px-8 py-6 shadow-2xl animate-scale-in">
                <CheckCircle2 data-testid="import-success-check" size={48} className="text-green-500" />
                <div data-testid="import-success-summary" className="text-sm font-medium text-gray-700 dark:text-gray-200">
                    {parts.join(' \u00B7 ')}
                </div>
            </div>
        </div>
    );
};
