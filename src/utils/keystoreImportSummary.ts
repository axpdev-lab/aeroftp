// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// The inline message shown after a keystore backup import: what was imported,
// and every part of the import that needs the user's attention.

import type { TranslationFunction } from '../i18n';

/** The fields of the `import_keystore` result that the message reports. */
export interface KeystoreImportOutcome {
    imported: number;
    skipped: number;
    requiresRestart?: boolean;
    userPartitionsRekeyed?: number;
    userPartitionsUnreadable?: number;
    profileDecisionsError?: string;
}

/**
 * An error as the end of a sentence. The notes are joined with a space, and a
 * note ending in a raw error (`...: {error}`) ran straight into the next one
 * ("...: locked Restart AeroFTP..."). The errors are the backend's English
 * text, so an English full stop is the right end for them.
 */
const asSentence = (error: string): string => {
    const trimmed = error.trim();
    return /[.!?]$/.test(trimmed) ? trimmed : `${trimmed}.`;
};

/**
 * Build the message for a finished import. `preferencesError` is the failure,
 * if any, of restoring the app preferences carried in the backup: that step
 * runs in the WebView after the backend import, and its failure must reach the
 * user, not only the console, while the rest of the message reports success.
 */
export function keystoreImportSummary(
    result: KeystoreImportOutcome,
    preferencesError: string | null,
    t: TranslationFunction,
): { type: 'success' | 'info'; text: string } {
    // Audit 2026-05-11 C2: the restart hint is appended to the success text
    // and the dedicated banner is triggered by the caller, so the hint survives
    // a toast that scrolls off-screen before the user reads it.
    const successText = t('settings.keystoreImported', {
        imported: result.imported,
        skipped: result.skipped,
    });
    // F-012: surface the cross-machine re-key outcome so a backup import never
    // silently leaves an empty "My Servers" with no explanation.
    const extraNotes: string[] = [];
    if ((result.userPartitionsRekeyed ?? 0) > 0) {
        extraNotes.push(t('settings.keystoreRekeyedPartitions', { count: result.userPartitionsRekeyed ?? 0, defaultValue: 'Re-keyed {count} account(s) to this device.' }));
    }
    if ((result.userPartitionsUnreadable ?? 0) > 0) {
        extraNotes.push(t('settings.keystoreUnreadablePartitions', { count: result.userPartitionsUnreadable ?? 0, defaultValue: '{count} account(s) could not be unlocked on this device. The backup was made on another computer: re-export it there with a password set on those accounts, then import it here.' }));
    }
    if (result.profileDecisionsError) {
        extraNotes.push(t('settings.keystoreDecisionsFailed', { error: asSentence(result.profileDecisionsError) }));
    }
    if (preferencesError) {
        extraNotes.push(t('settings.keystorePreferencesFailed', { error: asSentence(preferencesError) }));
    }
    if (result.requiresRestart) {
        extraNotes.push(t('settings.keystoreRestartRequired', { defaultValue: 'Restart AeroFTP to apply restored databases and plugins.' }));
    }
    const hadWarning = (result.userPartitionsUnreadable ?? 0) > 0
        || !!result.requiresRestart
        || !!result.profileDecisionsError
        || !!preferencesError;
    return {
        type: hadWarning ? 'info' : 'success',
        text: extraNotes.length > 0 ? `${successText}. ${extraNotes.join(' ')}` : successText,
    };
}
