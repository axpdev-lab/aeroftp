// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Tests for the accepted Flatpak host-config import: a failure and an import
// that copied nothing must never come back as "imported".

import { describe, expect, it, vi, beforeEach } from 'vitest';

const mockInvoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
    invoke: (cmd: string, args?: unknown) => mockInvoke(cmd, args),
}));

import { acceptFlatpakImport, flatpakImportResultDialog, flatpakOfferHandlers, type FlatpakImportOutcome } from './flatpakImport';

const report = (copied: number, vault: { vault_imported?: boolean; vault_skipped?: boolean } = {}) => ({
    imported: copied > 0,
    copied,
    vault_imported: vault.vault_imported ?? false,
    vault_skipped: vault.vault_skipped ?? false,
    source: '/home/u/.config/aeroftp',
    target: '/sandbox/aeroftp',
});

// Echo the key and its parameters, so the assertions read what was asked for.
const t = (key: string, params?: Record<string, string | number>) => {
    const shown = Object.entries(params ?? {}).map(([k, v]) => `${k}=${v}`).join(',');
    return shown ? `${key}(${shown})` : key;
};

describe('acceptFlatpakImport', () => {
    beforeEach(() => {
        mockInvoke.mockReset();
    });

    it('accepts through the backend command', async () => {
        mockInvoke.mockResolvedValueOnce(report(2));
        await acceptFlatpakImport();
        expect(mockInvoke).toHaveBeenCalledWith('flatpak_config_import_apply', { accept: true });
    });

    it('reports an import when files were copied', async () => {
        mockInvoke.mockResolvedValueOnce(report(3));
        expect(await acceptFlatpakImport()).toEqual({ kind: 'imported', copied: 3, vault: 'absent' });
    });

    it('reports nothing to import when the sandbox already had every file', async () => {
        mockInvoke.mockResolvedValueOnce(report(0));
        expect(await acceptFlatpakImport()).toEqual({ kind: 'nothing', vault: 'absent' });
    });

    it('reports the host vault it copied', async () => {
        mockInvoke.mockResolvedValueOnce(report(4, { vault_imported: true }));
        expect(await acceptFlatpakImport()).toEqual({ kind: 'imported', copied: 4, vault: 'imported' });
    });

    it('reports a host vault left behind because this install has its own', async () => {
        mockInvoke.mockResolvedValueOnce(report(2, { vault_skipped: true }));
        expect(await acceptFlatpakImport()).toEqual({ kind: 'imported', copied: 2, vault: 'skipped' });
        mockInvoke.mockResolvedValueOnce(report(0, { vault_skipped: true }));
        expect(await acceptFlatpakImport()).toEqual({ kind: 'nothing', vault: 'skipped' });
    });

    it('reports the failure, with its message, when the import fails', async () => {
        mockInvoke.mockRejectedValueOnce('Import host config from /a to /b: Permission denied (os error 13)');
        expect(await acceptFlatpakImport()).toEqual({
            kind: 'failed',
            error: 'Import host config from /a to /b: Permission denied (os error 13)',
        });
    });

    it('does not read a response without a count as an import', async () => {
        mockInvoke.mockResolvedValueOnce({ imported: true, source: null, target: null });
        expect((await acceptFlatpakImport()).kind).toBe('failed');
    });
});

describe('flatpakImportResultDialog', () => {
    it('promises servers and vault only when the host vault was copied', () => {
        expect(flatpakImportResultDialog({ kind: 'imported', copied: 5, vault: 'imported' }, t)).toEqual({
            message: 'flatpak.importedBody',
            confirmLabel: 'flatpak.restartNow',
            restart: true,
        });
    });

    it('says the servers and vault were not imported when this install has its own vault', () => {
        expect(flatpakImportResultDialog({ kind: 'imported', copied: 2, vault: 'skipped' }, t)).toEqual({
            message: 'flatpak.importedVaultSkippedBody',
            confirmLabel: 'flatpak.restartNow',
            restart: true,
        });
        expect(flatpakImportResultDialog({ kind: 'nothing', vault: 'skipped' }, t)).toEqual({
            message: 'flatpak.importNothingVaultSkippedBody',
            confirmLabel: 'common.ok',
            restart: false,
        });
    });

    it('does not mention a vault the host config does not have', () => {
        expect(flatpakImportResultDialog({ kind: 'imported', copied: 1, vault: 'absent' }, t).message)
            .toBe('flatpak.importedNoVaultBody');
        expect(flatpakImportResultDialog({ kind: 'nothing', vault: 'absent' }, t).message)
            .toBe('flatpak.importNothingBody');
    });

    it('shows the error of a failed import, without a restart', () => {
        expect(flatpakImportResultDialog({ kind: 'failed', error: 'Permission denied' }, t)).toEqual({
            message: 'flatpak.importFailedBody(error=Permission denied)',
            confirmLabel: 'common.ok',
            restart: false,
        });
    });
});

describe('flatpakOfferHandlers', () => {
    /** Imports that run until the test finishes them, as a slow copy does. */
    const offer = () => {
        const running: Array<(outcome: FlatpakImportOutcome) => void> = [];
        const actions = {
            accept: vi.fn(() => new Promise<FlatpakImportOutcome>(resolve => { running.push(resolve); })),
            decline: vi.fn(async () => {}),
            showOutcome: vi.fn(),
            close: vi.fn(),
        };
        const finish = (outcome: FlatpakImportOutcome) => running.splice(0).forEach(resolve => resolve(outcome));
        return { actions, handlers: flatpakOfferHandlers(actions), finish };
    };

    it('starts one import however many times Import is clicked', async () => {
        const { actions, handlers, finish } = offer();
        const first = handlers.onConfirm();
        const second = handlers.onConfirm();
        finish({ kind: 'nothing', vault: 'absent' });
        await Promise.all([first, second]);
        expect(actions.accept).toHaveBeenCalledTimes(1);
        expect(actions.showOutcome).toHaveBeenCalledTimes(1);
    });

    it('ignores Cancel and Escape while the import runs', async () => {
        const { actions, handlers, finish } = offer();
        const running = handlers.onConfirm();
        await handlers.onCancel();
        // A decline here would write the decision marker, and a failed import
        // would then promise an offer at the next start that never comes.
        expect(actions.decline).not.toHaveBeenCalled();
        expect(actions.close).not.toHaveBeenCalled();
        finish({ kind: 'imported', copied: 2, vault: 'skipped' });
        await running;
        expect(actions.showOutcome).toHaveBeenCalledWith({ kind: 'imported', copied: 2, vault: 'skipped' });
    });

    it('declines once, and ignores Import after a decline', async () => {
        const { actions, handlers, finish } = offer();
        await handlers.onCancel();
        await handlers.onCancel();
        const late = handlers.onConfirm();
        finish({ kind: 'nothing', vault: 'absent' });
        await late;
        expect(actions.decline).toHaveBeenCalledTimes(1);
        expect(actions.close).toHaveBeenCalledTimes(1);
        expect(actions.accept).not.toHaveBeenCalled();
    });
});
