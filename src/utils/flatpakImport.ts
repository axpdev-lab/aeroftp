// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// The accepted Flatpak host-config import and the outcome the GUI shows for it.

import { invoke } from '@tauri-apps/api/core';

/** What `flatpak_config_import_apply` returns. */
export interface FlatpakImportReport {
    imported: boolean;
    copied: number;
    source: string | null;
    target: string | null;
}

/** The three results the user must be able to tell apart after accepting. */
export type FlatpakImportOutcome =
    | { kind: 'imported'; copied: number }
    | { kind: 'nothing' }
    | { kind: 'failed'; error: string };

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
        return { kind: 'failed', error: 'unexpected response from the import' };
    }
    return report.copied > 0 ? { kind: 'imported', copied: report.copied } : { kind: 'nothing' };
}
