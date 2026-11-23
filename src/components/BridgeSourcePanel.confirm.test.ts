// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// Behavioural test of the panel's `confirmImport` closure, run from the
// production source: a preview confirmed once cannot be confirmed again
// (its scan already settled the secrets of the profiles left out), while a
// failed confirm leaves it retryable.

import { transformWithOxc } from 'vite';
import { describe, expect, it, vi } from 'vitest';
import panelSource from './BridgeSourcePanel.tsx?raw';
import type { ServerProfile } from '../types';
import { createScanSettler, commitImportedServers, importedServerProfile, type ImportedServer } from './bridge/bridgeImportCommit';
import { keptRelaxationId } from './ImportSecurityReview';

function confirmImportClosure() {
    const start = panelSource.indexOf('    const confirmImport =');
    const end = panelSource.indexOf('\n    const ', start + 1);
    if (start < 0 || end < 0) throw new Error('Missing closure: confirmImport');
    return panelSource.slice(start, end);
}

async function panel(context: Record<string, unknown>) {
    const { code } = await transformWithOxc(confirmImportClosure(), 'bridge-panel-confirm.ts');
    return new Function(...Object.keys(context), `${code}\nreturn { confirmImport };`)(
        ...Object.values(context),
    ) as { confirmImport: () => Promise<void> };
}

const servers: ImportedServer[] = [
    { id: 'srv_a', name: 'Alpha', host: 'a.example', port: 21, username: 'u' },
    { id: 'srv_b', name: 'Beta', host: 'b.example', port: 21, username: 'u' },
];

function fixture() {
    const onImport = vi.fn(async (_servers: ServerProfile[], _expectedUserId?: number | null) => undefined);
    const commit = vi.fn<typeof commitImportedServers>(async (selected, _keys, importFn) => {
        await importFn(selected, null);
        return { added: selected.length, updated: 0 };
    });
    const context: Record<string, unknown> = {
        result: { servers, skipped: [], sourcePath: null, totalRemotes: 2, scanId: 'scan-1' },
        selectedIds: new Set(servers.map(s => s.id)),
        allSecurityEntries: [],
        keptRelaxed: new Set<string>(),
        committingRef: { current: false },
        scans: createScanSettler(() => Promise.resolve()),
        setLoading: vi.fn(),
        setError: vi.fn(),
        setCommitted: vi.fn(),
        setSuccess: vi.fn(),
        cancelDelayedClose: vi.fn(),
        closeTimerRef: { current: null },
        onClose: vi.fn(),
        t: (key: string) => key,
        commitImportedServers: commit,
        existingServerKeys: new Set<string>(),
        onImport,
        importedServerProfile,
        keptRelaxationId,
    };
    return { context, onImport, commit };
}

describe('BridgeSourcePanel confirmImport', () => {
    it('blocks a second confirm of a preview already confirmed', async () => {
        const { context, onImport } = fixture();
        context.selectedIds = new Set(['srv_a']);
        const scans = context.scans as ReturnType<typeof createScanSettler>;
        await scans.track('scan-1');
        const { confirmImport } = await panel(context);

        await confirmImport();
        expect(onImport).toHaveBeenCalledTimes(1);
        expect(scans.isCommitted()).toBe(true);
        const passed = onImport.mock.calls[0][0] as ServerProfile[];
        expect(passed.map(s => s.id)).toEqual(['srv_a']);

        // Before the 2.5 s close timer fires, a confirm of another
        // selection must not import again: the first settle already took
        // back the secrets of the profiles left out.
        const { confirmImport: confirmOther } = await panel({
            ...context,
            selectedIds: new Set(['srv_b']),
        });
        await confirmOther();
        expect(onImport).toHaveBeenCalledTimes(1);
    });

    it('leaves a failed confirm retryable', async () => {
        const { context, onImport, commit } = fixture();
        const scans = context.scans as ReturnType<typeof createScanSettler>;
        await scans.track('scan-1');
        const { confirmImport } = await panel(context);

        commit.mockResolvedValueOnce({ added: 0, updated: 0, error: 'vault write failed' });
        await confirmImport();
        expect(onImport).not.toHaveBeenCalled();
        expect(scans.isCommitted()).toBe(false);
        expect(context.setError).toHaveBeenCalledWith('vault write failed');

        await confirmImport();
        expect(onImport).toHaveBeenCalledTimes(1);
        expect(scans.isCommitted()).toBe(true);
    });

    it('announces nothing when a newer scan lands during the release', async () => {
        const { context, onImport } = fixture();
        let releaseSettle!: () => void;
        const held = new Promise<void>(resolve => { releaseSettle = resolve; });
        const settle = vi.fn(() => held);
        const scans = createScanSettler(settle);
        context.scans = scans;
        await scans.track('scan-1');
        const { confirmImport } = await panel(context);

        const pending = confirmImport();
        // The commit is done and the scan's settle is in flight when the
        // next preview lands: it owns the panel now, so the confirmed
        // commit announces and closes nothing for the scan it replaced.
        await vi.waitFor(() => expect(settle).toHaveBeenCalledWith('scan-1'));
        await scans.track('scan-2');
        releaseSettle();
        await pending;

        expect(onImport).toHaveBeenCalledTimes(1);
        expect(context.setCommitted).not.toHaveBeenCalled();
        expect(context.setSuccess).not.toHaveBeenCalled();
        expect((context.closeTimerRef as { current: unknown }).current).toBeNull();
        expect(scans.isCommitted()).toBe(false);
    });
});
