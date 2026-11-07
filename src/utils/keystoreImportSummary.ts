// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// The inline message shown after a keystore backup import: what was imported,
// and every part of the import that needs the user's attention.

import type { TranslationFunction } from '../i18n';
import { RELAXATION_LABEL_KEYS } from './profileSecurity';

/** The fields of the `import_keystore` result that the message reports. */
export interface KeystoreImportOutcome {
    imported: number;
    skipped: number;
    requiresRestart?: boolean;
    userPartitionsRekeyed?: number;
    userPartitionsUnreadable?: number;
    userPartitionsBackupPath?: string;
    /** The backup's account partition was left out: unreadable here (review of #980). */
    profilePartitionSkipped?: boolean;
    profileDecisionsError?: string;
    sqliteDbsFailed?: string[];
    /** Restored profiles whose security-relaxing options were put back to safe. */
    securityReset?: { name: string; keys: string[] }[];
    securityResetFailed?: number;
}

/** What the caller knows about the import that the result does not carry. */
export interface KeystoreImportContext {
    /**
     * The user reviewed the server list and chose per profile. Without a
     * review the import applied the plan's defaults on its own, so a failure
     * of that step is not about choices the user made.
     */
    reviewed: boolean;
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

export function keystoreImportModalResult(result: KeystoreImportOutcome): KeystoreImportOutcome | null {
    const needed = !!result.requiresRestart
        || (result.userPartitionsUnreadable ?? 0) > 0
        || (result.userPartitionsRekeyed ?? 0) > 0
        || !!result.userPartitionsBackupPath
        || !!result.sqliteDbsFailed?.length
        || (result.securityResetFailed ?? 0) > 0
        || !!result.securityReset?.length;
    return needed ? { ...result } : null;
}

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
    context: KeystoreImportContext = { reviewed: true },
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
        extraNotes.push(t('settings.keystoreRekeyedPartitions', { count: result.userPartitionsRekeyed ?? 0 }));
    }
    if ((result.userPartitionsUnreadable ?? 0) > 0) {
        extraNotes.push(t('settings.keystoreUnreadablePartitions', { count: result.userPartitionsUnreadable ?? 0 }));
    }
    if (result.profilePartitionSkipped) {
        extraNotes.push(t('settings.keystorePartitionSkipped'));
    }
    if (result.profileDecisionsError) {
        // Review B1 (4.2.1): a skip-existing import with no review applies the
        // default plan itself. When that fails, the user made no choices, so
        // the note says what was not done: the two lists were not merged.
        const key = context.reviewed ? 'settings.keystoreDecisionsFailed' : 'settings.keystoreListMergeFailed';
        extraNotes.push(t(key, { error: asSentence(result.profileDecisionsError) }));
    }
    if (result.securityReset?.length) {
        // CodeRabbit on #1524: a restore takes no option that turns a
        // security check off from the backup, and says which it put back.
        const details = result.securityReset
            .map(r => `${r.name} (${r.keys.map(k => t(RELAXATION_LABEL_KEYS[k] ?? k)).join(', ')})`)
            .join('; ');
        extraNotes.push(t('settings.keystoreSecurityReset', { count: result.securityReset.length, details }));
    }
    if ((result.securityResetFailed ?? 0) > 0) {
        extraNotes.push(t('settings.keystoreSecurityResetFailed', { count: result.securityResetFailed ?? 0 }));
    }
    if (result.sqliteDbsFailed?.length) {
        extraNotes.push(t('settings.keystoreDatabasesFailed', { names: result.sqliteDbsFailed.join(', ') }));
    }
    if (preferencesError) {
        extraNotes.push(t('settings.keystorePreferencesFailed', { error: asSentence(preferencesError) }));
    }
    if (result.requiresRestart) {
        extraNotes.push(t('settings.keystoreRestartRequired'));
    }
    const hadWarning = (result.userPartitionsUnreadable ?? 0) > 0
        || !!result.profilePartitionSkipped
        || !!result.requiresRestart
        || !!result.profileDecisionsError
        || (result.securityResetFailed ?? 0) > 0
        || !!result.sqliteDbsFailed?.length
        || !!preferencesError;
    return {
        type: hadWarning ? 'info' : 'success',
        text: extraNotes.length > 0 ? `${successText}. ${extraNotes.join(' ')}` : successText,
    };
}
