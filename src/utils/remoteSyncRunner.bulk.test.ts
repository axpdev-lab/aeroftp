// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

// AeroSync on a provider's native bulk (Requested 77): on Proton Drive in CLI
// mode each `provider_upload_file` started the official program once, so a
// run of 200 small files paid 200 starts. The files of a run that go the same
// way now travel in groups, and each one is still journalled, verified and
// recorded as on the single-file path.

import { createIntentFixture } from './testFixtures/syncIntents';
import { describe, expect, it } from 'vitest';
import {
    filesFromJournal,
    runRemoteSync,
    type SyncRunFile,
    type RemoteSyncConfig,
    type RemoteSyncDeps,
} from './remoteSyncRunner';
import type { SyncErrorInfo, SyncJournal } from '../types';

const config = (over: Partial<RemoteSyncConfig> = {}): RemoteSyncConfig => ({
    localRoot: '/home/u/work',
    profileId: 'A',
    remoteRoot: '/my-files/work',
    isProvider: true,
    isFtp: false,
    retryPolicy: { max_retries: 3, base_delay_ms: 1, max_delay_ms: 1, timeout_ms: 0, backoff_multiplier: 1 },
    verifyPolicy: 'none',
    deltaSyncEnabled: false,
    versionedBackup: null,
    transferBudget: 0,
    direction: 'bidirectional',
    ...over,
});

interface Call {
    cmd: string;
    args: Record<string, unknown> | undefined;
}
type Handler = (args: Record<string, unknown> | undefined) => unknown;
type BulkItem = { localPath: string; remotePath: string; size: number; modified: string | null };

/** `provider_sync_transfer_bulk` answers by file name: `fail` fails, `skip` is not attempted. */
const answer = (items: BulkItem[]) =>
    items.map((item) => item.remotePath.includes('fail')
        ? { status: 'failed', message: 'Permission denied: refused' }
        : item.remotePath.includes('skip') ? { status: 'not_attempted' } : { status: 'completed' });

const harness = (handlers: Record<string, Handler> = {}, support = { upload: true, download: true }, intentFixture = createIntentFixture()) => {
    const calls: Call[] = [];
    const invoke = async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
        calls.push({ cmd, args: cmd === 'save_sync_journal_cmd' ? structuredClone(args) : args });
        if (handlers[cmd]) return handlers[cmd](args) as T;
        if (cmd === 'provider_bulk_support') return support as T;
        if (cmd === 'provider_sync_transfer_bulk') return answer(args?.items as BulkItem[]) as T;
        if (cmd === 'classify_transfer_error') {
            return {
                kind: 'permission_denied',
                message: String(args?.rawError),
                retryable: false,
                file_path: String(args?.filePath),
            } satisfies SyncErrorInfo as T;
        }
        return undefined as T;
    };
    const deps: RemoteSyncDeps = { invoke: intentFixture.wrap(invoke), remoteTarget: { ownerBinding: 'test-owner-v1', epoch: 1, assertCurrent: () => undefined }, delay: async () => undefined, now: () => 0, makeId: () => 'journal' };
    const of = (cmd: string) => calls.filter((c) => c.cmd === cmd);
    const sent = () => of('provider_sync_transfer_bulk').map((c) => (c.args?.items as BulkItem[]).map((i) => i.remotePath));
    const journal = () => [...calls].reverse().find((c) => c.cmd === 'save_sync_journal_cmd')?.args?.journal as SyncJournal;
    return { calls, deps, of, sent, journal, intentFixture };
};

const files = (count: number, action: SyncRunFile['action'], name = (i: number) => `docs/f${i}.txt`): SyncRunFile[] =>
    Array.from({ length: count }, (_, i) => ({
        relativePath: name(i),
        action,
        size: 1024,
        mtime: '2026-10-24T05:00:00Z',
    }));

describe('remoteSyncRunner on a native bulk', () => {
    it('sends 200 uploads in one call and journals every file', async () => {
        const h = harness();
        const report = await runRemoteSync(files(200, 'upload'), { remote: [], local: [] }, config(), {}, h.deps);
        expect(h.of('provider_sync_transfer_bulk')).toHaveLength(1);
        expect(h.of('provider_upload_file')).toHaveLength(0);
        expect(h.sent()[0]).toHaveLength(200);
        expect(h.of('provider_sync_transfer_bulk')[0].args?.upload).toBe(true);
        expect(report.uploaded).toBe(200);
        expect(report.totalBytes).toBe(200 * 1024);
        expect(report.errors).toHaveLength(0);
        expect(h.journal().entries.every((e) => e.status === 'completed' && e.attempts === 1)).toBe(true);
    });

    it('caps a call at 256 files and reads 300 downloads in two, with their remote times', async () => {
        const h = harness();
        const report = await runRemoteSync(files(300, 'download'), { remote: [], local: [] }, config(), {}, h.deps);
        expect(h.sent().map((group) => group.length)).toEqual([256, 44]);
        const first = (h.of('provider_sync_transfer_bulk')[0].args?.items as BulkItem[])[0];
        expect(first).toEqual({
            localPath: '/home/u/work/docs/f0.txt',
            remotePath: '/my-files/work/docs/f0.txt',
            size: 1024,
            modified: '2026-10-24T05:00:00Z',
        });
        expect(report.downloaded).toBe(300);
        expect(h.of('provider_download_file')).toHaveLength(0);
    });

    it('keeps a provider without the bulk on its single-file commands, for one call more', async () => {
        const h = harness({}, { upload: false, download: false });
        const report = await runRemoteSync(files(3, 'upload'), { remote: [], local: [] }, config(), {}, h.deps);
        expect(h.of('provider_bulk_support')).toHaveLength(1);
        expect(h.of('provider_sync_transfer_bulk')).toHaveLength(0);
        expect(h.of('provider_upload_file')).toHaveLength(3);
        expect(report.uploaded).toBe(3);
        // FTP and a local-local run do not ask.
        const ftp = harness();
        await runRemoteSync(files(3, 'upload'), { remote: [], local: [] }, config({ isProvider: false, isFtp: true }), {}, ftp.deps);
        expect(ftp.of('provider_bulk_support')).toHaveLength(0);
    });

    it('takes a failed file as final and sends a file the bulk did not take on its own', async () => {
        const h = harness();
        const run = files(3, 'upload', (i) => ['docs/ok.txt', 'docs/fail.txt', 'docs/skip.txt'][i]);
        const report = await runRemoteSync(run, { remote: [], local: [] }, config(), {}, h.deps);
        expect(h.of('provider_upload_file').map((c) => c.args?.remotePath)).toEqual(['/my-files/work/docs/skip.txt']);
        expect(report.uploaded).toBe(2);
        expect(report.errors.map((e) => e.file_path)).toEqual(['docs/fail.txt']);
        const status = Object.fromEntries(h.journal().entries.map((e) => [e.relative_path, e.status]));
        expect(status).toEqual({ 'docs/ok.txt': 'completed', 'docs/fail.txt': 'failed', 'docs/skip.txt': 'completed' });
    });

    it('reports a fixed message, not the provider text, when a failed bulk file cannot be classified', async () => {
        const secret = 'access_token=bulk-secret-never-visible';
        const statuses: unknown[] = [];
        const h = harness({
            provider_sync_transfer_bulk: (args) => (args?.items as BulkItem[]).map(() => ({ status: 'failed', message: `refused ${secret}` })),
            classify_transfer_error: () => { throw new Error('classifier unavailable'); },
        });
        const report = await runRemoteSync(files(2, 'upload'), { remote: [], local: [] }, config(),
            { onFileStatus: (...status) => { statuses.push(status); } }, h.deps);
        expect(h.of('classify_transfer_error')).toHaveLength(2);
        expect(report.errors.map((e) => e.message)).toEqual(Array(2).fill('Transfer failed; compare again before retrying'));
        expect(JSON.stringify(report)).not.toContain(secret);
        expect(JSON.stringify(statuses)).not.toContain(secret);
        expect(JSON.stringify(h.journal())).not.toContain(secret);
    });

    it('keeps the provider text of a failed bulk file out of the journal when the owner changed', async () => {
        const secret = 'access_token=owner-change-secret-never-visible';
        const statuses: unknown[] = [];
        const h = harness({
            provider_sync_transfer_bulk: () => [{ status: 'target_changed' }, { status: 'failed', message: `refused ${secret}` }],
            classify_transfer_error: () => { throw new Error('classifier unavailable'); },
        });
        const report = await runRemoteSync(files(2, 'upload'), { remote: [], local: [] }, config(),
            { onFileStatus: (...status) => { statuses.push(status); } }, h.deps).catch((error: unknown) => error);
        const failed = h.journal().entries.find((e) => e.relative_path === 'docs/f1.txt');
        expect(failed?.status).toBe('failed');
        expect(failed?.last_error?.message).toBe('Transfer failed; compare again before retrying');
        expect(JSON.stringify(h.of('save_sync_journal_cmd').map((c) => c.args))).not.toContain(secret);
        expect(JSON.stringify(report, Object.getOwnPropertyNames(report ?? {}))).not.toContain(secret);
        expect(String(report)).not.toContain(secret);
        expect(JSON.stringify(statuses)).not.toContain(secret);
    });

    it('stops a group at the file that reaches the budget, the last one the single-file path sends too', async () => {
        const h = harness();
        const report = await runRemoteSync(files(5, 'upload'), { remote: [], local: [] }, config({ transferBudget: 2500 }), {}, h.deps);
        expect(h.sent()).toEqual([['/my-files/work/docs/f0.txt', '/my-files/work/docs/f1.txt', '/my-files/work/docs/f2.txt']]);
        expect(report.uploaded).toBe(3);
        expect(report.skipped).toBe(2);
        expect(report.cancelled).toBe(true);
    });

    it('leaves the files after a Stop pending for a resume, and finishes the group already sent', async () => {
        const h = harness();
        let stopped = false;
        const run = files(300, 'upload');
        const report = await runRemoteSync(run, { remote: [], local: [] }, config(), {
            isCancelled: () => stopped,
            onProgress: (done) => {
                if (done === 1) stopped = true;
            },
        }, h.deps);
        expect(h.sent().map((group) => group.length)).toEqual([256]);
        expect(report.uploaded).toBe(256);
        expect(report.cancelled).toBe(true);
        const pending = h.journal().entries.filter((e) => e.status === 'pending');
        expect(pending).toHaveLength(44);
        expect(h.journal().completed).toBe(false);
    });

    it('keeps each replaced copy before the group goes, and keeps a file out when its copy could not be kept', async () => {
        const order: string[] = [];
        const h = harness({
            sync_backup_run_stamp: () => 'stamp',
            sync_backup_archive_remote: (args) => {
                order.push(`keep ${String(args?.rel)}`);
                if (String(args?.rel).endsWith('f1.txt')) throw new Error('read-only');
                return `kept/${String(args?.rel)}`;
            },
            provider_sync_transfer_bulk: (args) => {
                order.push('bulk');
                return answer(args?.items as BulkItem[]);
            },
        });
        const run = files(3, 'upload').map((f) => ({ ...f, overwritesExisting: true }));
        const report = await runRemoteSync(run, { remote: [], local: [] }, config({ versionedBackup: { dir: '.v' } }), {}, h.deps);
        expect(order.filter((o) => !o.includes('aerocorrect'))).toEqual(['keep docs/f0.txt', 'keep docs/f1.txt', 'keep docs/f2.txt', 'bulk']);
        expect(h.sent()).toEqual([['/my-files/work/docs/f0.txt', '/my-files/work/docs/f2.txt']]);
        expect(report.uploaded).toBe(2);
        expect(report.errors.map((e) => e.file_path)).toEqual(['docs/f1.txt']);
        // Kept once: the single-file path does not keep it a second time.
        expect(h.of('sync_backup_archive_remote').filter((c) => !String(c.args?.rel).includes('aerocorrect'))).toHaveLength(3);
    });

    it.each(['upload', 'download'] as const)('archives a lone %s before its single-file fallback', async (action) => {
        const order: string[] = [];
        const archive = action === 'upload' ? 'sync_backup_archive_remote' : 'sync_backup_archive_local';
        const h = harness({
            sync_backup_run_stamp: () => 'stamp',
            [archive]: (args) => {
                if (!String(args?.rel).endsWith('.aerocorrect')) order.push('archive');
                return 'kept';
            },
            [action === 'upload' ? 'provider_upload_file' : 'provider_download_file']: () => order.push('write'),
        });
        const run = files(1, action).map((f) => ({ ...f, overwritesExisting: true }));
        await runRemoteSync(run, { remote: [], local: [] }, config({ versionedBackup: { dir: '.v' } }), {}, h.deps);
        expect(order).toEqual(['archive', 'write']);
    });

    it('archives the final upload after a full bulk chunk exactly once', async () => {
        const h = harness({ sync_backup_run_stamp: () => 'stamp', sync_backup_archive_remote: () => 'kept' });
        const run = files(257, 'upload').map((f) => ({ ...f, overwritesExisting: true }));
        const report = await runRemoteSync(run, { remote: [], local: [] }, config({ versionedBackup: { dir: '.v' } }), {}, h.deps);
        expect(report.uploaded).toBe(257);
        expect(h.of('sync_backup_archive_remote').filter((c) => !String(c.args?.rel).endsWith('.aerocorrect'))).toHaveLength(257);
        expect(h.of('provider_upload_file')).toHaveLength(1);
    });

    it('captures local state before uploading a lone file without scan mtime', async () => {
        const order: string[] = [];
        const h = harness({
            get_file_properties: () => { order.push('read'); return { size: 7, modified: '2026-10-25T01:00:00' }; },
            provider_upload_file: () => order.push('write'),
        });
        await runRemoteSync(files(1, 'upload').map((f) => ({ ...f, mtime: null })),
            { remote: [], local: [] }, config(), {}, { ...h.deps, writeIndex: true });
        expect(order).toEqual(['read', 'write']);
    });

    it('verifies each downloaded file as the single-file path does', async () => {
        const h = harness({ verify_local_transfer: () => ({ passed: true, message: null }) });
        await runRemoteSync(files(4, 'download'), { remote: [], local: [] }, config({ verifyPolicy: 'size_only' }), {}, h.deps);
        expect(h.of('verify_local_transfer').map((c) => c.args?.localPath)).toEqual(
            [0, 1, 2, 3].map((i) => `/home/u/work/docs/f${i}.txt`),
        );
    });

    it('goes one by one for the rest of the run when the call is refused', async () => {
        const h = harness({
            provider_sync_transfer_bulk: () => {
                throw new Error('Not connected to any provider');
            },
        });
        const report = await runRemoteSync(files(4, 'download'), { remote: [], local: [] }, config(), {}, h.deps);
        expect(h.of('provider_sync_transfer_bulk')).toHaveLength(1);
        expect(h.of('provider_download_file')).toHaveLength(4);
        expect(report.downloaded).toBe(4);
    });

    it('does not send again what a resumed run already finished', async () => {
        const run = files(4, 'upload');
        const resumeJournal: SyncJournal = {
            resume_safe: true, intent_protocol: 1, owner_binding: 'test-owner-v1',
            profile_id: 'A',
            id: 'resume',
            created_at: '2026-10-24T05:00:00Z',
            updated_at: '2026-10-24T05:00:00Z',
            local_path: '/home/u/work',
            remote_path: '/my-files/work',
            direction: 'bidirectional',
            retry_policy: config().retryPolicy,
            verify_policy: 'none',
            entries: run.map((f, i) => ({
                relative_path: f.relativePath,
                source_path: null,
                action: 'upload',
                status: i < 2 ? 'completed' : 'pending',
                attempts: i < 2 ? 1 : 0,
                last_error: null,
                verified: null,
                bytes_transferred: i < 2 ? 1024 : 0,
                ec_status: null,
            })),
            completed: false,
        };
        const h = harness();
        const report = await runRemoteSync(run, { remote: [], local: [] }, config(), {}, { ...h.deps, resumeJournal });
        expect(h.sent()).toEqual([['/my-files/work/docs/f2.txt', '/my-files/work/docs/f3.txt']]);
        expect(report.uploaded).toBe(4);
    });

    // From the review: the copy a transfer replaces is moved into the backup
    // folder before the group goes. A Stop during the preparation used to let
    // it go on moving the copies of the files after it, then send nothing.
    it('stops preparing a group at a Stop, sends nothing, and leaves the rest untouched', async () => {
        let stopped = false;
        const h = harness({
            sync_backup_run_stamp: () => 'stamp',
            sync_backup_archive_remote: (args) => {
                if (String(args?.rel).endsWith('.aerocorrect')) return null;
                stopped = true; // the Stop comes while the first copy is kept
                return `kept/${String(args?.rel)}`;
            },
        });
        const run = files(3, 'upload').map((f) => ({ ...f, overwritesExisting: true }));
        const report = await runRemoteSync(run, { remote: [], local: [] }, config({ versionedBackup: { dir: '.v' } }), {
            isCancelled: () => stopped,
        }, h.deps);
        const kept = h.of('sync_backup_archive_remote').map((c) => String(c.args?.rel)).filter((rel) => !rel.endsWith('.aerocorrect'));
        expect(kept).toEqual(['docs/f0.txt']);
        expect(h.of('provider_sync_transfer_bulk')).toHaveLength(0);
        expect(h.of('provider_upload_file')).toHaveLength(0);
        expect(report.cancelled).toBe(true);
        const status = Object.fromEntries(h.journal().entries.map((e) => [e.relative_path, e.status]));
        expect(status).toEqual({ 'docs/f0.txt': 'pending', 'docs/f1.txt': 'pending', 'docs/f2.txt': 'pending' });
    });

    // A resume archives every download again, and a stopped run had already
    // moved those copies into the backup folder: on the local disk nothing
    // there is a fact, and the download goes on (it failed on every resume).
    it('resumes downloads whose copies a stopped run already kept, on the bulk and one by one', async () => {
        for (const support of [{ upload: true, download: true }, { upload: false, download: false }]) {
            const h = harness({ sync_backup_run_stamp: () => 'stamp', sync_backup_archive_local: () => null, sync_backup_verify_receipt: () => true }, support);
            const run = files(3, 'download');
            const resumeJournal: SyncJournal = {
            resume_safe: true, intent_protocol: 1, owner_binding: 'test-owner-v1',
            profile_id: 'A',
                id: 'resume',
                backup_dir: '.v',
                backup_stamp: 'stamp',
                created_at: '2026-10-24T05:00:00Z',
                updated_at: '2026-10-24T05:00:00Z',
                local_path: '/home/u/work',
                remote_path: '/my-files/work',
                direction: 'bidirectional',
                retry_policy: config().retryPolicy,
                verify_policy: 'none',
                entries: run.map((f) => ({
                    relative_path: f.relativePath,
                    overwrites_existing: true,
                    archive_state: 'archived',
                    archive_receipt: `/home/u/work/.v/stamp/${f.relativePath}`,
                    source_path: null,
                    action: 'download',
                    status: 'pending',
                    attempts: 0,
                    last_error: null,
                    verified: null,
                    bytes_transferred: 1024,
                    ec_status: null,
                })),
                completed: false,
            };
            const report = await runRemoteSync(filesFromJournal(resumeJournal), { remote: [], local: [] },
                config({ versionedBackup: { dir: '.v' } }), {}, { ...h.deps, resumeJournal });
            expect(report.errors).toEqual([]);
            expect(report.downloaded).toBe(3);
            expect(h.of('sync_backup_archive_local')).toHaveLength(3);
        }
    });
});

it.each(['upload', 'download'] as const)('persists every acknowledged bulk %s before a tab switch stops follow-up work', async action => {
    let current = true;
    const first = harness({ provider_sync_transfer_bulk: args => {
        current = false;
        return (args!.items as BulkItem[]).map(() => ({ status: 'completed' }));
    } });
    first.deps.remoteTarget!.assertCurrent = () => { if (!current) throw new Error('CONNECTION_CHANGED: tab switched'); };
    const plan = files(3, action);
    const report = await runRemoteSync(plan, { remote: [], local: [] }, config(), {}, first.deps);
    expect(report.cancelled).toBe(true);
    const persisted = first.of('save_sync_journal_cmd').slice(0, -1).map(call => call.args!.journal as SyncJournal);
    expect(persisted.some(saved => saved.entries.length === 3 && saved.entries.every(entry => entry.status === 'transferred'))).toBe(true);
    const journal = structuredClone(first.journal());
    expect(journal.entries.map(entry => entry.status)).toEqual(['transferred', 'transferred', 'transferred']);
    expect(journal.entries.map(entry => entry.bytes_transferred)).toEqual([1024, 1024, 1024]);
    const resumed = harness({}, undefined, first.intentFixture);
    if (action === 'download') {
        await expect(runRemoteSync(filesFromJournal(journal), { remote: [], local: [] }, config(), {}, { ...resumed.deps, resumeJournal: journal })).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
        expect(resumed.of('provider_download_file')).toHaveLength(0);
        return;
    }
    const result = await runRemoteSync(filesFromJournal(journal), { remote: [], local: [] }, config(), {}, { ...resumed.deps, resumeJournal: journal });
    expect(result.errors).toEqual([]);
    expect(resumed.of('provider_sync_transfer_bulk')).toHaveLength(0);
    expect(resumed.of('provider_upload_file')).toHaveLength(0);
    expect(resumed.of('provider_download_file')).toHaveLength(0);
    expect(resumed.journal().entries.every(entry => entry.status === 'completed')).toBe(true);
});

it('resumes acknowledged mixed-result downloads with verification, without downloading them again', async () => {
    const settings = config({ verifyPolicy: 'size_only' });
    const first = harness({ provider_sync_transfer_bulk: () => [
        { status: 'completed' }, { status: 'target_changed' }, { status: 'not_attempted' },
    ] });
    const report = await runRemoteSync(files(3, 'download'), { remote: [], local: [] }, settings, {}, first.deps);
    expect(report.cancelled).toBe(true);
    const journal = structuredClone(first.journal());
    expect(journal.entries[0].status).toBe('transferred');
    expect(first.of('verify_local_transfer')).toHaveLength(0);
    expect(first.of('sync_ec_verify_repair')).toHaveLength(0);
    const resumed = harness({
        sync_ec_verify_repair: () => ({ status: 'verified' }),
        verify_local_transfer: args => ({ passed: !String(args!.localPath).endsWith('f0.txt'), message: 'bad size' }),
    }, undefined, first.intentFixture);
    await expect(runRemoteSync(filesFromJournal(journal), { remote: [], local: [] }, settings, {}, { ...resumed.deps, resumeJournal: journal })).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
    expect(resumed.of('provider_download_file')).toHaveLength(0);
    expect(resumed.of('sync_ec_verify_repair')).toHaveLength(0);
    expect(journal.entries[0].status).toBe('transferred');
    expect(journal.completed).toBe(false);
});

it('keeps an acknowledged download unfinished when its requested local verification rejects', async () => {
    const h = harness({ verify_local_transfer: () => { throw new Error('could not inspect destination'); } });
    const report = await runRemoteSync(files(2, 'download'), { remote: [], local: [] }, config({ verifyPolicy: 'size_only' }), {}, h.deps);
    expect(report.verifyFailed).toBe(2);
    expect(h.journal().completed).toBe(false);
    expect(h.journal().entries.every(entry => entry.status === 'verify_failed')).toBe(true);
});

it('requires a new compare after an acknowledgement checkpoint rejects, including after restart', async () => {
    let persisted: SyncJournal | undefined;
    const h = harness({ save_sync_journal_cmd: args => {
        const snapshot = structuredClone(args!.journal as SyncJournal);
        if (snapshot.entries.some(entry => entry.status === 'transferred')) throw new Error('disk full at ack');
        persisted = snapshot;
    } });
    const plan = files(3, 'upload');
    const report = await runRemoteSync(plan, { remote: [], local: [] }, config(), {}, h.deps);
    expect(report.cancelled).toBe(true);
    expect(report.errors.some(error => error.message.includes('SYNC_RESUME_NEEDS_COMPARE'))).toBe(true);
    expect(persisted!.entries.every(entry => entry.status === 'in_progress')).toBe(true);
    expect(h.of('provider_sync_transfer_bulk')).toHaveLength(1);
    expect(h.of('provider_upload_file')).toHaveLength(0);
    const restarted = harness({}, undefined, h.intentFixture);
    await expect(runRemoteSync(filesFromJournal(persisted!), { remote: [], local: [] }, config(), {}, { ...restarted.deps, resumeJournal: persisted })).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
    expect(restarted.calls).toHaveLength(0);
});

it('never dispatches a bulk before its write-ahead checkpoint is persisted', async () => {
    const h = harness({ save_sync_journal_cmd: args => {
        if ((args!.journal as SyncJournal).entries.some(entry => entry.status === 'in_progress')) throw new Error('cannot save intent');
    } });
    const report = await runRemoteSync(files(3, 'upload'), { remote: [], local: [] }, config(), {}, h.deps);
    expect(report.cancelled).toBe(true);
    expect(report.errors.some(error => error.message.includes('SYNC_RESUME_NEEDS_COMPARE'))).toBe(true);
    expect(h.of('provider_sync_transfer_bulk')).toHaveLength(0);
    expect(h.of('provider_upload_file')).toHaveLength(0);
});

it('preserves the acknowledged upload local timestamp in an index-writing resume', async () => {
    let current = true;
    const first = harness({
        get_file_properties: () => ({ size: 1024, modified: '2026-11-11T01:02:03' }),
        provider_sync_transfer_bulk: args => { current = false; return (args!.items as BulkItem[]).map(() => ({ status: 'completed' })); },
    });
    first.deps.writeIndex = true;
    first.deps.remoteTarget!.assertCurrent = () => { if (!current) throw new Error('CONNECTION_CHANGED'); };
    const plan = files(3, 'upload').map(file => ({ ...file, mtime: null }));
    await runRemoteSync(plan, { remote: [], local: [] }, config(), {}, first.deps);
    const journal = structuredClone(first.journal());
    expect(journal.entries.every(entry => entry.local_modified === '2026-11-11T01:02:03Z')).toBe(true);
    const resumed = harness({}, undefined, first.intentFixture);
    resumed.deps.writeIndex = true;
    await runRemoteSync(filesFromJournal(journal), { remote: [], local: [] }, config(), {}, { ...resumed.deps, resumeJournal: journal });
    expect(resumed.of('get_file_properties')).toHaveLength(0);
    expect(resumed.journal().entries.every(entry => entry.local_modified === '2026-11-11T01:02:03Z')).toBe(true);
    const index = resumed.of('save_sync_index_cmd')[0].args!.index as { files: Record<string, { modified: string }> };
    expect(Object.values(index.files).every(entry => entry.modified === '2026-11-11T01:02:03Z')).toBe(true);
});

it.each(['upload', 'download'] as const)('requires a fresh compare before an unproven EC tail can modify an edited %s', async action => {
    let current = true;
    const settings = config({ errorCorrection: { enabled: true, pct: 10 } });
    const first = harness({ provider_sync_transfer_bulk: args => { current = false; return (args!.items as BulkItem[]).map(() => ({ status: 'completed' })); } });
    first.deps.remoteTarget!.assertCurrent = () => { if (!current) throw new Error('CONNECTION_CHANGED'); };
    await runRemoteSync(files(3, action).map(file => ({ ...file, expectedSha256: '1'.repeat(64) })), { remote: [], local: [] }, settings, {}, first.deps);
    const journal = structuredClone(first.journal());
    // The local file now contains a same-size user edit; a signed journal's
    // ownership proof alone is no content proof. Do not generate stale parity
    // or repair over that edit, even when size/mtime happen to match.
    const resumed = harness({ get_file_properties: () => ({ size: 1024, modified: '2026-11-12T00:00:00' }) }, undefined, first.intentFixture);
    await expect(runRemoteSync(filesFromJournal(journal), { remote: [], local: [] }, settings, {}, { ...resumed.deps, resumeJournal: journal })).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
    expect(resumed.calls).toHaveLength(0);
});

it('a single-file crash after its write cannot replay a saved pending entry', async () => {
    let crashSnapshot: SyncJournal | undefined;
    const h = harness({ provider_upload_file: () => { crashSnapshot = structuredClone(h.journal()); } }, { upload: false, download: false });
    expect((await runRemoteSync(files(1, 'upload'), { remote: [], local: [] }, config(), {}, h.deps)).uploaded).toBe(1);
    // This is exactly what reached disk before the native command; the DB
    // generation advanced independently before its first possible effect.
    expect(crashSnapshot!.entries[0].status).toBe('pending');
    const restarted = harness({}, undefined, h.intentFixture);
    await expect(runRemoteSync(filesFromJournal(crashSnapshot!), { remote: [], local: [] }, config(), {}, { ...restarted.deps, resumeJournal: crashSnapshot })).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
    expect(restarted.of('provider_upload_file')).toHaveLength(0);
    expect(restarted.of('provider_sync_transfer_bulk')).toHaveLength(0);
});

it.each([true, false])('stops all dispatch when an archive checkpoint rejects (bulk=%s)', async bulk => {
    const h = harness({ save_sync_journal_cmd: args => {
        if ((args!.journal as SyncJournal).entries.some(entry => entry.archive_state === 'started')) throw new Error('intent disk full');
    } }, { upload: bulk, download: bulk });
    const plan = files(3, 'upload').map(file => ({ ...file, overwritesExisting: true }));
    const report = await runRemoteSync(plan, { remote: [], local: [] }, config({ versionedBackup: { dir: '.v' } }), {}, h.deps);
    expect(report.cancelled).toBe(true);
    expect(report.errors.some(error => error.message.includes('SYNC_RESUME_NEEDS_COMPARE'))).toBe(true);
    expect(h.of('sync_backup_archive_remote')).toHaveLength(0);
    expect(h.of('provider_upload_file')).toHaveLength(0);
    expect(h.of('provider_sync_transfer_bulk')).toHaveLength(0);
});

it('never adopts a same-size edited acknowledged download as the synced index baseline', async () => {
    let current = true;
    const first = harness({ get_file_properties: () => ({ size: 1024, modified: '2026-11-11T00:00:00' }),
        provider_sync_transfer_bulk: args => { current = false; return (args!.items as BulkItem[]).map(() => ({ status: 'completed' })); } });
    first.deps.writeIndex = true;
    first.deps.remoteTarget!.assertCurrent = () => { if (!current) throw new Error('CONNECTION_CHANGED'); };
    await runRemoteSync(files(3, 'download').map(file => ({ ...file, mtime: null })), { remote: [], local: [] }, config({ verifyPolicy: 'size_only' }), {}, first.deps);
    const journal = structuredClone(first.journal());
    expect(journal.entries.every(entry => entry.local_modified === '2026-11-11T00:00:00Z')).toBe(true);
    const resumed = harness({ get_file_properties: () => ({ size: 1024, modified: '2026-11-12T00:00:00' }), verify_local_transfer: () => ({ passed: true }) }, undefined, first.intentFixture);
    resumed.deps.writeIndex = true;
    await expect(runRemoteSync(filesFromJournal(journal), { remote: [], local: [] }, config({ verifyPolicy: 'size_only' }), {}, { ...resumed.deps, resumeJournal: journal })).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
    expect(resumed.of('save_sync_index_cmd')).toHaveLength(0);
    expect(resumed.of('sync_ec_verify_repair')).toHaveLength(0);
    expect(journal.entries[0].local_modified).toBe('2026-11-11T00:00:00Z');
});

describe('sync intent fixture', () => {
    it('refuses to begin intents for a pair that holds no run lease, even with no token', async () => {
        const fixture = createIntentFixture();
        const invoke = fixture.wrap(async <T,>() => undefined as T);
        await expect(invoke('sync_intent_begin_cmd', { localPath: '/l', remotePath: '/r', paths: ['a.txt'] }))
            .rejects.toThrow('SYNC_RUN_BUSY');
        expect(fixture.generations.size).toBe(0);
        // With the pair's lease the same request is served.
        const lease = await invoke<string>('sync_run_lease_begin_cmd', { localPath: '/l', remotePath: '/r' });
        await expect(invoke('sync_intent_begin_cmd', { localPath: '/l', remotePath: '/r', paths: ['a.txt'], syncRunLease: lease }))
            .resolves.toHaveLength(1);
        expect([...fixture.generations.keys()]).toEqual(['a.txt']);
    });
});
