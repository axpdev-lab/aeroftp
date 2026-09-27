// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Tests for the message shown after a keystore backup import: every part of
// the import that failed or needs a restart must reach the user.

import { describe, expect, it } from 'vitest';
import { keystoreImportSummary } from './keystoreImportSummary';

// Echo the key and its parameters, so the assertions read what was asked for.
const t = (key: string, params?: Record<string, string | number>) => {
    const shown = Object.entries(params ?? {})
        .filter(([k]) => k !== 'defaultValue')
        .map(([k, v]) => `${k}=${v}`)
        .join(',');
    return shown ? `${key}(${shown})` : key;
};

describe('keystoreImportSummary', () => {
    it('reports a clean import as a success', () => {
        expect(keystoreImportSummary({ imported: 12, skipped: 1 }, null, t)).toEqual({
            type: 'success',
            text: 'settings.keystoreImported(imported=12,skipped=1)',
        });
    });

    it('reports preferences that could not be restored, with the error', () => {
        const summary = keystoreImportSummary({ imported: 12, skipped: 0 }, 'QuotaExceededError', t);
        expect(summary.type).toBe('info');
        expect(summary.text).toContain('settings.keystorePreferencesFailed(error=QuotaExceededError.)');
    });

    it('keeps reporting a restart and a failed profile decision', () => {
        const summary = keystoreImportSummary(
            { imported: 3, skipped: 0, requiresRestart: true, profileDecisionsError: 'locked' },
            null,
            t,
        );
        expect(summary.type).toBe('info');
        expect(summary.text).toBe(
            'settings.keystoreImported(imported=3,skipped=0). settings.keystoreDecisionsFailed(error=locked.) settings.keystoreRestartRequired',
        );
    });

    it('ends an error with a full stop, so the next note does not run into it', () => {
        const summary = keystoreImportSummary(
            { imported: 1, skipped: 0, requiresRestart: true },
            'QuotaExceededError: The quota has been exceeded',
            t,
        );
        expect(summary.text).toContain('(error=QuotaExceededError: The quota has been exceeded.) settings.keystoreRestartRequired');
        // An error that already ends a sentence is left as it is.
        expect(keystoreImportSummary({ imported: 1, skipped: 0 }, 'Disk full.', t).text)
            .toContain('(error=Disk full.)');
    });

    it('treats re-keyed accounts as information, and unreadable ones as a warning', () => {
        expect(keystoreImportSummary({ imported: 1, skipped: 0, userPartitionsRekeyed: 2 }, null, t).type).toBe('success');
        expect(keystoreImportSummary({ imported: 1, skipped: 0, userPartitionsUnreadable: 1 }, null, t).type).toBe('info');
    });
});
