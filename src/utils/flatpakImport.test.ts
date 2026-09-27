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

import { acceptFlatpakImport } from './flatpakImport';

const report = (copied: number) => ({ imported: copied > 0, copied, source: '/home/u/.config/aeroftp', target: '/sandbox/aeroftp' });

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
        expect(await acceptFlatpakImport()).toEqual({ kind: 'imported', copied: 3 });
    });

    it('reports nothing to import when the sandbox already had every file', async () => {
        mockInvoke.mockResolvedValueOnce(report(0));
        expect(await acceptFlatpakImport()).toEqual({ kind: 'nothing' });
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
