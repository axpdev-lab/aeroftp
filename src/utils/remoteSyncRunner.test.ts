// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import {
    runRemoteSync,
    groupErrorsByKind,
    filesFromJournal,
    retryPolicyForSpeed,
    type SyncRunFile,
    type SyncRunDirs,
    type RemoteSyncConfig,
    type RemoteSyncDeps,
} from './remoteSyncRunner';
import type {
    RetryPolicy,
    SyncErrorInfo,
    SyncJournal,
    VerifyResult,
} from '../types';

const RETRY: RetryPolicy = {
    max_retries: 3,
    base_delay_ms: 500,
    max_delay_ms: 10_000,
    timeout_ms: 0,
    backoff_multiplier: 2,
};

const baseConfig = (over: Partial<RemoteSyncConfig> = {}): RemoteSyncConfig => ({
    localRoot: '/home/u/work',
    remoteRoot: '/srv/data',
    isProvider: false,
    isFtp: false,
    retryPolicy: RETRY,
    verifyPolicy: 'none',
    deltaSyncEnabled: false,
    versioningStrategy: null,
    transferBudget: 0,
    direction: 'bidirectional',
    ...over,
});

const noDirs: SyncRunDirs = { remote: [], local: [] };

interface Call {
    cmd: string;
    args: Record<string, unknown> | undefined;
}

type Handler = (
    args: Record<string, unknown> | undefined,
    cmdCallIndex: number,
) => unknown;

const makeInvoke = (handlers: Record<string, Handler> = {}) => {
    const calls: Call[] = [];
    const perCmd = new Map<string, number>();
    const invoke = async <T = unknown>(
        cmd: string,
        args?: Record<string, unknown>,
    ): Promise<T> => {
        const idx = perCmd.get(cmd) ?? 0;
        perCmd.set(cmd, idx + 1);
        calls.push({ cmd, args });
        const handler = handlers[cmd];
        if (handler) return handler(args, idx) as T;
        return undefined as T;
    };
    return { invoke, calls };
};

const noWaitDeps = (
    invoke: RemoteSyncDeps['invoke'],
    extra: Partial<RemoteSyncDeps> = {},
): RemoteSyncDeps => ({
    invoke,
    delay: async () => undefined,
    now: () => 0,
    makeId: () => 'test-journal-id',
    ...extra,
});

const file = (
    relativePath: string,
    action: SyncRunFile['action'],
    over: Partial<SyncRunFile> = {},
): SyncRunFile => ({
    relativePath,
    action,
    size: 1024,
    mtime: '2026-05-22T10:00:00Z',
    ...over,
});

describe('remoteSyncRunner — copy legs', () => {
    it('uploads and downloads, counting bytes and dirs', async () => {
        const { invoke, calls } = makeInvoke();
        const report = await runRemoteSync(
            [
                file('a.txt', 'upload', { size: 100 }),
                file('docs/b.txt', 'download', { size: 200 }),
            ],
            { remote: ['newdir'], local: [] },
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );

        expect(report.uploaded).toBe(1);
        expect(report.downloaded).toBe(1);
        expect(report.totalBytes).toBe(300);
        expect(report.dirsCreated).toBe(1);
        expect(report.errors).toHaveLength(0);
        expect(report.cancelled).toBe(false);

        // Parent dir of docs/b.txt is pre-created locally.
        expect(calls.some((c) => c.cmd === 'create_local_folder'
            && c.args?.path === '/home/u/work/docs')).toBe(true);
        // Standalone remote dir created.
        expect(calls.some((c) => c.cmd === 'create_remote_folder'
            && c.args?.path === '/srv/data/newdir')).toBe(true);
        // Journal lifecycle: written then deleted on clean completion.
        expect(calls.some((c) => c.cmd === 'save_sync_journal_cmd')).toBe(true);
        expect(calls.some((c) => c.cmd === 'delete_sync_journal_cmd')).toBe(true);
        expect(calls.some((c) => c.cmd === 'reset_cancel_flag')).toBe(true);
    });

    it('routes provider commands when isProvider is set', async () => {
        const { invoke, calls } = makeInvoke();
        await runRemoteSync(
            [file('a.txt', 'upload')],
            noDirs,
            baseConfig({ isProvider: true }),
            {},
            noWaitDeps(invoke),
        );
        expect(calls.some((c) => c.cmd === 'provider_upload_file')).toBe(true);
        expect(calls.some((c) => c.cmd === 'upload_file')).toBe(false);
    });
});

describe('remoteSyncRunner — orphan deletes', () => {
    it('deletes a remote orphan', async () => {
        const { invoke, calls } = makeInvoke();
        const report = await runRemoteSync(
            [file('stale.txt', 'delete-remote')],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        expect(report.deleted).toBe(1);
        expect(calls.some((c) => c.cmd === 'delete_remote_file'
            && c.args?.path === '/srv/data/stale.txt')).toBe(true);
    });

    it('archives a local file before deleting it when versioning is on', async () => {
        const { invoke, calls } = makeInvoke();
        const report = await runRemoteSync(
            [file('old.txt', 'delete-local')],
            noDirs,
            baseConfig({ versioningStrategy: 'trash' }),
            {},
            noWaitDeps(invoke),
        );
        expect(report.deleted).toBe(1);
        const archiveIdx = calls.findIndex((c) => c.cmd === 'archive_before_sync_delete');
        const deleteIdx = calls.findIndex((c) => c.cmd === 'delete_local_file');
        expect(archiveIdx).toBeGreaterThanOrEqual(0);
        expect(deleteIdx).toBeGreaterThan(archiveIdx);
    });

    it('passes the isDir hint and skips archiving for directory deletes', async () => {
        const { invoke, calls } = makeInvoke();
        await runRemoteSync(
            [file('stale-dir', 'delete-remote', { isDir: true })],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        const del = calls.find((c) => c.cmd === 'delete_remote_file');
        expect(del?.args).toEqual({ path: '/srv/data/stale-dir', isDir: true });

        const { invoke: invoke2, calls: calls2 } = makeInvoke();
        await runRemoteSync(
            [file('old-dir', 'delete-local', { isDir: true })],
            noDirs,
            baseConfig({ versioningStrategy: 'trash' }),
            {},
            noWaitDeps(invoke2),
        );
        expect(calls2.some((c) => c.cmd === 'archive_before_sync_delete')).toBe(false);
        expect(calls2.some((c) => c.cmd === 'delete_local_file')).toBe(true);
    });

    it('skips archiving when no versioning strategy is configured', async () => {
        const { invoke, calls } = makeInvoke();
        await runRemoteSync(
            [file('old.txt', 'delete-local')],
            noDirs,
            baseConfig({ versioningStrategy: 'disabled' }),
            {},
            noWaitDeps(invoke),
        );
        expect(calls.some((c) => c.cmd === 'archive_before_sync_delete')).toBe(false);
    });
});

describe('remoteSyncRunner — retry with backoff', () => {
    it('retries a retryable failure then succeeds', async () => {
        const delays: number[] = [];
        const { invoke } = makeInvoke({
            upload_file: (_args, idx) => {
                if (idx < 2) throw new Error('network glitch');
                return undefined;
            },
            classify_transfer_error: (args) => ({
                kind: 'network',
                message: String(args?.rawError),
                retryable: true,
                file_path: String(args?.filePath),
            } satisfies SyncErrorInfo),
        });
        const report = await runRemoteSync(
            [file('a.txt', 'upload')],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, { delay: async (ms) => { delays.push(ms); } }),
        );
        expect(report.uploaded).toBe(1);
        expect(report.retried).toBe(1);
        expect(report.errors).toHaveLength(0);
        // Exponential backoff: 500ms then 1000ms.
        expect(delays).toEqual([500, 1000]);
    });

    it('gives up on a non-retryable error and records it', async () => {
        const { invoke } = makeInvoke({
            upload_file: () => { throw new Error('permission denied'); },
            classify_transfer_error: (args) => ({
                kind: 'permission_denied',
                message: String(args?.rawError),
                retryable: false,
                file_path: String(args?.filePath),
            } satisfies SyncErrorInfo),
        });
        const report = await runRemoteSync(
            [file('a.txt', 'upload')],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        expect(report.uploaded).toBe(0);
        expect(report.errors).toHaveLength(1);
        expect(report.errors[0].kind).toBe('permission_denied');
        expect(report.retried).toBe(0);
    });
});

describe('remoteSyncRunner — verify policy', () => {
    it('flags a download whose verification fails', async () => {
        const statuses: Array<[string, string]> = [];
        const { invoke } = makeInvoke({
            verify_local_transfer: (): VerifyResult => ({
                path: '/home/u/work/a.txt',
                passed: false,
                policy: 'size_only',
                expected_size: 100,
                actual_size: 40,
                size_match: false,
                mtime_match: null,
                hash_match: null,
                message: 'size mismatch',
            }),
        });
        const report = await runRemoteSync(
            [file('a.txt', 'download', { size: 100 })],
            noDirs,
            baseConfig({ verifyPolicy: 'size_only' }),
            { onFileStatus: (p, s) => statuses.push([p, s]) },
            noWaitDeps(invoke),
        );
        expect(report.downloaded).toBe(0);
        expect(report.verifyFailed).toBe(1);
        expect(report.errors).toHaveLength(1);
        expect(statuses.some(([, s]) => s === 'verify_failed')).toBe(true);
    });

    it('does not call the verifier when policy is none', async () => {
        const { invoke, calls } = makeInvoke();
        await runRemoteSync(
            [file('a.txt', 'download')],
            noDirs,
            baseConfig({ verifyPolicy: 'none' }),
            {},
            noWaitDeps(invoke),
        );
        expect(calls.some((c) => c.cmd === 'verify_local_transfer')).toBe(false);
    });
});

describe('remoteSyncRunner — cancellation and budget', () => {
    it('stops on cancel and leaves the journal undeleted', async () => {
        let seen = 0;
        const { invoke, calls } = makeInvoke();
        const report = await runRemoteSync(
            [file('a.txt', 'upload'), file('b.txt', 'upload'), file('c.txt', 'upload')],
            noDirs,
            baseConfig(),
            { isCancelled: () => { seen += 1; return seen > 1; } },
            noWaitDeps(invoke),
        );
        expect(report.uploaded).toBe(1);
        expect(report.skipped).toBe(2);
        expect(report.cancelled).toBe(true);
        // Interrupted run keeps its journal for a later resume.
        expect(calls.some((c) => c.cmd === 'delete_sync_journal_cmd')).toBe(false);
    });

    it('halts once the transfer budget is exhausted', async () => {
        const { invoke } = makeInvoke();
        const report = await runRemoteSync(
            [
                file('a.txt', 'upload', { size: 600 }),
                file('b.txt', 'upload', { size: 600 }),
                file('c.txt', 'upload', { size: 600 }),
            ],
            noDirs,
            baseConfig({ transferBudget: 1000 }),
            {},
            noWaitDeps(invoke),
        );
        expect(report.uploaded).toBe(2);
        expect(report.skipped).toBe(1);
        expect(report.cancelled).toBe(true);
    });
});

describe('remoteSyncRunner — journal resume', () => {
    it('skips entries the resumed journal already completed', async () => {
        const resumeJournal: SyncJournal = {
            id: 'j1',
            created_at: '2026-05-22T09:00:00Z',
            updated_at: '2026-05-22T09:05:00Z',
            local_path: '/home/u/work',
            remote_path: '/srv/data',
            direction: 'bidirectional',
            retry_policy: RETRY,
            verify_policy: 'none',
            entries: [
                { relative_path: 'a.txt', action: 'upload', status: 'completed', attempts: 1, last_error: null, verified: true, bytes_transferred: 100 },
                { relative_path: 'b.txt', action: 'upload', status: 'pending', attempts: 0, last_error: null, verified: null, bytes_transferred: 0 },
            ],
            completed: false,
        };
        const { invoke, calls } = makeInvoke();
        const report = await runRemoteSync(
            [file('a.txt', 'upload', { size: 100 }), file('b.txt', 'upload', { size: 200 })],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, { resumeJournal }),
        );
        expect(report.uploaded).toBe(2); // 1 resumed + 1 fresh
        expect(report.totalBytes).toBe(300);
        // Only b.txt is re-uploaded; a.txt was already done.
        const uploadCalls = calls.filter((c) => c.cmd === 'upload_file');
        expect(uploadCalls).toHaveLength(1);
        expect((uploadCalls[0].args?.params as { local_path: string }).local_path)
            .toBe('/home/u/work/b.txt');
    });
});

describe('remoteSyncRunner — delta savings', () => {
    it('aggregates per-file delta stats into the report', async () => {
        const { invoke } = makeInvoke();
        const deltaStats = new Map([
            ['a.txt', { bytes_sent: 20, total_size: 100, speedup: 5 }],
            ['b.txt', { bytes_sent: 30, total_size: 300, speedup: 10 }],
        ]);
        const report = await runRemoteSync(
            [file('a.txt', 'upload', { size: 100 }), file('b.txt', 'upload', { size: 300 })],
            noDirs,
            baseConfig({ deltaSyncEnabled: true }),
            {},
            noWaitDeps(invoke, { deltaStats }),
        );
        expect(report.delta_savings).toBeDefined();
        expect(report.delta_savings?.files_using_delta).toBe(2);
        expect(report.delta_savings?.total_bytes_sent).toBe(50);
        expect(report.delta_savings?.bytes_saved).toBe(350);
        expect(report.delta_bytes_on_wire).toBe(50);
    });

    it('omits delta_savings when no file used the delta path', async () => {
        const { invoke } = makeInvoke();
        const report = await runRemoteSync(
            [file('a.txt', 'upload')],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, { deltaStats: new Map() }),
        );
        expect(report.delta_savings).toBeUndefined();
    });
});

describe('remoteSyncRunner — progress + bandwidth', () => {
    it('reports progress from 0 to total', async () => {
        const { invoke } = makeInvoke();
        const progress: Array<[number, number]> = [];
        await runRemoteSync(
            [file('a.txt', 'upload'), file('b.txt', 'upload')],
            noDirs,
            baseConfig(),
            { onProgress: (c, t) => progress.push([c, t]) },
            noWaitDeps(invoke),
        );
        expect(progress[0]).toEqual([0, 2]);
        expect(progress[progress.length - 1]).toEqual([2, 2]);
    });

    it('applies bandwidth caps before transferring', async () => {
        const { invoke, calls } = makeInvoke();
        await runRemoteSync(
            [file('a.txt', 'upload')],
            noDirs,
            baseConfig({ isFtp: true, uploadLimitKbps: 256, downloadLimitKbps: 512 }),
            {},
            noWaitDeps(invoke),
        );
        const limitCall = calls.find((c) => c.cmd === 'set_speed_limit');
        expect(limitCall?.args).toEqual({ downloadKb: 512, uploadKb: 256 });
    });
});

describe('remoteSyncRunner — helpers', () => {
    it('groupErrorsByKind buckets errors by kind', () => {
        const errors: SyncErrorInfo[] = [
            { kind: 'network', message: 'a', retryable: true, file_path: 'a' },
            { kind: 'network', message: 'b', retryable: true, file_path: 'b' },
            { kind: 'auth', message: 'c', retryable: false, file_path: 'c' },
        ];
        const grouped = groupErrorsByKind(errors);
        expect(grouped.get('network')).toHaveLength(2);
        expect(grouped.get('auth')).toHaveLength(1);
    });

    it('filesFromJournal reconstructs upload/download entries only', () => {
        const journal: SyncJournal = {
            id: 'j',
            created_at: '', updated_at: '',
            local_path: '', remote_path: '',
            direction: 'bidirectional',
            retry_policy: RETRY, verify_policy: 'none',
            entries: [
                { relative_path: 'a', action: 'upload', status: 'pending', attempts: 0, last_error: null, verified: null, bytes_transferred: 0 },
                { relative_path: 'b', action: 'download', status: 'pending', attempts: 0, last_error: null, verified: null, bytes_transferred: 0 },
                { relative_path: 'c', action: 'delete', status: 'pending', attempts: 0, last_error: null, verified: null, bytes_transferred: 0 },
            ],
            completed: false,
        };
        const files = filesFromJournal(journal);
        expect(files).toHaveLength(2);
        expect(files.map((f) => f.action)).toEqual(['upload', 'download']);
        expect(files[1].overwritesExisting).toBe(true);
    });
});

describe('remoteSyncRunner — GAP-6 sync index', () => {
    it('does not touch the index when writeIndex is unset', async () => {
        const { invoke, calls } = makeInvoke();
        await runRemoteSync([file('a.txt', 'upload')], noDirs, baseConfig(), {}, noWaitDeps(invoke));
        expect(calls.some((c) => c.cmd === 'save_sync_index_cmd')).toBe(false);
    });

    it('merges synced files into the index and drops successful deletes', async () => {
        let savedIndex: Record<string, unknown> | undefined;
        const { invoke } = makeInvoke({
            load_sync_index_cmd: () => ({
                version: 1,
                last_sync: 'old',
                local_path: '/home/u/work',
                remote_path: '/srv/data',
                files: { 'stale.txt': { size: 1, modified: null, is_dir: false } },
            }),
            save_sync_index_cmd: (args) => {
                savedIndex = args?.index as Record<string, unknown>;
            },
        });
        await runRemoteSync(
            [
                file('docs/up.txt', 'upload', { size: 200 }),
                file('stale.txt', 'delete-remote'),
            ],
            { remote: ['emptydir'], local: [] },
            baseConfig(),
            {},
            noWaitDeps(invoke, { writeIndex: true }),
        );
        const files = (savedIndex?.files ?? {}) as Record<string, { is_dir: boolean; size: number }>;
        // Uploaded nested file recorded with its size.
        expect(files['docs/up.txt']).toMatchObject({ size: 200, is_dir: false });
        // The deleted file is dropped from the index.
        expect(files['stale.txt']).toBeUndefined();
        // Standalone directory recorded as a directory.
        expect(files['emptydir']).toMatchObject({ is_dir: true });
    });
});

describe('remoteSyncRunner — GAP-7 keep-both rename', () => {
    it('uploads from sourcePath and writes the suffixed relativePath', async () => {
        const { invoke, calls } = makeInvoke();
        await runRemoteSync(
            [
                file('report.txt.20260522T143012.bak', 'upload', {
                    sourcePath: 'report.txt',
                }),
            ],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        const upload = calls.find((c) => c.cmd === 'upload_file');
        const params = upload?.args?.params as { local_path: string; remote_path: string };
        // Source read from the original path, destination is the suffixed name.
        expect(params.local_path).toBe('/home/u/work/report.txt');
        expect(params.remote_path).toBe('/srv/data/report.txt.20260522T143012.bak');
    });

    it('downloads a rename from the remote source to the suffixed local path', async () => {
        const { invoke, calls } = makeInvoke();
        await runRemoteSync(
            [
                file('notes.md.TS.bak', 'download', { sourcePath: 'notes.md' }),
            ],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        const download = calls.find((c) => c.cmd === 'download_file');
        const params = download?.args?.params as { local_path: string; remote_path: string };
        expect(params.remote_path).toBe('/srv/data/notes.md');
        expect(params.local_path).toBe('/home/u/work/notes.md.TS.bak');
    });
});

describe('remoteSyncRunner — GAP-8 retryPolicyForSpeed', () => {
    it('keeps the conservative 3-retry default for normal and fast', () => {
        expect(retryPolicyForSpeed('normal').max_retries).toBe(3);
        expect(retryPolicyForSpeed('fast').max_retries).toBe(3);
    });

    it('pushes harder with shorter backoff for turbo and extreme', () => {
        expect(retryPolicyForSpeed('turbo').max_retries).toBe(4);
        const extreme = retryPolicyForSpeed('extreme');
        expect(extreme.max_retries).toBe(5);
        expect(extreme.base_delay_ms).toBeLessThan(retryPolicyForSpeed('normal').base_delay_ms);
    });

    it('falls back to the default policy for an unknown mode', () => {
        expect(retryPolicyForSpeed('whatever').max_retries).toBe(3);
    });

    it('GAP-9a — maniac uses 2 retries with a tight backoff and long timeout', () => {
        const maniac = retryPolicyForSpeed('maniac');
        expect(maniac.max_retries).toBe(2);
        expect(maniac.base_delay_ms).toBe(250);
        expect(maniac.max_delay_ms).toBe(2_000);
        expect(maniac.timeout_ms).toBe(300_000);
        expect(maniac.backoff_multiplier).toBe(1.5);
    });
});

describe('remoteSyncRunner — GAP-9a maniac mode', () => {
    it('skips journal persistence when journalEnabled is false', async () => {
        const { invoke, calls } = makeInvoke();
        await runRemoteSync(
            [file('a.txt', 'upload')],
            noDirs,
            baseConfig({ journalEnabled: false }),
            {},
            noWaitDeps(invoke),
        );
        expect(calls.some((c) => c.cmd === 'save_sync_journal_cmd')).toBe(false);
        expect(calls.some((c) => c.cmd === 'delete_sync_journal_cmd')).toBe(false);
    });

    it('still persists the journal when journalEnabled is left default', async () => {
        const { invoke, calls } = makeInvoke();
        await runRemoteSync(
            [file('a.txt', 'upload')],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        expect(calls.some((c) => c.cmd === 'save_sync_journal_cmd')).toBe(true);
    });

    it('runs a post-sync verification sweep over completed downloads', async () => {
        const verifyCalls: Record<string, unknown>[] = [];
        const { invoke } = makeInvoke({
            verify_local_transfer: (args) => {
                verifyCalls.push(args ?? {});
                const passed = (args?.localPath as string).includes('good');
                return { passed, message: passed ? 'ok' : 'size mismatch' } as VerifyResult;
            },
        });
        const report = await runRemoteSync(
            [
                file('good.txt', 'download'),
                file('bad.txt', 'download'),
                file('up.txt', 'upload'),
            ],
            noDirs,
            baseConfig({ journalEnabled: false, postSyncVerification: true }),
            {},
            noWaitDeps(invoke),
        );
        // Only the two downloads are swept; the upload is skipped.
        expect(verifyCalls).toHaveLength(2);
        expect(verifyCalls.every((a) => a.policy === 'size_and_mtime')).toBe(true);
        expect(report.postSyncVerification).toEqual({ ok: 1, mismatches: 1, failed: 0 });
    });

    it('omits the post-sync report when postSyncVerification is unset', async () => {
        const { invoke } = makeInvoke();
        const report = await runRemoteSync(
            [file('a.txt', 'download')],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        expect(report.postSyncVerification).toBeUndefined();
    });
});

describe('remoteSyncRunner — GAP-9b threaded transfer tuning', () => {
    it('accepts parallelStreams + compressionMode config without altering the sequential run', async () => {
        const { invoke, calls } = makeInvoke();
        const report = await runRemoteSync(
            [file('a.txt', 'upload'), file('b.txt', 'upload')],
            noDirs,
            baseConfig({ parallelStreams: 8, compressionMode: 'on' }),
            {},
            noWaitDeps(invoke),
        );
        // The run still completes both entries one at a time: the threaded
        // tuning is config-only until APPENDIX-DAG-ENGINE Fase 2 consumes it.
        expect(report.uploaded).toBe(2);
        const uploads = calls.filter((c) => c.cmd === 'upload_file');
        expect(uploads).toHaveLength(2);
    });
});

describe('remoteSyncRunner — GAP-10 local-local mode', () => {
    const localLocalConfig = (over: Partial<RemoteSyncConfig> = {}): RemoteSyncConfig =>
        baseConfig({ isLocalLocal: true, ...over });

    it('routes upload and download through copy_local_file between the two roots', async () => {
        const { invoke, calls } = makeInvoke();
        const report = await runRemoteSync(
            [
                file('up.txt', 'upload', { size: 100 }),
                file('docs/down.txt', 'download', { size: 200 }),
            ],
            noDirs,
            localLocalConfig(),
            {},
            noWaitDeps(invoke),
        );

        expect(report.uploaded).toBe(1);
        expect(report.downloaded).toBe(1);
        expect(report.totalBytes).toBe(300);
        expect(report.errors).toHaveLength(0);

        // No protocol transfer commands at all.
        expect(calls.some((c) => c.cmd === 'upload_file')).toBe(false);
        expect(calls.some((c) => c.cmd === 'download_file')).toBe(false);
        expect(calls.some((c) => c.cmd === 'provider_upload_file')).toBe(false);

        // upload = copy left → right.
        expect(calls.some((c) => c.cmd === 'copy_local_file'
            && c.args?.from === '/home/u/work/up.txt'
            && c.args?.to === '/srv/data/up.txt')).toBe(true);
        // download = copy right → left.
        expect(calls.some((c) => c.cmd === 'copy_local_file'
            && c.args?.from === '/srv/data/docs/down.txt'
            && c.args?.to === '/home/u/work/docs/down.txt')).toBe(true);
    });

    it('creates the right-side parent directory with create_local_folder', async () => {
        const { invoke, calls } = makeInvoke();
        await runRemoteSync(
            [file('nested/deep/f.txt', 'upload')],
            { remote: ['emptydir'], local: [] },
            localLocalConfig(),
            {},
            noWaitDeps(invoke),
        );
        // Never the remote/provider mkdir commands.
        expect(calls.some((c) => c.cmd === 'create_remote_folder')).toBe(false);
        expect(calls.some((c) => c.cmd === 'provider_mkdir')).toBe(false);
        // Parent dirs of the upload land on the right root via create_local_folder.
        expect(calls.some((c) => c.cmd === 'create_local_folder'
            && c.args?.path === '/srv/data/nested')).toBe(true);
        expect(calls.some((c) => c.cmd === 'create_local_folder'
            && c.args?.path === '/srv/data/nested/deep')).toBe(true);
        // Standalone dir too.
        expect(calls.some((c) => c.cmd === 'create_local_folder'
            && c.args?.path === '/srv/data/emptydir')).toBe(true);
    });

    it('deletes a right-side orphan with delete_local_file', async () => {
        const { invoke, calls } = makeInvoke();
        const report = await runRemoteSync(
            [file('stale.txt', 'delete-remote')],
            noDirs,
            localLocalConfig(),
            {},
            noWaitDeps(invoke),
        );
        expect(report.deleted).toBe(1);
        expect(calls.some((c) => c.cmd === 'delete_remote_file')).toBe(false);
        expect(calls.some((c) => c.cmd === 'delete_local_file'
            && c.args?.path === '/srv/data/stale.txt')).toBe(true);
    });

    it('never issues bandwidth-cap commands for a local-local run', async () => {
        const { invoke, calls } = makeInvoke();
        await runRemoteSync(
            [file('a.txt', 'upload')],
            noDirs,
            localLocalConfig({ uploadLimitKbps: 512, downloadLimitKbps: 512 }),
            {},
            noWaitDeps(invoke),
        );
        expect(calls.some((c) => c.cmd === 'set_speed_limit')).toBe(false);
        expect(calls.some((c) => c.cmd === 'provider_set_speed_limit')).toBe(false);
    });

    it('verifies a local-local download against the left-side copy', async () => {
        const verifyResult: VerifyResult = {
            path: '/home/u/work/v.txt',
            passed: true,
            policy: 'size_and_mtime',
            expected_size: 64,
            actual_size: 64,
            size_match: true,
            mtime_match: true,
            hash_match: null,
            message: 'ok',
        };
        const { invoke, calls } = makeInvoke({
            verify_local_transfer: () => verifyResult,
        });
        const report = await runRemoteSync(
            [file('v.txt', 'download', { size: 64 })],
            noDirs,
            localLocalConfig({ verifyPolicy: 'size_and_mtime' }),
            {},
            noWaitDeps(invoke),
        );
        expect(report.downloaded).toBe(1);
        expect(report.verifyFailed).toBe(0);
        expect(calls.some((c) => c.cmd === 'verify_local_transfer'
            && c.args?.localPath === '/home/u/work/v.txt')).toBe(true);
    });
});
