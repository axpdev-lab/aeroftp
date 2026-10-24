// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

// AeroSync on a provider's native bulk (Requested 77): on Proton Drive in CLI
// mode each `provider_upload_file` started the official program once, so a
// run of 200 small files paid 200 starts. The files of a run that go the same
// way now travel in groups, and each one is still journalled, verified and
// recorded as on the single-file path.

import { describe, expect, it } from 'vitest';
import {
    runRemoteSync,
    type SyncRunFile,
    type RemoteSyncConfig,
    type RemoteSyncDeps,
} from './remoteSyncRunner';
import type { SyncErrorInfo, SyncJournal } from '../types';

const config = (over: Partial<RemoteSyncConfig> = {}): RemoteSyncConfig => ({
    localRoot: '/home/u/work',
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

const harness = (handlers: Record<string, Handler> = {}, support = { upload: true, download: true }) => {
    const calls: Call[] = [];
    const invoke = async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
        calls.push({ cmd, args });
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
    const deps: RemoteSyncDeps = { invoke, delay: async () => undefined, now: () => 0, makeId: () => 'journal' };
    const of = (cmd: string) => calls.filter((c) => c.cmd === cmd);
    const sent = () => of('provider_sync_transfer_bulk').map((c) => (c.args?.items as BulkItem[]).map((i) => i.remotePath));
    const journal = () => [...calls].reverse().find((c) => c.cmd === 'save_sync_journal_cmd')?.args?.journal as SyncJournal;
    return { calls, deps, of, sent, journal };
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
});
