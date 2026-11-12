// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { wrapIntentInvoke } from './testFixtures/syncIntents';
import { describe, expect, it } from 'vitest';
import { filesFromJournal, runRemoteSync, type RemoteSyncConfig, type SyncRunFile } from './remoteSyncRunner';
import { QueuedTargetChangedError } from './queuedTarget';
import type { SyncJournal } from '../types';

const config: RemoteSyncConfig = {
    localRoot: '/local', remoteRoot: '/remote', profileId: 'A', isProvider: true, isFtp: false,
    retryPolicy: { max_retries: 1, base_delay_ms: 1, max_delay_ms: 1, timeout_ms: 0, backoff_multiplier: 1 },
    verifyPolicy: 'none', deltaSyncEnabled: false, versionedBackup: { dir: '.v' },
    transferBudget: 0, direction: 'bidirectional',
};
const item: SyncRunFile = { relativePath: 'one', action: 'upload', size: 1, mtime: null, overwritesExisting: true };
const dirs = { remote: [], local: [] };

function harness() {
    let current = 'A'; let epoch = 1; let sourceExists = true; let switchAfterArchive = false;
    let failReceiptSave = false; let receiptExists = true;
    let saved: SyncJournal | undefined;
    const calls: string[] = [];
    const invoke = async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
        if (cmd === 'save_sync_journal_cmd') {
            const journal = args!.journal as SyncJournal;
            if (failReceiptSave && journal.entries[0].archive_state === 'archived') throw new Error('disk full');
            saved = structuredClone(journal);
        }
        if (cmd === 'sync_backup_run_stamp') return 'stamp' as T;
        if (cmd === 'provider_bulk_support') return null as T;
        if (cmd === 'sync_ec_generate') {
            if (args?.expectedGuiTarget !== epoch || current !== 'A') throw 'CONNECTION_CHANGED';
            calls.push(`A:sync_ec_generate:${String(args?.pct)}`);
            return { status: 'generated' } as T;
        }
        if (['sync_backup_archive_remote', 'sync_backup_verify_receipt', 'provider_upload_file', 'provider_mkdir'].includes(cmd)) {
            if (args?.expectedGuiTarget !== epoch || current !== 'A') throw 'CONNECTION_CHANGED';
            calls.push(`${current}:${cmd}:${String(args?.rel ?? '')}`);
            if (cmd === 'sync_backup_archive_remote') {
                if (String(args?.rel).endsWith('.aerocorrect')) return null as T;
                if (!sourceExists) return null as T;
                sourceExists = false;
                if (switchAfterArchive) current = 'B';
                return '/remote/.v/stamp/one' as T;
            }
            if (cmd === 'sync_backup_verify_receipt') return receiptExists as T;
            if (cmd === 'provider_upload_file') sourceExists = true;
        }
        return undefined as T;
    };
    const deps = () => ({ invoke: wrapIntentInvoke(invoke), remoteTarget: { ownerBinding: 'test-owner-v1', epoch, assertCurrent: () => {
        if (current !== 'A') throw new QueuedTargetChangedError('changed');
    } }, delay: async () => {}, now: () => 0, makeId: () => 'j' });
    return { calls, deps, saved: () => structuredClone(saved!),
        switchAfterArchive: () => { switchAfterArchive = true; },
        reconnectA: () => { current = 'A'; epoch++; switchAfterArchive = false; },
        recreate: () => { sourceExists = true; }, failReceiptSave: () => { failReceiptSave = true; },
        removeDestination: () => { sourceExists = false; },
        allowSaves: () => { failReceiptSave = false; }, removeReceipt: () => { receiptExists = false; } };
}
const run = (h: ReturnType<typeof harness>, journal?: SyncJournal, cancelled = false) =>
    runRemoteSync(journal ? filesFromJournal(journal) : [item], dirs, config, { isCancelled: () => cancelled },
        { ...h.deps(), ...(journal ? { resumeJournal: journal } : {}) });

describe('versioned backup resume', () => {
    it('restores policy and overwrite intent when stopped before any archive', async () => {
        const h = harness(); await run(h, undefined, true); const j = h.saved();
        expect(j.backup_dir).toBe('.v'); expect(j.entries[0].overwrites_existing).toBe(true);
        expect((await run(h, j)).uploaded).toBe(1);
        expect(h.calls).toContain('A:sync_backup_archive_remote:one');
    });
    it('retains source metadata and EC policy across a resume', async () => {
        const h = harness();
        const ecConfig = { ...config, errorCorrection: { enabled: true, pct: 12, maxFileSize: 1024 } };
        const ecItem = { ...item, mtime: '2026-11-11T00:00:00Z', expectedSha256: 'abc' };
        await runRemoteSync([ecItem], dirs, ecConfig, { isCancelled: () => true }, h.deps());
        const j = h.saved();
        expect(j.ec_enabled).toBe(true); expect(j.ec_pct).toBe(12); expect(j.ec_max_file_size).toBe(1024);
        expect(filesFromJournal(j)[0]).toMatchObject({ size: 1, mtime: ecItem.mtime, expectedSha256: 'abc' });
        await expect(runRemoteSync(filesFromJournal(j), dirs, config, {},
            { ...h.deps(), resumeJournal: j })).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
        await runRemoteSync(filesFromJournal(j), dirs, ecConfig, {}, { ...h.deps(), resumeJournal: j });
        expect(h.calls).toContain('A:sync_ec_generate:12');
    });
    it('persists standalone directories and refuses a caller that omits them', async () => {
        const h = harness(); const planned = { remote: ['empty'], local: [] };
        await runRemoteSync([item], planned, config, { isCancelled: () => true }, h.deps());
        expect(h.calls.filter(call => call.startsWith('A:provider_mkdir:'))).toHaveLength(0);
        const j = h.saved(); expect(j.remote_dirs).toEqual(['empty']);
        await expect(runRemoteSync(filesFromJournal(j), dirs, config, {},
            { ...h.deps(), resumeJournal: j })).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
        await runRemoteSync(filesFromJournal(j), planned, config, {}, { ...h.deps(), resumeJournal: j });
        expect(h.calls.filter(call => call.startsWith('A:provider_mkdir'))).toHaveLength(2);
    });
    it('verifies a saved receipt and never touches B after a switch before the sidecar', async () => {
        const h = harness(); h.switchAfterArchive(); await run(h); const j = h.saved();
        expect(j.entries[0].archive_state).toBe('archived');
        expect(h.calls.some(call => call.startsWith('B:'))).toBe(false);
        h.reconnectA(); expect((await run(h, j)).uploaded).toBe(1);
        expect(h.calls).toContain('A:sync_backup_verify_receipt:one');
    });
    it('archives a newly recreated destination even when an earlier receipt exists', async () => {
        const h = harness(); h.switchAfterArchive(); await run(h); const j = h.saved();
        h.reconnectA(); h.recreate(); await run(h, j);
        expect(h.calls.filter(call => call === 'A:sync_backup_archive_remote:one')).toHaveLength(2);
        expect(h.calls).not.toContain('A:sync_backup_verify_receipt:one');
    });
    it('archives a destination that appeared after an initially non-overwriting plan', async () => {
        const h = harness(); const newItem = { ...item, overwritesExisting: false };
        await runRemoteSync([newItem], dirs, config, { isCancelled: () => true }, h.deps());
        const j = h.saved();
        await run(h, j);
        const archive = h.calls.indexOf('A:sync_backup_archive_remote:one');
        const upload = h.calls.indexOf('A:provider_upload_file:');
        expect(archive).toBeGreaterThanOrEqual(0); expect(upload).toBeGreaterThan(archive);
    });
    it('continues a new-file resume when the destination is still absent', async () => {
        const h = harness(); const newItem = { ...item, overwritesExisting: false };
        await runRemoteSync([newItem], dirs, config, { isCancelled: () => true }, h.deps());
        const j = h.saved(); h.removeDestination();
        expect((await run(h, j)).uploaded).toBe(1);
    });
    it('refuses an ambiguous checkpoint or missing receipt without an upload', async () => {
        const h = harness(); h.failReceiptSave(); await run(h); const j = h.saved();
        expect(j.entries[0].archive_state).toBe('started'); h.allowSaves();
        await expect(run(h, j)).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
        expect(h.calls.filter(call => call.startsWith('A:provider_upload_file'))).toHaveLength(0);
    });
    it('refuses a caller that drops the saved overwrite intent or backup policy', async () => {
        const h = harness(); await run(h, undefined, true); const j = h.saved();
        await expect(runRemoteSync([{ ...item, overwritesExisting: false }], dirs, config, {},
            { ...h.deps(), resumeJournal: j })).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
        await expect(runRemoteSync(filesFromJournal(j), dirs, { ...config, versionedBackup: null }, {},
            { ...h.deps(), resumeJournal: j })).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
        expect(h.calls.filter(call => call.startsWith('A:provider_upload_file'))).toHaveLength(0);
    });
    it('does not silently complete a journal with an unreconstructable delete', async () => {
        const h = harness(); await run(h, undefined, true); const j = h.saved();
        j.entries[0].action = 'delete';
        await expect(run(h, j)).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
        expect(h.calls).toEqual([]);
    });
    it('rejects a legacy journal before any remote or local operation', async () => {
        const h = harness(); await run(h, undefined, true); const j = h.saved();
        delete j.resume_safe;
        await expect(run(h, j)).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
        expect(h.calls).toEqual([]);
    });
    it('rejects a resume without proof of the original remote owner', async () => {
        const h = harness(); await run(h, undefined, true); const j = h.saved();
        delete j.owner_binding;
        const deps = h.deps();
        await expect(runRemoteSync(filesFromJournal(j), dirs, config, {}, {
            ...deps, resumeJournal: j, remoteTarget: { ...deps.remoteTarget, ownerBinding: undefined },
        })).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
        expect(h.calls).toEqual([]);
    });
    it('refuses a missing saved backup without an upload', async () => {
        const h = harness(); h.switchAfterArchive(); await run(h); const j = h.saved();
        h.reconnectA(); h.removeReceipt(); const report = await run(h, j);
        expect(report.uploaded).toBe(0); expect(report.errors.length).toBeGreaterThan(0);
    });
});
