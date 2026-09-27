// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// The accepted Flatpak host-config import and the outcome the GUI shows for it.

import { invoke } from '@tauri-apps/api/core';
import type { TranslationFunction } from '../i18n';

/** What `flatpak_config_import_apply` returns. */
export interface FlatpakImportReport {
    imported: boolean;
    copied: number;
    /** The host vault and saved servers were copied into this install. */
    vault_imported: boolean;
    /**
     * The host has a vault, and this install already had its own: the import
     * never overwrites, so the host vault and saved servers stayed behind.
     */
    vault_skipped: boolean;
    source: string | null;
    target: string | null;
}

/**
 * What the import did with the host vault and the saved servers encrypted
 * under it: copied, left behind because this install already has its own, or
 * nothing to say because the host config holds none.
 */
export type HostVault = 'imported' | 'skipped' | 'absent';

/** The three results the user must be able to tell apart after accepting. */
export type FlatpakImportOutcome =
    | { kind: 'imported'; copied: number; vault: HostVault }
    | { kind: 'nothing'; vault: HostVault }
    /** `error` is the backend's text, or `null` when the result could not be read. */
    | { kind: 'failed'; error: string | null };

/**
 * Run the accepted import and say what it did. Only files actually copied
 * count as an import: a failure, or a sandbox that already had every host
 * file, must not end in the "imported, restart now" dialog.
 */
export async function acceptFlatpakImport(): Promise<FlatpakImportOutcome> {
    let report: FlatpakImportReport;
    try {
        report = await invoke<FlatpakImportReport>('flatpak_config_import_apply', { accept: true });
    } catch (e) {
        return { kind: 'failed', error: e instanceof Error ? e.message : String(e) };
    }
    if (typeof report?.copied !== 'number') {
        return { kind: 'failed', error: null };
    }
    const vault: HostVault = report.vault_skipped ? 'skipped' : report.vault_imported ? 'imported' : 'absent';
    return report.copied > 0 ? { kind: 'imported', copied: report.copied, vault } : { kind: 'nothing', vault };
}

/** What the offer's buttons need from the GUI. */
export interface FlatpakOfferActions {
    /** Run the accepted import; never rejects (see {@link acceptFlatpakImport}). */
    accept: () => Promise<FlatpakImportOutcome>;
    /** Record the decline. */
    decline: () => Promise<void>;
    /** Replace the offer with the dialog for this outcome. */
    showOutcome: (outcome: FlatpakImportOutcome) => void;
    /** Close the offer after a decline. */
    close: () => void;
}

/**
 * The confirm and cancel handlers of the import offer (Escape runs cancel).
 * Only the first answer counts: the offer stays on screen while the copy runs,
 * and a second click on Import would start a second copy whose result replaces
 * the first one's dialog, while Cancel or Escape would record a decline, so a
 * failed import would then promise an offer at the next start that never
 * comes.
 */
export function flatpakOfferHandlers(actions: FlatpakOfferActions): {
    onConfirm: () => Promise<void>;
    onCancel: () => Promise<void>;
} {
    let answered = false;
    return {
        onConfirm: async () => {
            if (answered) return;
            answered = true;
            actions.showOutcome(await actions.accept());
        },
        onCancel: async () => {
            if (answered) return;
            answered = true;
            await actions.decline();
            actions.close();
        },
    };
}

/** The dialog the GUI shows for an outcome. */
export interface FlatpakImportResultDialog {
    message: string;
    confirmLabel: string;
    /** The confirm button restarts AeroFTP: only after files were copied. */
    restart: boolean;
}

/**
 * What the dialog after an accepted import says, and what its button does. It
 * says exactly what was imported: "your servers and vault" only when the host
 * vault was copied, and plainly that they were not when this install already
 * had its own vault, which the import never overwrites.
 */
export function flatpakImportResultDialog(
    outcome: FlatpakImportOutcome,
    t: TranslationFunction,
): FlatpakImportResultDialog {
    switch (outcome.kind) {
        case 'imported': {
            const body = {
                imported: 'flatpak.importedBody',
                skipped: 'flatpak.importedVaultSkippedBody',
                absent: 'flatpak.importedNoVaultBody',
            }[outcome.vault];
            return { message: t(body), confirmLabel: t('flatpak.restartNow'), restart: true };
        }
        case 'nothing':
            return {
                message: t(outcome.vault === 'skipped' ? 'flatpak.importNothingVaultSkippedBody' : 'flatpak.importNothingBody'),
                confirmLabel: t('common.ok'),
                restart: false,
            };
        case 'failed':
            return {
                message: t('flatpak.importFailedBody', {
                    error: outcome.error ?? t('flatpak.importUnreadableResult'),
                }),
                confirmLabel: t('common.ok'),
                restart: false,
            };
    }
}
