// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import {
    readEmptyDirAnswer,
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
    versionedBackup: null,
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

const lastSavedJournal = (calls: Call[]): SyncJournal | undefined => {
    for (let i = calls.length - 1; i >= 0; i--) {
        if (calls[i].cmd === 'save_sync_journal_cmd') {
            return calls[i].args?.journal as SyncJournal | undefined;
        }
    }
    return undefined;
};

const denyUpload: Record<string, Handler> = {
    upload_file: () => {
        throw new Error('permission denied');
    },
    classify_transfer_error: (args) => ({
        kind: 'permission_denied',
        message: String(args?.rawError),
        retryable: false,
        file_path: String(args?.filePath),
    } satisfies SyncErrorInfo),
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
        // Two folders are created: the `docs` parent dir for docs/b.txt AND the
        // standalone `newdir`. Parent dirs of transferred files now count too, so
        // the receipt reconciles with the compare's directory-inclusive count.
        expect(report.dirsCreated).toBe(2);
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
        expect(lastSavedJournal(calls)?.completed).toBe(true);
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

describe('remoteSyncRunner — error correction sidecars', () => {
    it('generates a sidecar after a successful upload without changing transfer success', async () => {
        const { invoke, calls } = makeInvoke({
            sync_ec_generate: () => ({ status: 'generated' }),
        });
        const report = await runRemoteSync(
            [file('a.txt', 'upload')],
            noDirs,
            baseConfig({ errorCorrection: { enabled: true, pct: 20 } }),
            {},
            noWaitDeps(invoke),
        );

        expect(report.uploaded).toBe(1);
        expect(report.ec_generated).toBe(1);
        const ecCall = calls.find((c) => c.cmd === 'sync_ec_generate');
        expect(ecCall?.args).toMatchObject({
            localPath: '/home/u/work/a.txt',
            remotePath: '/srv/data/a.txt',
            relativePath: 'a.txt',
            pct: 20,
            isProvider: false,
        });
    });

    it('runs verify/repair after a successful download when SHA-256 is available', async () => {
        const { invoke, calls } = makeInvoke({
            sync_ec_verify_repair: () => ({ status: 'repaired' }),
        });
        const report = await runRemoteSync(
            [file('b.txt', 'download', { expectedSha256: 'a'.repeat(64) })],
            noDirs,
            baseConfig({ errorCorrection: { enabled: true, pct: 20 } }),
            {},
            noWaitDeps(invoke),
        );

        expect(report.downloaded).toBe(1);
        expect(report.ec_repaired).toBe(1);
        const ecCall = calls.find((c) => c.cmd === 'sync_ec_verify_repair');
        expect(ecCall?.args).toMatchObject({
            localPath: '/home/u/work/b.txt',
            remotePath: '/srv/data/b.txt',
            relativePath: 'b.txt',
            expectedSha256: 'a'.repeat(64),
            expectedMtime: '2026-05-22T10:00:00Z',
            isProvider: false,
        });
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
});

describe('remoteSyncRunner: versioned backup', () => {
    const backup = { versionedBackup: { dir: '.aeroftp-versions' } };
    const kept = (args: Record<string, unknown> | undefined) =>
        `${String(args?.root)}/${String(args?.dir)}/${String(args?.stamp)}/${String(args?.rel)}`;
    const stampedInvoke = (handlers: Record<string, (args: Record<string, unknown> | undefined, idx: number) => unknown> = {}) =>
        makeInvoke({
            sync_backup_run_stamp: () => '20260925T070000Z',
            sync_backup_archive_local: kept,
            sync_backup_archive_remote: kept,
            ...handlers,
        });
    const idx = (calls: { cmd: string }[], cmd: string) => calls.findIndex((c) => c.cmd === cmd);

    it('moves the remote copy aside before an upload overwrites it', async () => {
        const { invoke, calls } = stampedInvoke();
        const report = await runRemoteSync(
            [file('docs/a.txt', 'upload', { overwritesExisting: true })],
            noDirs,
            baseConfig(backup),
            {},
            noWaitDeps(invoke),
        );
        expect(report.uploaded).toBe(1);
        const archive = calls.find((c) => c.cmd === 'sync_backup_archive_remote');
        expect(archive?.args).toEqual({
            useProvider: false,
            root: '/srv/data',
            dir: '.aeroftp-versions',
            stamp: '20260925T070000Z',
            rel: 'docs/a.txt',
        });
        expect(idx(calls, 'sync_backup_archive_remote')).toBeLessThan(idx(calls, 'upload_file'));
    });

    it('moves the local copy aside before a download overwrites it', async () => {
        const { invoke, calls } = stampedInvoke();
        await runRemoteSync(
            [file('b.txt', 'download', { overwritesExisting: true })],
            noDirs,
            baseConfig(backup),
            {},
            noWaitDeps(invoke),
        );
        const archive = calls.find((c) => c.cmd === 'sync_backup_archive_local');
        expect(archive?.args).toMatchObject({ root: '/home/u/work', rel: 'b.txt' });
        expect(idx(calls, 'sync_backup_archive_local')).toBeLessThan(idx(calls, 'download_file'));
    });

    it('does not archive a new file or a keep-both copy', async () => {
        const { invoke, calls } = stampedInvoke();
        await runRemoteSync(
            [file('new.txt', 'upload'), file('n.txt', 'download', { overwritesExisting: false })],
            noDirs,
            baseConfig(backup),
            {},
            noWaitDeps(invoke),
        );
        expect(calls.some((c) => c.cmd.startsWith('sync_backup_'))).toBe(false);
    });

    it('turns a file delete into the move, on either side, and leaves folders to the empty-folder removal', async () => {
        const { invoke, calls } = stampedInvoke({ sync_remove_empty_dir: () => 'removed' });
        const report = await runRemoteSync(
            [
                file('gone.txt', 'delete-remote'),
                file('old.txt', 'delete-local'),
                file('empty-dir', 'delete-remote', { isDir: true }),
            ],
            noDirs,
            baseConfig({ ...backup, isProvider: true }),
            {},
            noWaitDeps(invoke),
        );
        expect(report.deleted).toBe(3);
        expect(calls.find((c) => c.cmd === 'sync_backup_archive_remote')?.args)
            .toMatchObject({ useProvider: true, rel: 'gone.txt' });
        expect(calls.find((c) => c.cmd === 'sync_backup_archive_local')?.args)
            .toMatchObject({ root: '/home/u/work', rel: 'old.txt' });
        // The move replaced both file deletes; the folder is still removed,
        // and only as an empty folder.
        expect(calls.some((c) => c.cmd === 'provider_delete_file' || c.cmd === 'delete_local_file')).toBe(false);
        expect(calls.filter((c) => c.cmd === 'sync_remove_empty_dir').map((c) => c.args))
            .toEqual([{ target: 'provider', path: '/srv/data/empty-dir' }]);
    });

    it('asks for one stamp per run however many copies it keeps', async () => {
        const { invoke, calls } = stampedInvoke();
        await runRemoteSync(
            [file('a.txt', 'delete-remote'), file('b.txt', 'delete-remote'), file('c.txt', 'upload', { overwritesExisting: true })],
            noDirs,
            baseConfig(backup),
            {},
            noWaitDeps(invoke),
        );
        expect(calls.filter((c) => c.cmd === 'sync_backup_run_stamp')).toHaveLength(1);
        const stamps = new Set(calls.filter((c) => c.cmd === 'sync_backup_archive_remote').map((c) => c.args?.stamp));
        expect([...stamps]).toEqual(['20260925T070000Z']);
    });

    it('fails a file whose backup fails without writing or deleting it, and goes on', async () => {
        const { invoke, calls } = stampedInvoke({
            sync_backup_archive_remote: (args) => {
                if (args?.rel === 'locked.txt') throw new Error('versioned backup needs to move files');
                return '/srv/data/.aeroftp-versions/x';
            },
        });
        const report = await runRemoteSync(
            [
                file('locked.txt', 'upload', { overwritesExisting: true }),
                file('locked.txt', 'delete-remote'),
                file('fine.txt', 'upload', { overwritesExisting: true }),
            ],
            noDirs,
            baseConfig(backup),
            {},
            noWaitDeps(invoke),
        );
        expect(report.errors.map((e) => e.file_path)).toEqual(['locked.txt', 'locked.txt']);
        const uploads = calls.filter((c) => c.cmd === 'upload_file');
        expect(uploads).toHaveLength(1);
        expect((uploads[0].args as { params: { remote_path: string } }).params.remote_path).toBe('/srv/data/fine.txt');
        expect(calls.some((c) => c.cmd === 'delete_remote_file')).toBe(false);
    });

    it('fails a file whose destination copy the backup cannot find, instead of writing over it', async () => {
        const { invoke, calls } = stampedInvoke({ sync_backup_archive_remote: () => null });
        const report = await runRemoteSync(
            [file('a.txt', 'upload', { overwritesExisting: true }), file('b.txt', 'delete-remote')],
            noDirs,
            baseConfig(backup),
            {},
            noWaitDeps(invoke),
        );
        expect(report.errors.map((e) => e.file_path)).toEqual(['a.txt', 'b.txt']);
        expect(report.errors[0].message).toContain('was not found');
        expect(calls.some((c) => c.cmd === 'upload_file' || c.cmd === 'delete_remote_file')).toBe(false);
        expect(report.deleted).toBe(0);
    });

    it('archives the right-hand folder on the local disk for a local-local pair', async () => {
        const { invoke, calls } = stampedInvoke();
        await runRemoteSync(
            [file('r.txt', 'upload', { overwritesExisting: true })],
            noDirs,
            baseConfig({ ...backup, isLocalLocal: true }),
            {},
            noWaitDeps(invoke),
        );
        expect(calls.find((c) => c.cmd === 'sync_backup_archive_local')?.args)
            .toMatchObject({ root: '/srv/data', rel: 'r.txt' });
        expect(calls.some((c) => c.cmd === 'sync_backup_archive_remote')).toBe(false);
    });

    it('keeps the .aerocorrect sidecar with the copy it protects', async () => {
        // The move replaced the delete and skipped the sidecar cleanup, so a
        // sidecar stayed beside a file that was gone; the compare never lists
        // sidecars, so no later run removed it.
        const { invoke, calls } = stampedInvoke();
        const report = await runRemoteSync(
            [file('gone.txt', 'delete-remote'), file('c.txt', 'upload', { overwritesExisting: true })],
            noDirs,
            baseConfig(backup),
            {},
            noWaitDeps(invoke),
        );
        expect(report.errors).toEqual([]);
        expect(calls.filter((c) => c.cmd === 'sync_backup_archive_remote').map((c) => c.args?.rel)).toEqual([
            'gone.txt',
            'gone.txt.aerocorrect',
            'c.txt',
            'c.txt.aerocorrect',
        ]);
        expect(calls.some((c) => c.cmd === 'delete_remote_file')).toBe(false);
    });

    it('deletes a sidecar it could not move, and still counts the file', async () => {
        const { invoke, calls } = stampedInvoke({
            sync_backup_archive_remote: (args) => {
                if (String(args?.rel).endsWith('.aerocorrect')) throw new Error('move refused');
                return kept(args);
            },
        });
        const report = await runRemoteSync(
            [file('gone.txt', 'delete-remote')],
            noDirs,
            baseConfig(backup),
            {},
            noWaitDeps(invoke),
        );
        expect(report.errors).toEqual([]);
        expect(report.deleted).toBe(1);
        expect(calls.find((c) => c.cmd === 'delete_remote_file')?.args)
            .toEqual({ path: '/srv/data/gone.txt.aerocorrect', isDir: false });
    });

    it('does not archive or delete a sidecar that is not there', async () => {
        const { invoke, calls } = stampedInvoke({
            sync_backup_archive_remote: (args) =>
                String(args?.rel).endsWith('.aerocorrect') ? null : kept(args),
        });
        const report = await runRemoteSync(
            [file('gone.txt', 'delete-remote')],
            noDirs,
            baseConfig(backup),
            {},
            noWaitDeps(invoke),
        );
        expect(report.errors).toEqual([]);
        expect(calls.some((c) => c.cmd === 'delete_remote_file')).toBe(false);
    });

    it('does nothing of the kind when versioned backup is off', async () => {
        const { invoke, calls } = stampedInvoke();
        await runRemoteSync(
            [file('a.txt', 'upload', { overwritesExisting: true }), file('b.txt', 'delete-local')],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        expect(calls.some((c) => c.cmd.startsWith('sync_backup_'))).toBe(false);
        expect(calls.some((c) => c.cmd === 'delete_local_file')).toBe(true);
    });
});

/**
 * A folder row is removed only once it is empty, and never when something
 * under it stayed this run (B2, H6 of the 4.2.1 pre-release review). The
 * recursive delete used to take along a file whose move into the backup
 * folder had failed, and a file the compare excluded (`docs/.env`), which has
 * no row of its own.
 */
describe('remoteSyncRunner: a folder row never takes along what stayed', () => {
    const backup = { versionedBackup: { dir: '.aeroftp-versions' } };
    /** Every call that could remove the folder at `path`, whatever the command. */
    const folderRemovals = (calls: Call[], path: string) =>
        calls.filter((c) =>
            ['delete_remote_file', 'delete_local_file', 'provider_delete_file', 'provider_delete_dir', 'sync_remove_empty_dir']
                .includes(c.cmd) && c.args?.path === path);

    it('keeps the folder of a file whose backup failed, and removes nothing of it (B2)', async () => {
        const { invoke, calls } = makeInvoke({
            sync_backup_run_stamp: () => '20260929T070000Z',
            sync_backup_archive_remote: () => {
                throw new Error('move refused');
            },
            sync_remove_empty_dir: () => 'removed',
        });
        const statuses = new Map<string, string>();
        const report = await runRemoteSync(
            [file('docs/only-copy.txt', 'delete-remote'), file('docs', 'delete-remote', { isDir: true })],
            noDirs,
            baseConfig(backup),
            { onFileStatus: (path, status) => statuses.set(path, status) },
            noWaitDeps(invoke),
        );
        expect(folderRemovals(calls, '/srv/data/docs')).toEqual([]);
        expect(report.errors.map((e) => e.file_path)).toEqual(['docs/only-copy.txt']);
        expect(report.deleted).toBe(0);
        expect(report.skipped).toBe(1);
        expect(statuses.get('docs')).toBe('skipped');
        expect(lastSavedJournal(calls)?.entries.find((e) => e.relative_path === 'docs')?.status).toBe('skipped');
    });

    it('keeps every folder above a folder that stayed', async () => {
        const { invoke, calls } = makeInvoke({
            sync_remove_empty_dir: (args) => (args?.path === '/srv/data/a/b' ? 'kept:entries' : 'removed'),
        });
        const report = await runRemoteSync(
            [file('a/b', 'delete-remote', { isDir: true }), file('a', 'delete-remote', { isDir: true })],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        expect(folderRemovals(calls, '/srv/data/a')).toEqual([]);
        expect(report.skipped).toBe(2);
        expect(report.errors).toEqual([]);
        // Round 2: each kept folder says why. The lower one holds entries the
        // plan did not have; the upper one holds a row that did not complete.
        expect(report.keptDirs).toEqual([
            { file_path: 'a/b', reason: 'entries' },
            { file_path: 'a', reason: 'unfinished_rows' },
        ]);
    });

    it('removes a folder with versioned backup on only by the non-recursive removal (B2)', async () => {
        const { invoke, calls } = makeInvoke({
            sync_backup_run_stamp: () => '20260929T070000Z',
            sync_backup_archive_remote: (args) => `/srv/data/.aeroftp-versions/S/${String(args?.rel)}`,
            sync_remove_empty_dir: () => 'removed',
        });
        const report = await runRemoteSync(
            [file('docs/a.txt', 'delete-remote'), file('docs', 'delete-remote', { isDir: true })],
            noDirs,
            baseConfig(backup),
            {},
            noWaitDeps(invoke),
        );
        expect(folderRemovals(calls, '/srv/data/docs').map((c) => c.cmd)).toEqual(['sync_remove_empty_dir']);
        expect(report.deleted).toBe(2);
        expect(report.errors).toEqual([]);
    });

    it('keeps a folder that still holds an excluded file, as kept and not as an error (H6)', async () => {
        // docs/.env is excluded, so the compare gave it no row: only the
        // folder row says anything about docs.
        const { invoke, calls } = makeInvoke({ sync_remove_empty_dir: () => 'kept:entries' });
        const report = await runRemoteSync(
            [file('docs', 'delete-remote', { isDir: true })],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        expect(folderRemovals(calls, '/srv/data/docs')).toEqual([
            { cmd: 'sync_remove_empty_dir', args: { target: 'ftp', path: '/srv/data/docs' } },
        ]);
        expect(report.deleted).toBe(0);
        expect(report.skipped).toBe(1);
        expect(report.errors).toEqual([]);
        expect(report.keptDirs).toEqual([{ file_path: 'docs', reason: 'entries' }]);
    });

    it('says when the server, not the listing, is what keeps a folder', async () => {
        // An FTP LIST that hides dot files: RMD refuses, the listing shows
        // nothing, and the run says so instead of a bare "skipped".
        const { invoke } = makeInvoke({ sync_remove_empty_dir: () => 'kept:server' });
        const report = await runRemoteSync(
            [file('docs', 'delete-remote', { isDir: true })],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        expect(report.skipped).toBe(1);
        expect(report.keptDirs).toEqual([{ file_path: 'docs', reason: 'server' }]);
        // The old bare answer is not read as anything.
        expect(() => readEmptyDirAnswer('kept')).toThrow(/unexpected answer/);
    });

    it.each([
        ['a provider remote', { isProvider: true }, 'delete-remote', 'provider', '/srv/data/d'],
        ['the right folder of a local pair', { isLocalLocal: true }, 'delete-remote', 'local', '/srv/data/d'],
        ['the local side', {}, 'delete-local', 'local', '/home/u/work/d'],
    ] as const)('removes a folder on %s without recursing', async (_label, over, action, target, path) => {
        const { invoke, calls } = makeInvoke({ sync_remove_empty_dir: () => 'removed' });
        const report = await runRemoteSync(
            [file('d', action, { isDir: true })],
            noDirs,
            baseConfig(over),
            {},
            noWaitDeps(invoke),
        );
        expect(folderRemovals(calls, path)).toEqual([{ cmd: 'sync_remove_empty_dir', args: { target, path } }]);
        expect(report.deleted).toBe(1);
    });

    it('fails a folder whose removal gives no clear answer', async () => {
        const { invoke } = makeInvoke({ sync_remove_empty_dir: () => undefined });
        const report = await runRemoteSync(
            [file('d', 'delete-remote', { isDir: true })],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        expect(report.deleted).toBe(0);
        expect(report.errors.map((e) => e.file_path)).toEqual(['d']);
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
        expect(lastSavedJournal(calls)?.completed).toBe(false);
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

    it('retries failed journal entries on resume and skips completed ones', async () => {
        const resumeJournal: SyncJournal = {
            id: 'j-fail',
            created_at: '2026-05-22T09:00:00Z',
            updated_at: '2026-05-22T09:05:00Z',
            local_path: '/home/u/work',
            remote_path: '/srv/data',
            direction: 'bidirectional',
            retry_policy: RETRY,
            verify_policy: 'none',
            entries: [
                {
                    relative_path: 'ok.txt',
                    action: 'upload',
                    status: 'completed',
                    attempts: 1,
                    last_error: null,
                    verified: true,
                    bytes_transferred: 100,
                },
                {
                    relative_path: 'bad.txt',
                    action: 'upload',
                    status: 'failed',
                    attempts: 1,
                    last_error: {
                        kind: 'permission_denied',
                        message: 'permission denied',
                        retryable: false,
                        file_path: 'bad.txt',
                    },
                    verified: null,
                    bytes_transferred: 0,
                },
            ],
            completed: false,
        };
        const { invoke, calls } = makeInvoke();
        const report = await runRemoteSync(
            [file('ok.txt', 'upload', { size: 100 }), file('bad.txt', 'upload', { size: 200 })],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, { resumeJournal }),
        );
        expect(report.uploaded).toBe(2);
        const uploadCalls = calls.filter((c) => c.cmd === 'upload_file');
        expect(uploadCalls).toHaveLength(1);
        expect((uploadCalls[0].args?.params as { local_path: string }).local_path)
            .toBe('/home/u/work/bad.txt');
    });

    it('transfers remaining files when resuming a cancelled run', async () => {
        let seen = 0;
        const first = makeInvoke();
        await runRemoteSync(
            [file('a.txt', 'upload'), file('b.txt', 'upload'), file('c.txt', 'upload')],
            noDirs,
            baseConfig(),
            { isCancelled: () => { seen += 1; return seen > 1; } },
            noWaitDeps(first.invoke),
        );
        const journal = JSON.parse(JSON.stringify(lastSavedJournal(first.calls))) as SyncJournal;
        expect(journal.completed).toBe(false);

        const second = makeInvoke();
        const report = await runRemoteSync(
            [file('a.txt', 'upload'), file('b.txt', 'upload'), file('c.txt', 'upload')],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(second.invoke, { resumeJournal: journal }),
        );
        expect(report.uploaded).toBe(3);
        const uploadCalls = second.calls.filter((c) => c.cmd === 'upload_file');
        expect(uploadCalls).toHaveLength(2);
        expect(uploadCalls.map((c) => (c.args?.params as { local_path: string }).local_path))
            .toEqual(['/home/u/work/b.txt', '/home/u/work/c.txt']);
    });

    it('retries skipped entries saved by an older cancelled journal', async () => {
        const resumeJournal: SyncJournal = {
            id: 'j-skipped',
            created_at: '2026-05-22T09:00:00Z',
            updated_at: '2026-05-22T09:05:00Z',
            local_path: '/home/u/work',
            remote_path: '/srv/data',
            direction: 'bidirectional',
            retry_policy: RETRY,
            verify_policy: 'none',
            entries: [
                {
                    relative_path: 'a.txt',
                    action: 'upload',
                    status: 'completed',
                    attempts: 1,
                    last_error: null,
                    verified: true,
                    bytes_transferred: 100,
                },
                {
                    relative_path: 'b.txt',
                    action: 'upload',
                    status: 'skipped',
                    attempts: 0,
                    last_error: null,
                    verified: null,
                    bytes_transferred: 0,
                },
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
        expect(report.uploaded).toBe(2);
        const uploadCalls = calls.filter((c) => c.cmd === 'upload_file');
        expect(uploadCalls).toHaveLength(1);
        expect((uploadCalls[0].args?.params as { local_path: string }).local_path)
            .toBe('/home/u/work/b.txt');
    });
});

describe('remoteSyncRunner: journal kept on failures', () => {
    it('does not complete or delete the journal when a non-cancelled run has a failed file', async () => {
        const { invoke, calls } = makeInvoke(denyUpload);
        const report = await runRemoteSync(
            [file('a.txt', 'upload'), file('b.txt', 'upload')],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        expect(report.cancelled).toBe(false);
        expect(report.uploaded).toBe(0);
        expect(report.errors).toHaveLength(2);
        const saved = lastSavedJournal(calls);
        expect(saved?.completed).toBe(false);
        expect(saved?.entries.every((e) => e.status === 'failed')).toBe(true);
        expect(calls.some((c) => c.cmd === 'delete_sync_journal_cmd')).toBe(false);
    });

    it('keeps the journal resumable when one file succeeds and another fails', async () => {
        const { invoke, calls } = makeInvoke({
            upload_file: (_args, idx) => {
                if (idx >= 1) throw new Error('permission denied');
                return undefined;
            },
            classify_transfer_error: (args) => ({
                kind: 'permission_denied',
                message: String(args?.rawError),
                retryable: false,
                file_path: String(args?.filePath),
            } satisfies SyncErrorInfo),
        });
        const report = await runRemoteSync(
            [file('ok.txt', 'upload', { size: 100 }), file('bad.txt', 'upload', { size: 200 })],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        expect(report.cancelled).toBe(false);
        expect(report.uploaded).toBe(1);
        expect(report.errors).toHaveLength(1);
        const saved = lastSavedJournal(calls);
        expect(saved?.completed).toBe(false);
        expect(saved?.entries.find((e) => e.relative_path === 'ok.txt')?.status).toBe('completed');
        expect(saved?.entries.find((e) => e.relative_path === 'bad.txt')?.status).toBe('failed');
        expect(calls.some((c) => c.cmd === 'delete_sync_journal_cmd')).toBe(false);
    });

    it('does not complete or delete the journal when a download fails verification', async () => {
        const { invoke, calls } = makeInvoke({
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
            {},
            noWaitDeps(invoke),
        );
        expect(report.cancelled).toBe(false);
        expect(report.verifyFailed).toBe(1);
        const saved = lastSavedJournal(calls);
        expect(saved?.completed).toBe(false);
        expect(saved?.entries[0]?.status).toBe('verify_failed');
        expect(calls.some((c) => c.cmd === 'delete_sync_journal_cmd')).toBe(false);
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

    // Minor 1 (fourth review of #949): the time was read back when the index
    // was saved, after the whole run, so a same-size edit made in between was
    // recorded as the synced state. It is read when the download completes.
    it('records the time a download left, not the one at index save', async () => {
        let savedIndex: Record<string, unknown> | undefined;
        let indexPhase = false;
        const { invoke } = makeInvoke({
            get_file_properties: () => ({
                size: 7,
                modified: indexPhase ? '2026-09-26T11:00:00' : '2026-09-26T10:00:00',
            }),
            load_sync_index_cmd: () => {
                indexPhase = true;
                return null;
            },
            save_sync_index_cmd: (args) => {
                savedIndex = args?.index as Record<string, unknown>;
            },
        });
        await runRemoteSync(
            [file('from-list.txt', 'download', { size: 7, mtime: null })],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, { writeIndex: true }),
        );
        const files = (savedIndex?.files ?? {}) as Record<string, { modified: string | null }>;
        expect(files['from-list.txt']?.modified).toBe('2026-09-26T10:00:00Z');
    });

    // Major 1 (re-review of #949): a download recorded the remote side's
    // time, and a backend listing no comparable time (FTP LIST dates) gives
    // none, so the local side of that file was compared by size alone on the
    // next run and a same-size local edit went unseen. With no remote time the
    // index takes the downloaded file's own, read back from disk.
    it('records the downloaded file time when the remote gives none', async () => {
        let savedIndex: Record<string, unknown> | undefined;
        const { invoke } = makeInvoke({
            get_file_properties: () => ({ size: 7, modified: '2026-09-26T10:00:00' }),
            save_sync_index_cmd: (args) => {
                savedIndex = args?.index as Record<string, unknown>;
            },
        });
        await runRemoteSync(
            [
                file('from-list.txt', 'download', { size: 7, mtime: null }),
                file('dated.txt', 'download', { size: 9 }),
            ],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, { writeIndex: true }),
        );
        const files = (savedIndex?.files ?? {}) as Record<string, { modified: string | null }>;
        expect(files['from-list.txt']?.modified).toBe('2026-09-26T10:00:00Z');
        // A remote time is kept: the download stamped it on the local copy.
        expect(files['dated.txt']?.modified).toBe('2026-05-22T10:00:00Z');
    });

    // CodeRabbit on #949 (a90e5933): a download the interrupted run finished
    // is skipped on resume, so this run never reads its time, and the index
    // entry the interrupted run wrote with it was replaced by null (the
    // remote gives none): the next compare fell back to size alone. Minor 4
    // (verification of the fifth round): the time is the one the journal
    // entry recorded when the download completed, not whatever the index
    // holds (a crashed run saved none, and a chain of resumes carries older
    // runs' entries forward).
    it('keeps the index time of a download the resumed journal finished', async () => {
        const resumeJournal: SyncJournal = {
            id: 'j-landed',
            created_at: '2026-09-26T09:00:00Z',
            updated_at: '2026-09-26T09:05:00Z',
            local_path: '/home/u/work',
            remote_path: '/srv/data',
            direction: 'bidirectional',
            retry_policy: RETRY,
            verify_policy: 'none',
            entries: [
                // The size it recorded is not the one listed now (8 against 7),
                // so the test tells which of the two the index takes.
                { relative_path: 'done.txt', action: 'download', status: 'completed', attempts: 1, last_error: null, verified: null, bytes_transferred: 7, local_size: 8, local_modified: '2026-09-26T09:04:00Z' },
                // Written before the journal kept these: no time.
                { relative_path: 'resized.txt', action: 'download', status: 'completed', attempts: 1, last_error: null, verified: null, bytes_transferred: 9 },
            ],
            completed: false,
        };
        let savedIndex: Record<string, unknown> | undefined;
        const { invoke, calls } = makeInvoke({
            load_sync_index_cmd: () => ({
                version: 2,
                last_sync: '2026-09-26T09:05:00Z',
                local_path: '/home/u/work',
                remote_path: '/srv/data',
                files: {
                    // What an older run recorded: the journal wins.
                    'done.txt': { size: 7, modified: '2026-09-20T08:00:00Z', is_dir: false },
                    'resized.txt': { size: 9, modified: '2026-09-20T08:00:00Z', is_dir: false },
                },
            }),
            save_sync_index_cmd: (args) => {
                savedIndex = args?.index as Record<string, unknown>;
            },
        });
        await runRemoteSync(
            [
                file('done.txt', 'download', { size: 7, mtime: null }),
                file('resized.txt', 'download', { size: 9, mtime: null }),
            ],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, { writeIndex: true, resumeJournal }),
        );
        expect(calls.filter((c) => c.cmd === 'download_file')).toHaveLength(0);
        const files = (savedIndex?.files ?? {}) as Record<string, { modified: string | null; size: number }>;
        expect(files['done.txt']).toMatchObject({ size: 8, modified: '2026-09-26T09:04:00Z' });
        expect(files['resized.txt']?.modified).toBeNull();
    });

    const resumeOf = (entries: SyncJournal['entries']): SyncJournal => ({
        id: 'j-resume',
        created_at: '2026-09-26T09:00:00Z',
        updated_at: '2026-09-26T09:05:00Z',
        local_path: '/home/u/work',
        remote_path: '/srv/data',
        direction: 'bidirectional',
        retry_policy: RETRY,
        verify_policy: 'none',
        entries,
        completed: false,
    });
    const savedAt = (lastSync: string, files: Record<string, unknown>) => () => ({
        version: 2,
        last_sync: lastSync,
        local_path: '/home/u/work',
        remote_path: '/srv/data',
        files,
    });

    // m3 (verification of the fourth round of #949): a run that crashed
    // saved no index, and the entry there is from the run before it, of the
    // file before the transfer. It was adopted when the size matched.
    it('does not adopt an index entry saved before the resumed journal', async () => {
        let savedIndex: Record<string, unknown> | undefined;
        const { invoke } = makeInvoke({
            load_sync_index_cmd: savedAt('2026-09-26T08:55:00Z', {
                'done.txt': { size: 7, modified: '2026-09-20T08:00:00Z', is_dir: false },
            }),
            save_sync_index_cmd: (args) => {
                savedIndex = args?.index as Record<string, unknown>;
            },
        });
        await runRemoteSync(
            [file('done.txt', 'download', { size: 7, mtime: null })],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, {
                writeIndex: true,
                resumeJournal: resumeOf([
                    { relative_path: 'done.txt', action: 'download', status: 'completed', attempts: 1, last_error: null, verified: null, bytes_transferred: 7 },
                ]),
            }),
        );
        const files = (savedIndex?.files ?? {}) as Record<string, { modified: string | null }>;
        expect(files['done.txt']?.modified).toBeNull();
    });

    // m4 (same verification): a run resumed from its journal knows neither
    // the time nor the size of what it uploads (the journal keeps only the
    // bytes of finished entries), and recorded the upload with no time: a
    // same-size local edit went unseen. The file is read before it goes up;
    // an upload the interrupted run finished keeps the time it saved.
    it('records the time and size of the uploads of a resumed run', async () => {
        let savedIndex: Record<string, unknown> | undefined;
        const { invoke, calls } = makeInvoke({
            get_file_properties: () => ({ size: 7, modified: '2026-09-26T10:00:00' }),
            load_sync_index_cmd: savedAt('2026-09-26T09:05:00Z', {}),
            save_sync_index_cmd: (args) => {
                savedIndex = args?.index as Record<string, unknown>;
            },
        });
        await runRemoteSync(
            [
                file('done.txt', 'upload', { size: 5, mtime: null }),
                file('next.txt', 'upload', { size: 0, mtime: null }),
            ],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, {
                writeIndex: true,
                resumeJournal: resumeOf([
                    { relative_path: 'done.txt', action: 'upload', status: 'completed', attempts: 1, last_error: null, verified: true, bytes_transferred: 5, local_size: 5, local_modified: '2026-09-26T09:04:00Z' },
                    { relative_path: 'next.txt', action: 'upload', status: 'pending', attempts: 0, last_error: null, verified: null, bytes_transferred: 0 },
                ]),
            }),
        );
        expect(calls.filter((c) => c.cmd === 'upload_file')).toHaveLength(1);
        const files = (savedIndex?.files ?? {}) as Record<string, { size: number; modified: string | null }>;
        expect(files['next.txt']).toMatchObject({ size: 7, modified: '2026-09-26T10:00:00Z' });
        expect(files['done.txt']).toMatchObject({ size: 5, modified: '2026-09-26T09:04:00Z' });
    });

    // Minor 4 (verification of the fifth round): a completed transfer keeps
    // in its journal entry what the index records for it, so a later resume
    // has it even when this run saves no index (it crashes).
    it('records in the journal what the index records for a transfer', async () => {
        const journals: SyncJournal[] = [];
        const { invoke } = makeInvoke({
            get_file_properties: () => ({ size: 7, modified: '2026-09-26T10:00:00' }),
            save_sync_journal_cmd: (args) => {
                journals.push(JSON.parse(JSON.stringify(args?.journal)) as SyncJournal);
            },
        });
        await runRemoteSync(
            [
                file('listed.txt', 'download', { size: 7, mtime: null }),
                file('dated.txt', 'upload', { size: 9 }),
            ],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, { writeIndex: true }),
        );
        const last = journals[journals.length - 1];
        const byPath = Object.fromEntries(last.entries.map((e) => [e.relative_path, e]));
        expect(byPath['listed.txt']).toMatchObject({ local_size: 7, local_modified: '2026-09-26T10:00:00Z' });
        expect(byPath['dated.txt']).toMatchObject({ local_size: 9, local_modified: '2026-05-22T10:00:00Z' });
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

describe('remoteSyncRunner: speed mode reaches the transfer', () => {
    // The speed mode's only transfer-level effect is the delta flag
    // (SPEED_PRESETS); these pin that the flag reaches both transfer commands
    // on both routes, since the Plan tab no longer shows stream or
    // compression controls that would suggest otherwise.
    it.each([true, false])('passes deltaSyncEnabled=%s to upload and download', async (delta) => {
        for (const isProvider of [false, true]) {
            const { invoke, calls } = makeInvoke();
            await runRemoteSync(
                [file('up.txt', 'upload'), file('down.txt', 'download')],
                noDirs,
                baseConfig({ deltaSyncEnabled: delta, isProvider }),
                {},
                noWaitDeps(invoke),
            );
            const transfers = calls.filter((c) => /^(provider_)?(upload|download)_file$/.test(c.cmd));
            expect(transfers.map((c) => c.cmd).sort()).toEqual(
                isProvider ? ['provider_download_file', 'provider_upload_file'] : ['download_file', 'upload_file'],
            );
            for (const c of transfers) {
                const flag = isProvider
                    ? (c.args as { useDelta?: boolean }).useDelta
                    : (c.args as { params: { use_delta?: boolean } }).params.use_delta;
                expect(flag, `${c.cmd} isProvider=${isProvider}`).toBe(delta);
            }
        }
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
