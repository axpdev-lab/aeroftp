// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { createIntentFixture, wrapIntentInvoke } from './testFixtures/syncIntents';
import { transformWithOxc } from 'vite';
import { describe, expect, it, vi } from 'vitest';
import APP from '../App.tsx?raw';
import {
    readEmptyDirAnswer,
    runRemoteSync,
    LEASED_COMMANDS,
    withRunLease,
    PAGE_NONCE,
    groupErrorsByKind,
    filesFromJournal,
    retryPolicyForSpeed,
    deltaGateDecision,
    runDeltaGate,
    type DeltaGateDeps,
    type SyncRunFile,
    type SyncRunDirs,
    type RemoteSyncConfig,
    type RemoteSyncDeps,
} from './remoteSyncRunner';
import type {
    RetryPolicy,
    SyncErrorInfo,
    SyncJournal,
    SyncIndex,
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
    profileId: 'A',
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
    invoke: wrapIntentInvoke(invoke),
    remoteTarget: { ownerBinding: 'test-owner-v1', epoch: 1, assertCurrent: () => undefined },
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

// The native side admits a run's command only while its lease is live, so
// every command of the set must carry the run's token, and no other one.
// A runner command's error ends with the classification the native side made
// from the full text. The run keeps that kind and retry verdict, and the
// report (and the journal and activity log it feeds) shows the text without
// the tag.
describe('remoteSyncRunner: a command error in the report', () => {
    // The two delete handlers: a failed file delete and a failed empty-folder
    // removal keep the tag out of the report, the status callback and the journal.
    it.each(['file', 'empty folder'] as const)('keeps the tag out of a failed %s delete', async kind => {
        const tagged = 'Permission denied (HTTP 403) [sync-error:permission_denied:false]';
        const { invoke, calls } = makeInvoke({
            delete_remote_file: () => { throw new Error(tagged); },
            sync_remove_empty_dir: () => { throw new Error(tagged); },
            classify_transfer_error: (args) => ({ kind: 'permission_denied', retryable: false, message: String(args?.rawError), file_path: String(args?.filePath) } as SyncErrorInfo),
        });
        const statuses: string[] = [];
        const row = kind === 'file' ? file('gone.txt', 'delete-remote') : file('gone-dir', 'delete-remote', { isDir: true });
        const report = await runRemoteSync([row], noDirs, baseConfig(), {
            onFileStatus: (_path, _status, message) => { if (message) statuses.push(message); },
        }, noWaitDeps(invoke));
        const everything = JSON.stringify({
            errors: report.errors,
            statuses,
            journals: calls.filter(c => c.cmd === 'save_sync_journal_cmd').map(c => c.args),
        });
        expect(report.errors.length + statuses.length).toBeGreaterThan(0);
        expect(everything).not.toContain('[sync-error:');
    });

    it.each([
        ['Permission denied (HTTP 403) [sync-error:permission_denied:false]', 'permission_denied', false, 1],
        ['Network error (HTTP 503) [sync-error:network:true]', 'network', true, 2],
    ] as const)('reports %s without the tag, with its kind and retry verdict', async (returned, kind, retryable, attempts) => {
        let uploads = 0;
        const { invoke } = makeInvoke({
            upload_file: () => { uploads++; throw new Error(returned); },
            // As natively (`classify_ipc_error`): the tag, read back.
            classify_transfer_error: (args) => {
                const raw = String(args?.rawError);
                const tag = /\[sync-error:([a-z_]+):(true|false)\]$/.exec(raw)!;
                return { kind: tag[1], retryable: tag[2] === 'true', message: raw, file_path: String(args?.filePath) } as SyncErrorInfo;
            },
            load_sync_index_cmd: () => null,
        });
        const report = await runRemoteSync([file('a.txt', 'upload')], noDirs, baseConfig({ retryPolicy: { ...RETRY, max_retries: 2 } }), {}, noWaitDeps(invoke));
        expect(report.errors).toHaveLength(1);
        const [error] = report.errors;
        expect(error.message).not.toContain('[sync-error:');
        expect([error.kind, error.retryable]).toEqual([kind, retryable]);
        expect(uploads).toBe(attempts);
    });
});

describe('remoteSyncRunner: lease token on native commands', () => {
    // The document binds its nonce before its first lease request, and every
    // request names it. A request refused as replaced (another document's
    // binding took the window) binds again and asks once more; a second
    // refusal stops the run.
    it('binds the document before asking, and retries once after a replaced page', async () => {
        const { invoke } = makeInvoke({ load_sync_index_cmd: () => null });
        const fixture = createIntentFixture();
        const calls: string[] = [];
        let refusals = 0;
        const replacedOnce: RemoteSyncDeps['invoke'] = async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
            if (cmd === 'sync_run_page_bind_cmd' || cmd === 'sync_run_lease_begin_cmd') calls.push(cmd);
            if (cmd === 'sync_run_lease_begin_cmd' && refusals === 0) {
                refusals++;
                throw new Error('SYNC_RUN_PAGE_REPLACED: this window\'s page was replaced; reload it and compare again');
            }
            return fixture.wrap(invoke)<T>(cmd, args);
        };
        await runRemoteSync([file('one.txt', 'upload')], noDirs, baseConfig(), {}, { ...noWaitDeps(invoke), invoke: replacedOnce });
        // The first binding may come from an earlier run of this document.
        expect(calls.slice(-3)).toEqual(['sync_run_lease_begin_cmd', 'sync_run_page_bind_cmd', 'sync_run_lease_begin_cmd']);
        expect(fixture.bindArgs[fixture.bindArgs.length - 1]?.pageNonce).toBe(PAGE_NONCE);
        expect(fixture.beginArgs.map(args => args.pageNonce)).toEqual([PAGE_NONCE]);
        // Refused again after the new binding: the run stops, no third ask.
        let begins = 0;
        const alwaysReplaced: RemoteSyncDeps['invoke'] = async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
            if (cmd === 'sync_run_lease_begin_cmd') {
                begins++;
                throw new Error('SYNC_RUN_PAGE_REPLACED: this window\'s page was replaced; reload it and compare again');
            }
            return fixture.wrap(invoke)<T>(cmd, args);
        };
        await expect(runRemoteSync([file('two.txt', 'upload')], noDirs, baseConfig(), {}, { ...noWaitDeps(invoke), invoke: alwaysReplaced }))
            .rejects.toThrow('SYNC_RUN_PAGE_REPLACED');
        expect(begins).toBe(2);
        expect(PAGE_NONCE).toMatch(/^page-[0-9a-f]{32}$/);
    });

    it('binds once for every run of the document', async () => {
        const { invoke } = makeInvoke({ load_sync_index_cmd: () => null });
        const fixture = createIntentFixture();
        await runRemoteSync([file('one.txt', 'upload')], noDirs, baseConfig(), {}, { ...noWaitDeps(invoke), invoke: fixture.wrap(invoke) });
        await runRemoteSync([file('two.txt', 'upload')], noDirs, baseConfig(), {}, { ...noWaitDeps(invoke), invoke: fixture.wrap(invoke) });
        expect(fixture.bindArgs.length).toBeLessThanOrEqual(1);
        expect(fixture.beginArgs.map(args => args.pageNonce)).toEqual([PAGE_NONCE, PAGE_NONCE]);
    });

    // Every member of the set, not only the commands a plan happens to reach.
    it.each([...LEASED_COMMANDS])('sends the run lease with %s', cmd => {
        expect(withRunLease(cmd, { path: '/a' }, 'run-lease-9')).toEqual({ path: '/a', syncRunLease: 'run-lease-9' });
    });
    it('sends no lease with a command outside the set', () => {
        for (const cmd of ['reset_cancel_flag', 'ftp_noop', 'provider_bulk_support', 'sync_backup_verify_receipt', 'classify_transfer_error']) {
            expect(LEASED_COMMANDS.has(cmd), cmd).toBe(false);
            expect(withRunLease(cmd, { path: '/a' }, 'run-lease-9')).toEqual({ path: '/a' });
        }
        expect(LEASED_COMMANDS.size).toBe(23);
    });

    it.each([false, true])('passes the run lease to every leased command (local-local: %s)', async isLocalLocal => {
        const { invoke, calls } = makeInvoke({ load_sync_index_cmd: () => null });
        await runRemoteSync(
            [file('up.txt', 'upload'), file('down.txt', 'download'), file('gone.txt', 'delete-remote'), file('old.txt', 'delete-local')],
            noDirs,
            baseConfig({ isLocalLocal, direction: 'bidirectional' }),
            {},
            noWaitDeps(invoke),
        );
        const leased = calls.filter(c => LEASED_COMMANDS.has(c.cmd));
        const others = calls.filter(c => !LEASED_COMMANDS.has(c.cmd));
        const sent = new Set(leased.map(c => c.cmd));
        // The plan reaches a copy, both deletes and a journal write.
        expect(sent).toContain(isLocalLocal ? 'copy_local_file' : 'upload_file');
        expect(sent).toContain('delete_local_file');
        expect(sent).toContain('save_sync_journal_cmd');
        for (const call of leased) expect(call.args?.syncRunLease, call.cmd).toMatch(/^run-lease-\d+$/);
        for (const call of others) expect(call.args?.syncRunLease, call.cmd).toBeUndefined();
    });
});

describe('remoteSyncRunner — copy legs', () => {
    it('refuses names the disk cannot hold apart, and downloads the rest (Requested 124)', async () => {
        const { invoke, calls } = makeInvoke({
            local_target_problems: async () => [
                { path: 'docs/File.txt', reason: 'This disk cannot tell it from docs/file.txt' },
                { path: 'docs/file.txt', reason: 'This disk cannot tell it from docs/File.txt' },
                { path: 'bad/colon:name.txt', reason: 'Windows cannot hold a name' },
            ],
        });
        const report = await runRemoteSync(
            [
                file('docs/File.txt', 'download'),
                file('docs/file.txt', 'download'),
                file('bad/colon:name.txt', 'download'),
                file('docs/ok.txt', 'download'),
            ],
            { remote: [], local: [] },
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        const sent = calls.filter((c) => c.cmd === 'download_file' || c.cmd === 'provider_download_file');
        expect(sent).toHaveLength(1);
        expect(JSON.stringify(sent[0].args)).toContain('ok.txt');
        expect(report.errors.map((e) => e.file_path).sort()).toEqual([
            'bad/colon:name.txt', 'docs/File.txt', 'docs/file.txt',
        ]);
        expect(report.errors[0].message).toContain('This disk cannot tell it');
        expect(calls.some((c) => c.cmd === 'create_local_folder'
            && String((c.args as { path?: string })?.path).endsWith('/bad'))).toBe(false);
        const asked = calls.find((c) => c.cmd === 'local_target_problems');
        expect(asked?.args).toMatchObject({
            destination: baseConfig().localRoot,
            files: ['docs/File.txt', 'docs/file.txt', 'bad/colon:name.txt', 'docs/ok.txt'],
        });
    });

    it('checks the names of a local pair\'s uploads against the folder they go to', async () => {
        const { invoke, calls } = makeInvoke({
            local_target_problems: async () => [
                { path: 'Foo.txt', reason: 'This disk cannot tell it from foo.txt' },
                { path: 'foo.txt', reason: 'This disk cannot tell it from Foo.txt' },
            ],
        });
        const report = await runRemoteSync(
            [file('Foo.txt', 'upload'), file('foo.txt', 'upload'), file('ok.txt', 'upload')],
            { remote: [], local: [] },
            baseConfig({ isLocalLocal: true }),
            {},
            noWaitDeps(invoke),
        );
        const copied = calls.filter((c) => c.cmd === 'copy_local_file');
        expect(copied).toHaveLength(1);
        expect(JSON.stringify(copied[0].args)).toContain('ok.txt');
        expect(report.errors.map((e) => e.file_path).sort()).toEqual(['Foo.txt', 'foo.txt']);
        expect(calls.find((c) => c.cmd === 'local_target_problems')?.args)
            .toMatchObject({ destination: baseConfig().remoteRoot, files: ['Foo.txt', 'foo.txt', 'ok.txt'] });
    });

    it('downloads nothing when the local names could not be checked', async () => {
        const { invoke, calls } = makeInvoke({
            local_target_problems: async () => { throw new Error('no answer'); },
        });
        const report = await runRemoteSync(
            [file('a.txt', 'download')],
            { remote: [], local: ['newdir'] },
            baseConfig(),
            {},
            noWaitDeps(invoke),
        );
        expect(calls.filter((c) => c.cmd === 'download_file')).toHaveLength(0);
        expect(calls.filter((c) => c.cmd === 'create_local_folder')).toHaveLength(0);
        expect(report.errors[0].message).toContain('Could not check');
    });

    it('does not create folders the disk cannot hold apart', async () => {
        const { invoke, calls } = makeInvoke({
            local_target_problems: async () => [
                { path: 'Foo', reason: 'This disk cannot tell Foo apart from foo' },
                { path: 'foo', reason: 'This disk cannot tell foo apart from Foo' },
            ],
        });
        await runRemoteSync([], { remote: [], local: ['Foo', 'foo', 'ok'] }, baseConfig(), {}, noWaitDeps(invoke));
        const made = calls.filter((c) => c.cmd === 'create_local_folder').map((c) => JSON.stringify(c.args));
        expect(made).toHaveLength(1);
        expect(made[0]).toContain('ok');
    });

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

    // A backup refused by the provider returns the command's fixed message with
    // its classification tag. The report and the journal keep the text the
    // user reads, without the tag.
    it('keeps the classification tag out of the report and journal of a failed backup', async () => {
        const { invoke, calls } = stampedInvoke({
            sync_backup_archive_remote: () => { throw new Error('Permission denied (HTTP 403) [sync-error:permission_denied:false]'); },
        });
        const report = await runRemoteSync(
            [file('docs/a.txt', 'upload', { overwritesExisting: true })],
            noDirs,
            baseConfig(backup),
            {},
            noWaitDeps(invoke),
        );
        expect(report.errors).toHaveLength(1);
        expect(report.errors[0].message).toContain('Permission denied');
        expect(report.errors[0].message).not.toContain('[sync-error:');
        const journals = JSON.stringify(calls.filter(c => c.cmd === 'save_sync_journal_cmd').map(c => c.args));
        expect(journals).not.toContain('[sync-error:');
    });

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
            expectedGuiTarget: 1,
            useProvider: false,
            root: '/srv/data',
            dir: '.aeroftp-versions',
            stamp: '20260925T070000Z',
            rel: 'docs/a.txt',
            syncRunLease: 'run-lease-1',
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
            .toEqual([{ target: 'provider', path: '/srv/data/empty-dir', expectedGuiTarget: 1, syncRunLease: 'run-lease-1' }]);
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
            .toEqual({ path: '/srv/data/gone.txt.aerocorrect', isDir: false, expectedGuiTarget: 1, syncRunLease: 'run-lease-1' });
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
describe('remoteSyncRunner: credentials in a native error', () => {
    const native = () => new Error('550 denied https://alice:hunter2@files.example.com/srv/data/x?access_token=tok-SECRET-123');
    const leaks = ['hunter2', 'tok-SECRET-123', 'alice:'];
    const run = async (plan: SyncRunFile[], handlers: Record<string, Handler>, over: Partial<RemoteSyncConfig> = {}) => {
        const { invoke, calls } = makeInvoke(handlers);
        const statuses: unknown[] = [];
        const report = await runRemoteSync(plan, noDirs, baseConfig(over),
            { onFileStatus: (...status) => { statuses.push(status); } }, noWaitDeps(invoke));
        const journal = lastSavedJournal(calls);
        for (const channel of [report, statuses, journal]) {
            for (const secret of leaks) expect(JSON.stringify(channel)).not.toContain(secret);
        }
        expect(report.errors).toHaveLength(1);
        const message = report.errors[0].message;
        expect(message).toContain('550 denied https://');
        expect(message).toContain('files.example.com/srv/data/x?access_token=***REDACTED***');
        expect(journal?.entries[0].last_error?.message).toBe(message);
        expect(statuses).toContainEqual([plan[0].relativePath, 'error', message]);
        return message;
    };

    it('masks them in a failed backup and keeps the error text', async () => {
        await run([file('a.txt', 'upload', { overwritesExisting: true })], {
            sync_backup_run_stamp: () => '20260925T070000Z',
            sync_backup_archive_remote: () => { throw native(); },
        }, { versionedBackup: { dir: '.aeroftp-versions' } });
    });

    it('masks them in a failed file delete and keeps the path', async () => {
        const message = await run([file('docs/x.txt', 'delete-remote')], { delete_remote_file: () => { throw native(); } });
        expect(message.startsWith('Delete failed: docs/x.txt - ')).toBe(true);
    });

    it('masks them in a failed empty-folder delete and keeps the path', async () => {
        const message = await run([file('docs', 'delete-remote', { isDir: true })], { sync_remove_empty_dir: () => { throw native(); } });
        expect(message.startsWith('Delete failed: docs - ')).toBe(true);
    });
});

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
            { cmd: 'sync_remove_empty_dir', args: { target: 'ftp', path: '/srv/data/docs', expectedGuiTarget: 1, syncRunLease: 'run-lease-1' } },
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
        expect(folderRemovals(calls, path)).toEqual([{ cmd: 'sync_remove_empty_dir', args: { target, path, ...('isLocalLocal' in over ? {} : { expectedGuiTarget: 1 }), syncRunLease: 'run-lease-1' } }]);
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

    it('passes the failure message with an error or verify_failed status', async () => {
        // The Transfer Queue row of a failed file shows this message (#364).
        const statuses: Array<[string, string, string | undefined]> = [];
        const { invoke } = makeInvoke({
            ...denyUpload,
            download_file: (_args, idx) => {
                if (idx === 0) throw new Error('connection reset');
                return undefined;
            },
            verify_local_transfer: (): VerifyResult => ({
                path: '/home/u/work/short.txt',
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
        await runRemoteSync(
            [file('up.txt', 'upload'), file('gone.txt', 'download'), file('short.txt', 'download', { size: 100 })],
            noDirs,
            baseConfig({ verifyPolicy: 'size_only' }),
            { onFileStatus: (p, s, m) => statuses.push([p, s, m]) },
            noWaitDeps(invoke),
        );
        expect(statuses.filter(([, s]) => s === 'error' || s === 'verify_failed')).toEqual([
            ['up.txt', 'error', 'Error: permission denied'],
            ['gone.txt', 'error', 'Error: connection reset'],
            ['short.txt', 'verify_failed', 'short.txt: size mismatch'],
        ]);
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
        const { invoke, calls } = makeInvoke();
        const report = await runRemoteSync(
            [file('a.txt', 'upload'), file('b.txt', 'upload'), file('c.txt', 'upload')],
            noDirs,
            baseConfig(),
            { isCancelled: () => calls.some(call => call.cmd === 'upload_file') },
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
            resume_safe: true, intent_protocol: 1, owner_binding: 'test-owner-v1',
            profile_id: 'A',
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
    it('resumes a bound Quick Connect through the real runner', async () => {
        const resumeJournal: SyncJournal = {
            resume_safe: true, intent_protocol: 1, owner_binding: 'test-owner-v1',
            profile_id: undefined,
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
            baseConfig({ profileId: undefined }),
            {},
            noWaitDeps(invoke, { resumeJournal }),
        );
        expect(calls.some(c => c.cmd === 'provider_gui_verify_resume_owner')).toBe(true);
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
            resume_safe: true, intent_protocol: 1, owner_binding: 'test-owner-v1',
            profile_id: 'A',
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
        const first = makeInvoke();
        await runRemoteSync(
            [file('a.txt', 'upload'), file('b.txt', 'upload'), file('c.txt', 'upload')],
            noDirs,
            baseConfig(),
            { isCancelled: () => first.calls.some(call => call.cmd === 'upload_file') },
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

    it('retries skipped entries saved by a cancelled journal', async () => {
        const resumeJournal: SyncJournal = {
            resume_safe: true, intent_protocol: 1, owner_binding: 'test-owner-v1',
            profile_id: 'A',
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

    // T14: a native timeout or network error can follow a write that landed
    // (its reply lost). The journal then requires a comparison before Resume
    // writes again; a definite refusal leaves it resumable.
    it('requires a comparison before Resume after a native outcome that leaves the write uncertain', async () => {
        for (const [kind, uncertain] of [['timeout', true], ['network', true], ['permission_denied', false]] as const) {
            const { invoke, calls } = makeInvoke({
                upload_file: () => { throw new Error('the reply did not come back'); },
                classify_transfer_error: (args) => ({
                    kind,
                    message: String(args?.rawError),
                    retryable: false,
                    file_path: String(args?.filePath),
                } satisfies SyncErrorInfo),
            });
            await runRemoteSync([file('a.txt', 'upload')], noDirs, baseConfig(), {}, noWaitDeps(invoke));
            const saved = lastSavedJournal(calls);
            expect(saved?.entries[0]?.status, kind).toBe('failed');
            expect(saved?.resume_safe, kind).toBe(!uncertain);
        }
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
        expect(limitCall?.args).toEqual({ downloadKb: 512, uploadKb: 256, expectedGuiTarget: 1 });
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
    // Requested 79: a history serves only its own profile and moves with it
    // when a Proton Drive profile changes client, so the run records the
    // profile in the journal and the index, and reads back only its own.
    it('records the profile in the journal and the index and loads its own index', async () => {
        let savedIndex: Record<string, unknown> | undefined;
        const { invoke, calls } = makeInvoke({
            load_sync_index_cmd: () => null,
            save_sync_index_cmd: (args) => {
                savedIndex = args?.index as Record<string, unknown>;
            },
        });
        await runRemoteSync(
            [file('a.txt', 'upload')],
            noDirs,
            baseConfig({ profileId: 'srv_p1' }),
            {},
            noWaitDeps(invoke, { writeIndex: true }),
        );
        expect(lastSavedJournal(calls)?.profile_id).toBe('srv_p1');
        expect(calls.find((c) => c.cmd === 'load_sync_index_cmd')?.args).toMatchObject({ profileId: 'srv_p1' });
        expect(savedIndex?.profile_id).toBe('srv_p1');
    });

    it('records no profile when the remote has none', async () => {
        let savedIndex: Record<string, unknown> | undefined;
        const { invoke, calls } = makeInvoke({
            save_sync_index_cmd: (args) => {
                savedIndex = args?.index as Record<string, unknown>;
            },
        });
        await runRemoteSync([file('a.txt', 'upload')], noDirs, baseConfig({ profileId: undefined }), {}, noWaitDeps(invoke, { writeIndex: true }));
        expect(lastSavedJournal(calls)).not.toHaveProperty('profile_id');
        expect(savedIndex).not.toHaveProperty('profile_id');
    });

    it('dates an upload in the index at its own end, not at the end of the run', async () => {
        vi.useFakeTimers({ toFake: ['Date'], now: new Date('2026-01-01T00:00:00.000Z') });
        try {
            let savedIndex: { files: Record<string, { uploaded_at?: string }> } | undefined;
            const oneMinuteLater = () => { vi.setSystemTime(Date.now() + 60_000); };
            const { invoke } = makeInvoke({
                upload_file: oneMinuteLater,
                provider_upload_file: oneMinuteLater,
                save_sync_index_cmd: (args) => {
                    savedIndex = args?.index as typeof savedIndex;
                },
            });
            await runRemoteSync([file('a.txt', 'upload'), file('b.txt', 'upload')], noDirs, baseConfig(), {}, noWaitDeps(invoke, { writeIndex: true }));
            const first = new Date(savedIndex!.files['a.txt'].uploaded_at!).getTime();
            const second = new Date(savedIndex!.files['b.txt'].uploaded_at!).getTime();
            expect(first).toBeGreaterThanOrEqual(new Date('2026-01-01T00:01:00.000Z').getTime());
            expect(second - first).toBeGreaterThanOrEqual(60_000);
        } finally {
            vi.useRealTimers();
        }
    });

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

    // L1: a delete the run's Stop cancelled answers the typed cancellation,
    // never a resolved invocation: the item is not counted, the index keeps
    // its entry and no journal record calls it completed.
    it('keeps a delete its Stop cancelled out of the history', async () => {
        let savedIndex: Record<string, unknown> | undefined;
        const journals: Array<Record<string, unknown>> = [];
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
            save_sync_journal_cmd: (args) => {
                journals.push(JSON.parse(JSON.stringify(args?.journal ?? {})));
            },
            delete_remote_file: () => {
                throw new Error('[delete_remote_file] Transfer cancelled by user');
            },
        });
        const report = await runRemoteSync(
            [file('stale.txt', 'delete-remote')],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, { writeIndex: true }),
        );
        expect(report.deleted).toBe(0);
        expect((savedIndex?.files as Record<string, unknown>)['stale.txt']).toBeDefined();
        expect(journals.length).toBeGreaterThan(0);
        for (const journal of journals) {
            const entries = (journal.entries ?? []) as Array<{ relative_path?: string; status?: string }>;
            for (const entry of entries) {
                if (entry.relative_path === 'stale.txt') expect(entry.status).not.toBe('completed');
            }
        }
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
            resume_safe: true, intent_protocol: 1, owner_binding: 'test-owner-v1',
            profile_id: 'A',
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
        resume_safe: true, intent_protocol: 1, owner_binding: 'test-owner-v1',
            profile_id: 'A',
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

    it.each([false, true])('refreshes edited pending upload metadata on Resume (bulk=%s) and retains acknowledged metadata', async bulk => {
        const old = '2026-09-26T09:00:00Z'; const edited = '2026-09-26T10:00:00Z';
        const resume = resumeOf([
            { relative_path: 'done', action: 'upload', status: 'completed', attempts: 1, last_error: null, verified: true, bytes_transferred: 5,
                source_size: 2, source_mtime: old, local_size: 5, local_modified: old },
            ...['one', 'two'].map(relative_path => ({ relative_path, action: 'upload' as const, status: 'pending' as const,
                attempts: 0, last_error: null, verified: null, bytes_transferred: 0, source_size: 3, source_mtime: old })),
        ]);
        let saved: SyncIndex | undefined;
        const { invoke, calls } = makeInvoke({
            get_file_properties: () => ({ size: 17, modified: edited.slice(0, -1) }),
            provider_bulk_support: () => ({ upload: bulk, download: false }),
            provider_sync_transfer_bulk: args => (args!.items as unknown[]).map(() => ({ status: 'completed' })),
            load_sync_index_cmd: savedAt(old, {}), save_sync_index_cmd: args => { saved = structuredClone(args!.index as SyncIndex); },
        });
        const pending = filesFromJournal(resume); const originalPlan = structuredClone(pending);
        const report = await runRemoteSync(pending, noDirs, baseConfig({ isProvider: bulk }), {},
            noWaitDeps(invoke, { resumeJournal: resume, writeIndex: true }));
        expect(report.uploaded).toBe(3);
        expect(report.totalBytes).toBe(5 + 17 + 17);
        expect(calls.filter(call => call.cmd === 'get_file_properties').map(call => call.args?.path)).toEqual(['/home/u/work/one', '/home/u/work/two']);
        expect(saved?.files.one).toMatchObject({ size: 17, modified: edited });
        expect(saved?.files.two).toMatchObject({ size: 17, modified: edited });
        expect(saved?.files.done).toMatchObject({ size: 5, modified: old });
        const savedJournal = lastSavedJournal(calls)!;
        const acknowledged = filesFromJournal(savedJournal);
        expect(acknowledged.find(file => file.relativePath === 'one')).toMatchObject({ size: 17, mtime: edited });
        expect(pending).toEqual(originalPlan);
        expect(resume.entries[1].source_mtime).toBe(old);
        expect(calls.filter(call => call.cmd === 'provider_sync_transfer_bulk')).toHaveLength(bulk ? 1 : 0);
    });

    it.each([true, false])('uses current resumed upload sizes for bulk budget admission (writeIndex=%s), allowing only the final file to cross the limit', async writeIndex => {
        const resume = resumeOf(['one', 'two'].map(relative_path => ({ relative_path, action: 'upload', status: 'pending',
            attempts: 0, last_error: null, verified: null, bytes_transferred: 0, source_size: 1, source_mtime: '2026-09-26T09:00:00Z' })));
        const { invoke, calls } = makeInvoke({ get_file_properties: () => ({ size: 100, modified: '2026-09-26T10:00:00' }),
            provider_bulk_support: () => ({ upload: true, download: false }) });
        const report = await runRemoteSync(filesFromJournal(resume), noDirs, baseConfig({ isProvider: true, transferBudget: 50 }), {},
            noWaitDeps(invoke, { resumeJournal: resume, writeIndex }));
        expect(report.uploaded).toBe(1);
        expect(report.totalBytes).toBe(100);
        expect(calls.filter(call => call.cmd === 'provider_sync_transfer_bulk')).toHaveLength(0);
        expect(calls.filter(call => call.cmd === 'provider_upload_file')).toHaveLength(1);
        expect(calls.filter(call => call.cmd === 'get_file_properties')).toHaveLength(1);
        expect(lastSavedJournal(calls)?.entries.find(entry => entry.relative_path === 'two')?.status).toBe('pending');
    });

    it.each(['provider', 'legacy', 'local'] as const)('does not dispatch a resumed %s upload after Stop during metadata', async path => {
        let release!: () => void;
        let stopped = false;
        const metadata = new Promise<void>(resolve => { release = resolve; });
        const resume = resumeOf([{ relative_path: 'one', action: 'upload', status: 'pending', attempts: 0,
            last_error: null, verified: null, bytes_transferred: 0, source_size: 1, source_mtime: '2026-09-26T09:00:00Z' }]);
        const { invoke, calls } = makeInvoke({ get_file_properties: async () => {
            await metadata; return { size: 100, modified: '2026-09-26T10:00:00' };
        }});
        const pending = runRemoteSync(filesFromJournal(resume), noDirs,
            baseConfig({ isProvider: path === 'provider', isFtp: path === 'legacy', isLocalLocal: path === 'local' }),
            { isCancelled: () => stopped }, noWaitDeps(invoke, { resumeJournal: resume, writeIndex: false }));
        await vi.waitFor(() => expect(calls.some(call => call.cmd === 'get_file_properties')).toBe(true));
        stopped = true;
        release();
        const report = await pending;
        expect(report.cancelled).toBe(true);
        expect(calls.filter(call => ['provider_upload_file', 'upload_file', 'copy_local_file', 'provider_sync_transfer_bulk'].includes(call.cmd))).toHaveLength(0);
    });

    it.each([false, true])('does not rearm cancellation or create directories after Stop (local=%s)', async isLocalLocal => {
        const { invoke, calls } = makeInvoke();
        const report = await runRemoteSync([file('nested/one', 'upload')], { local: ['empty-local'], remote: ['empty-remote'] },
            baseConfig({ isProvider: true, isLocalLocal }), { isCancelled: () => true }, noWaitDeps(invoke));
        expect(report.cancelled).toBe(true);
        expect(calls.filter(call => ['reset_cancel_flag', 'provider_mkdir', 'create_local_folder', 'create_remote_folder',
            'provider_upload_file', 'copy_local_file'].includes(call.cmd))).toHaveLength(0);
    });

    it('does not add cancelled directory-only creations to the baseline', async () => {
        let saved: { files: Record<string, unknown> } | undefined;
        const { invoke } = makeInvoke({ save_sync_index_cmd: args => { saved = args!.index as typeof saved; } });
        await runRemoteSync([], { local: ['local-empty'], remote: ['remote-empty'] }, baseConfig(),
            { isCancelled: () => true }, noWaitDeps(invoke, { writeIndex: true }));
        expect(saved).toBeDefined();
        expect(saved!.files).not.toHaveProperty('local-empty');
        expect(saved!.files).not.toHaveProperty('remote-empty');
    });

    it.each(['stopped', 'rejected', 'failed-status'] as const)('does not verify or index a final download whose EC was %s', async outcome => {
        let stopped = false;
        let destination = 'old';
        let saved: { files: Record<string, unknown> } | undefined;
        const { invoke, calls } = makeInvoke({ download_file: () => { destination = 'downloaded payload'; if (outcome === 'stopped') stopped = true; },
            sync_ec_verify_repair: () => { if (outcome === 'rejected') throw new Error('EC failed'); return { status: 'verify_failed' }; },
            save_sync_index_cmd: args => { saved = args!.index as typeof saved; } });
        const progress = vi.fn();
        const report = await runRemoteSync([file('last', 'download')], noDirs,
            baseConfig({ verifyPolicy: 'none', errorCorrection: { enabled: true, pct: 10 } }),
            { isCancelled: () => stopped, onProgress: progress }, noWaitDeps(invoke, { writeIndex: true }));
        const journal = lastSavedJournal(calls)!;
        expect(journal.entries[0].status).not.toBe('completed');
        expect(journal.entries[0].verified).toBe(false);
        expect(journal.completed).toBe(false);
        expect(saved!.files).not.toHaveProperty('last');
        expect(report.downloaded).toBe(0);
        expect(progress).toHaveBeenLastCalledWith(1, 1);
        // An edited destination must never be overwritten by replay of this failure.
        const downloadsBefore = calls.filter(call => call.cmd === 'download_file').length;
        stopped = false;
        destination = 'user edit after the failed verification';
        await expect(runRemoteSync(filesFromJournal(journal), noDirs,
            baseConfig({ verifyPolicy: 'none', errorCorrection: { enabled: true, pct: 10 } }), {},
            noWaitDeps(invoke, { resumeJournal: journal, writeIndex: true }))).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
        expect(calls.filter(call => call.cmd === 'download_file')).toHaveLength(downloadsBefore);
        expect(destination).toBe('user edit after the failed verification');
        if (outcome === 'stopped') {
            expect(report.cancelled).toBe(true);
            expect(calls.filter(call => call.cmd === 'sync_ec_verify_repair')).toHaveLength(0);
        }
    });

    // The same edited destination after an ordinary verification failure,
    // with EC off: the resume is refused too and nothing is downloaded again.
    it('refuses to resume a download whose ordinary verification failed, without EC', async () => {
        let destination = 'old';
        const { invoke, calls } = makeInvoke({
            download_file: () => { destination = 'downloaded payload'; },
            verify_local_transfer: () => ({ passed: false, size_match: false, mtime_match: null, hash_match: null, message: 'size mismatch' }),
        });
        await runRemoteSync([file('last', 'download')], noDirs, baseConfig({ verifyPolicy: 'size_only' }), {}, noWaitDeps(invoke));
        const journal = lastSavedJournal(calls)!;
        expect(journal.entries[0].status).toBe('verify_failed');
        expect(journal.ec_enabled ?? false).toBe(false);
        const downloadsBefore = calls.filter(call => call.cmd === 'download_file').length;
        destination = 'user edit after the failed verification';
        await expect(runRemoteSync(filesFromJournal(journal), noDirs, baseConfig({ verifyPolicy: 'size_only' }), {},
            noWaitDeps(invoke, { resumeJournal: journal }))).rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
        expect(calls.filter(call => call.cmd === 'download_file')).toHaveLength(downloadsBefore);
        expect(destination).toBe('user edit after the failed verification');
    });

    it('counts an EC failure followed by a successful download as two processed files', async () => {
        const progress = vi.fn();
        const { invoke } = makeInvoke({ sync_ec_verify_repair: (_args, index) => ({ status: index === 0 ? 'verify_failed' : 'verified' }) });
        const report = await runRemoteSync([file('bad', 'download'), file('good', 'download')], noDirs,
            baseConfig({ errorCorrection: { enabled: true, pct: 10 } }), { onProgress: progress }, noWaitDeps(invoke));
        expect(report.downloaded).toBe(1);
        expect(progress).toHaveBeenLastCalledWith(2, 2);
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

    it('keeps the end of an upload across a resumed run', async () => {
        let savedIndex: { files: Record<string, { uploaded_at?: string }> } | undefined;
        const journals: SyncJournal[] = [];
        const { invoke } = makeInvoke({
            save_sync_index_cmd: (args) => {
                savedIndex = args?.index as typeof savedIndex;
            },
            save_sync_journal_cmd: (args) => {
                journals.push(JSON.parse(JSON.stringify(args?.journal)) as SyncJournal);
            },
        });
        await runRemoteSync(
            [file('done.txt', 'upload', { size: 5 }), file('next.txt', 'upload', { size: 7 })],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, {
                writeIndex: true,
                resumeJournal: resumeOf([
                    { relative_path: 'done.txt', action: 'upload', status: 'completed', attempts: 1, last_error: null, verified: true, bytes_transferred: 5, uploaded_at: '2026-09-26T09:05:00.000Z' },
                    { relative_path: 'next.txt', action: 'upload', status: 'pending', attempts: 0, last_error: null, verified: null, bytes_transferred: 0 },
                ]),
            }),
        );
        expect(savedIndex?.files['done.txt'].uploaded_at).toBe('2026-09-26T09:05:00.000Z');
        const next = journals[journals.length - 1]?.entries.find((e) => e.relative_path === 'next.txt');
        expect(next?.uploaded_at).toBe(savedIndex?.files['next.txt'].uploaded_at);
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

describe('canonical Unicode paths (#1326)', () => {
    it('keeps a landed download when a delete retires its old mapped baseline', async () => {
        const nfc = 'café.txt';
        const nfd = 'cafe\u0301.txt';
        let saved: { files: Record<string, { size: number }> } | undefined;
        const { invoke } = makeInvoke({
            load_sync_index_cmd: () => ({
                version: 2, files: { [nfd]: { size: 8, modified: null, is_dir: false } },
                remote_spellings: { [nfd]: nfc },
            }),
            save_sync_index_cmd: args => { saved = args?.index as typeof saved; },
        });
        await runRemoteSync([
            file(nfd, 'download', { size: 19 }),
            file(nfc, 'delete-remote', { baselineKey: nfd }),
        ], noDirs, baseConfig(), {}, noWaitDeps(invoke, { writeIndex: true }));
        expect(saved?.files[nfd]).toMatchObject({ size: 19 });
    });

    it.each(['upload', 'download', 'delete-local', 'delete-remote'] as const)('preserves an independent exact Unicode twin baseline after %s', async action => {
        const nfc = 'café.txt';
        const nfd = 'cafe\u0301.txt';
        let saved: { files: Record<string, { size: number }> } | undefined;
        const { invoke } = makeInvoke({
            load_sync_index_cmd: () => ({
                version: 2, last_sync: '2026-05-22T09:00:00Z', local_path: '/home/u/work', remote_path: '/srv/data',
                files: {
                    [nfc]: { size: 8, modified: null, is_dir: false },
                    [nfd]: { size: 19, modified: null, is_dir: false },
                },
            }),
            save_sync_index_cmd: args => { saved = args?.index as typeof saved; },
        });
        await runRemoteSync([file(nfc, action, { baselineKey: nfc })], noDirs, baseConfig(), {}, noWaitDeps(invoke, { writeIndex: true }));
        expect(saved?.files[nfd]).toMatchObject({ size: 19 });
    });

    it.each(['upload', 'download'] as const)('keeps actual paths for %s, backup, verification, journal and index', async action => {
        const localRel = 'cafe\u0301/a.txt';
        const remoteRel = 'café/a.txt';
        const destination = action === 'upload' ? remoteRel : localRel;
        const source = action === 'upload' ? localRel : remoteRel;
        const { invoke, calls } = makeInvoke({
            sync_backup_run_stamp: () => 'stamp',
            sync_backup_archive_local: () => 'saved',
            sync_backup_archive_remote: () => 'saved',
            verify_local_transfer: () => ({ passed: true }),
        });
        await runRemoteSync([file(destination, action, { sourcePath: source, baselineKey: 'legacy-key', overwritesExisting: true })],
            noDirs, baseConfig({ verifyPolicy: 'size_only', versionedBackup: { dir: '.backup' } }), {}, noWaitDeps(invoke, { writeIndex: true }));
        const transfer = calls.find(c => c.cmd === (action === 'upload' ? 'upload_file' : 'download_file'));
        expect(transfer?.args?.params).toMatchObject({
            local_path: `/home/u/work/${localRel}`, remote_path: `/srv/data/${remoteRel}`,
        });
        const backup = calls.find(c => c.cmd === (action === 'upload' ? 'sync_backup_archive_remote' : 'sync_backup_archive_local'));
        expect(backup?.args?.rel).toBe(destination);
        const journal = lastSavedJournal(calls);
        expect(journal?.entries[0]).toMatchObject({ relative_path: destination, source_path: source, baseline_key: 'legacy-key' });
        expect(filesFromJournal(journal!)[0].baselineKey).toBe('legacy-key');
        if (action === 'download') {
            expect(calls.find(c => c.cmd === 'verify_local_transfer')?.args?.localPath).toBe(`/home/u/work/${localRel}`);
        }
        const index = calls.find(c => c.cmd === 'save_sync_index_cmd');
        const saved = index?.args?.index as {
            files: Record<string, unknown>;
            remote_spellings?: Record<string, string>;
        };
        expect(saved?.files[localRel]).toBeDefined();
        expect(saved?.remote_spellings?.[localRel]).toBe(remoteRel);
    });

    it('indexes a keep-both upload under the suffixed name and leaves the original key', async () => {
        let savedIndex: { files?: Record<string, { size: number }> } | undefined;
        const { invoke } = makeInvoke({
            load_sync_index_cmd: () => ({
                version: 2,
                last_sync: '2026-05-22T09:00:00Z',
                local_path: '/home/u/work',
                remote_path: '/srv/data',
                files: {
                    'report.txt': { size: 10, modified: '2026-05-22T09:00:00Z', is_dir: false },
                },
            }),
            save_sync_index_cmd: (args) => {
                savedIndex = args?.index as { files?: Record<string, { size: number }> };
            },
        });
        await runRemoteSync(
            [file('report.txt.20260522T143012.bak', 'upload', { sourcePath: 'report.txt', size: 12 })],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, { writeIndex: true }),
        );
        expect(savedIndex?.files?.['report.txt']).toMatchObject({ size: 10 });
        expect(savedIndex?.files?.['report.txt.20260522T143012.bak']).toMatchObject({ size: 12 });
        const spellings = (savedIndex as { remote_spellings?: Record<string, string> } | undefined)
            ?.remote_spellings;
        expect(spellings?.['report.txt.20260522T143012.bak']).toBeUndefined();
        expect(spellings?.['report.txt']).toBeUndefined();
    });

    it('drops the proven legacy baseline of a deleted file and leaves other keys', async () => {
        let savedIndex: { files?: Record<string, { size: number }> } | undefined;
        const { invoke } = makeInvoke({
            load_sync_index_cmd: () => ({
                version: 2,
                last_sync: '2026-05-22T09:00:00Z',
                local_path: '/home/u/work',
                remote_path: '/srv/data',
                files: {
                    'café.txt': { size: 8, modified: '2026-05-22T09:00:00Z', is_dir: false },
                    'report.txt': { size: 10, modified: '2026-05-22T09:00:00Z', is_dir: false },
                },
            }),
            save_sync_index_cmd: (args) => {
                savedIndex = args?.index as { files?: Record<string, { size: number }> };
            },
        });
        await runRemoteSync(
            [file('cafe\u0301.txt', 'delete-local', { baselineKey: 'café.txt' })],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, { writeIndex: true }),
        );
        expect(savedIndex?.files?.['café.txt']).toBeUndefined();
        expect(savedIndex?.files?.['cafe\u0301.txt']).toBeUndefined();
        expect(savedIndex?.files?.['report.txt']).toMatchObject({ size: 10 });
    });

    it('drops the stored local key when the delete names the remote spelling', async () => {
        const localRel = 'cafe\u0301.txt';
        const remoteRel = 'café.txt';
        let savedIndex: {
            files?: Record<string, { size: number }>;
            remote_spellings?: Record<string, string>;
        } | undefined;
        const { invoke } = makeInvoke({
            load_sync_index_cmd: () => ({
                version: 2,
                last_sync: '2026-05-22T09:00:00Z',
                local_path: '/home/u/work',
                remote_path: '/srv/data',
                files: {
                    [localRel]: { size: 8, modified: '2026-05-22T09:00:00Z', is_dir: false },
                    'report.txt': { size: 10, modified: '2026-05-22T09:00:00Z', is_dir: false },
                },
                remote_spellings: { [localRel]: remoteRel },
            }),
            save_sync_index_cmd: (args) => {
                savedIndex = args?.index as typeof savedIndex;
            },
        });
        await runRemoteSync(
            [file(remoteRel, 'delete-remote', { baselineKey: localRel })],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, { writeIndex: true }),
        );
        expect(savedIndex?.files?.[localRel]).toBeUndefined();
        expect(savedIndex?.files?.[remoteRel]).toBeUndefined();
        expect(savedIndex?.files?.['report.txt']).toMatchObject({ size: 10 });
        expect(savedIndex?.remote_spellings?.[localRel]).toBeUndefined();
    });

    it('replaces a legacy remote key when the upload uses the local spelling', async () => {
        const localRel = 'cafe\u0301.txt';
        const remoteRel = 'café.txt';
        let savedIndex: {
            files?: Record<string, { size: number }>;
            remote_spellings?: Record<string, string>;
        } | undefined;
        const { invoke } = makeInvoke({
            load_sync_index_cmd: () => ({
                version: 2,
                last_sync: '2026-05-22T09:00:00Z',
                local_path: '/home/u/work',
                remote_path: '/srv/data',
                files: {
                    [remoteRel]: { size: 8, modified: '2026-05-22T09:00:00Z', is_dir: false },
                },
            }),
            save_sync_index_cmd: (args) => {
                savedIndex = args?.index as typeof savedIndex;
            },
        });
        await runRemoteSync(
            [file(remoteRel, 'upload', { sourcePath: localRel, baselineKey: remoteRel, size: 9 })],
            noDirs,
            baseConfig(),
            {},
            noWaitDeps(invoke, { writeIndex: true }),
        );
        expect(savedIndex?.files?.[remoteRel]).toBeUndefined();
        expect(savedIndex?.files?.[localRel]).toMatchObject({ size: 9 });
        expect(savedIndex?.remote_spellings?.[localRel]).toBe(remoteRel);
    });
});


describe('review8 recovery boundaries', () => {
    it('preserves the stored index when loading it fails', async () => {
        const { invoke, calls } = makeInvoke({
            load_sync_index_cmd: () => { throw new Error('index unreadable'); },
        });
        await runRemoteSync([file('one', 'upload')], noDirs, baseConfig(), {}, noWaitDeps(invoke, { writeIndex: true }));
        expect(calls.filter(c => c.cmd === 'save_sync_index_cmd')).toHaveLength(0);
    });

    it('refuses Resume even after a timed-out write completes later', async () => {
        vi.useFakeTimers();
        try {
            let finish!: () => void;
            const pending = new Promise<void>(resolve => { finish = resolve; });
            let saved: SyncJournal | undefined;
            let destination = ''; let indexed = '';
            const { invoke, calls } = makeInvoke({
                upload_file: (_args, attempt) => attempt === 0 ? pending.then(() => { destination = 'old'; }) : (destination = 'new'),
                save_sync_journal_cmd: args => { saved = structuredClone(args!.journal as SyncJournal); },
                save_sync_index_cmd: () => { indexed = destination; },
            });
            const config = baseConfig({ retryPolicy: { ...RETRY, timeout_ms: 10 } });
            const run = runRemoteSync([file('one', 'upload')], noDirs, config, {}, noWaitDeps(invoke, { writeIndex: true }));
            await vi.advanceTimersByTimeAsync(11);
            expect(saved?.resume_safe).toBe(false);
            expect(calls.filter(c => c.cmd === 'save_sync_index_cmd')).toHaveLength(0);
            // The old native command is still live. A fresh Compare/run must
            // not replace its token or publish an index ahead of its write.
            await expect(runRemoteSync([file('one', 'upload')], noDirs, baseConfig(), {}, noWaitDeps(invoke, { writeIndex: true })))
                .rejects.toThrow('SYNC_RUN_BUSY');
            expect(calls.filter(c => c.cmd === 'upload_file')).toHaveLength(1);
            finish();
            await run;
            const unsafe = structuredClone(saved!);
            await expect(runRemoteSync([file('one', 'upload')], noDirs, config, {}, noWaitDeps(invoke, { resumeJournal: saved })))
                .rejects.toThrow('SYNC_RESUME_NEEDS_COMPARE');
            expect(calls.filter(c => c.cmd === 'upload_file')).toHaveLength(1);
            await runRemoteSync([file('one', 'upload')], noDirs, baseConfig(), {}, noWaitDeps(invoke, { writeIndex: true }));
            expect(indexed).toBe('new');
            expect(destination).toBe('new');
            expect(unsafe.resume_safe).toBe(false);
        } finally { vi.useRealTimers(); }
    });
});


describe('durable delete authority', () => {
    it.each(['timeout', 'index-load-error'] as const)('fences completed deletes before a later %s', async failure => {
        vi.useFakeTimers();
        try {
            let saved: SyncJournal | undefined;
            let finish!: () => void; const native = new Promise<void>(resolve => { finish = resolve; });
            const { invoke, calls } = makeInvoke({
                upload_file: () => failure === 'timeout' ? native : undefined,
                load_sync_index_cmd: () => { throw new Error('index temporarily unreadable'); },
                save_sync_journal_cmd: args => { saved = structuredClone(args!.journal as SyncJournal); },
            });
            const run = runRemoteSync([file('one', 'delete-remote'), file('later', 'upload')], noDirs,
                baseConfig({ retryPolicy: { ...RETRY, timeout_ms: 10 } }), {}, noWaitDeps(invoke, { writeIndex: true }));
            await vi.advanceTimersByTimeAsync(11);
            finish();
            await run;
            const deleted = saved?.entries.find(entry => entry.relative_path === 'one');
            expect(deleted?.status).toBe('completed');
            expect(deleted?.intent_token).toBeTruthy();
            expect(calls.filter(c => c.cmd === 'delete_remote_file' && c.args?.path === '/srv/data/one')).toHaveLength(1);
            expect(calls.filter(c => c.cmd === 'save_sync_index_cmd')).toHaveLength(0);
        } finally { vi.useRealTimers(); }
    });
});


describe('full-review owner and report boundaries', () => {
    it('redacts a server error containing both the target marker and a token', async () => {
        const secret='private-token-123';
        const status=vi.fn();
        const {invoke,calls}=makeInvoke({
            upload_file:()=>{throw `server response CONNECTION_CHANGED access_token=${secret}`;},
            provider_gui_target_binding:()=> 'test-owner-v1',
            classify_transfer_error:()=>({kind:'unknown',message:'Transfer refused [redacted]',retryable:false,file_path:'one'}),
        });
        const report=await runRemoteSync([file('one','upload')],noDirs,baseConfig(),{onFileStatus:status},noWaitDeps(invoke));
        expect(calls.some(call=>call.cmd==='classify_transfer_error')).toBe(true);
        expect(JSON.stringify(report)).not.toContain(secret);
        expect(JSON.stringify(status.mock.calls)).not.toContain(secret);
    });

    it.each(['delete', 'backup'] as const)('redacts a token-bearing %s refusal when ownership confirmation fails', async action => {
        const secret = 'delete-token-never-visible';
        const status = vi.fn();
        const fail = () => { throw `CONNECTION_CHANGED access_token=${secret}`; };
        const { invoke } = makeInvoke({
            delete_remote_file: fail, sync_backup_archive_remote: fail,
            provider_gui_target_binding: () => { throw new Error('old owner'); },
        });
        const report = await runRemoteSync([file('one', 'delete-remote')], noDirs,
            baseConfig(action === 'backup' ? { versionedBackup: { dir: '.versions' } } : {}),
            { onFileStatus: status }, noWaitDeps(invoke));
        expect(report.cancelled).toBe(true);
        expect(JSON.stringify(report)).not.toContain(secret);
        expect(JSON.stringify(status.mock.calls)).not.toContain(secret);
    });

    it('cannot publish after an owner switch during the index load', async () => {
        let current=true;let entered!:()=>void;const loading=new Promise<void>(resolve=>{entered=resolve;});
        let resolveIndex!:(value:null)=>void;const pending=new Promise<null>(resolve=>{resolveIndex=resolve;});
        const {invoke,calls}=makeInvoke({load_sync_index_cmd:()=>{entered();return pending;}});
        const run=runRemoteSync([file('one','upload')],noDirs,baseConfig(),{},noWaitDeps(invoke,{
            writeIndex:true,remoteTarget:{epoch:1,ownerBinding:'test-owner-v1',assertCurrent:()=>{if(!current)throw new Error('changed');}},
        }));
        await loading;current=false;resolveIndex(null);const report=await run;
        expect(calls.some(call=>call.cmd==='save_sync_index_cmd')).toBe(false);
        // The transfer landed, but the report must not read as a clean success.
        expect(report.uploaded).toBe(1);
        expect(report.cancelled).toBe(true);
        expect(report.errors).toEqual([{kind:'unknown',message:expect.stringContaining('CONNECTION_CHANGED'),retryable:false,file_path:''}]);
        expect(calls.find(call=>call.cmd==='load_sync_index_cmd')?.args).toMatchObject({expectedGuiTarget:1,ownerBinding:'test-owner-v1'});
    });
});

describe('root pair identity and resumed delete history', () => {
    it.each([['/', '/'], ['C:/', '/']] as const)('keeps roots %s and %s in durable commands without double-slash file paths', async (localRoot, remoteRoot) => {
        let journal: SyncJournal | undefined; let index: SyncIndex | undefined;
        const { invoke, calls } = makeInvoke({
            save_sync_journal_cmd: args => { journal = structuredClone(args!.journal as SyncJournal); },
            save_sync_index_cmd: args => { index = structuredClone(args!.index as SyncIndex); },
        });
        const deps = noWaitDeps(invoke, { writeIndex: true }); const backend = deps.invoke;
        const roots: Array<Record<string, unknown>> = [];
        deps.invoke = async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
            if (cmd === 'sync_intent_snapshot_cmd' || cmd === 'sync_intent_begin_cmd') {
                if (!args?.localPath || !args?.remotePath) throw new Error('absolute pair paths required');
                roots.push(args!);
            }
            return backend<T>(cmd, args);
        };
        const report = await runRemoteSync([file('one', 'upload')], noDirs, baseConfig({ localRoot, remoteRoot }), {}, deps);
        expect(report.uploaded).toBe(1);
        expect(roots.length).toBeGreaterThanOrEqual(2);
        for (const root of roots) expect(root).toMatchObject({ localPath: localRoot, remotePath: remoteRoot });
        expect(journal).toMatchObject({ local_path: localRoot, remote_path: remoteRoot });
        expect(index).toMatchObject({ local_path: localRoot, remote_path: remoteRoot });
        expect(calls.find(call => call.cmd === 'upload_file')?.args).toMatchObject({ params: {
            local_path: localRoot === '/' ? '/one' : 'C:/one', remote_path: '/one',
        } });
    });

    it('removes a completed mapped delete baseline on Resume before retiring its intent', async () => {
        let journal: SyncJournal | undefined;
        let index: SyncIndex = { version: 2, last_sync: '', local_path: '/home/u/work', remote_path: '/srv/data',
            files: { 'old-local': { size: 1024, modified: null, is_dir: false } }, remote_spellings: { 'old-local': 'old-remote' } };
        const remoteFiles = new Set(['old-remote']); let savedTokens: unknown;
        const { invoke, calls } = makeInvoke({
            delete_remote_file: args => { if (args?.path === '/srv/data/old-remote') remoteFiles.delete('old-remote'); },
            upload_file: (_args, attempt) => { if (attempt === 0) throw new Error('temporary upload failure'); },
            classify_transfer_error: args => ({ kind: 'unknown', message: String(args?.rawError), retryable: false, file_path: 'later' }),
            save_sync_journal_cmd: args => { journal = structuredClone(args!.journal as SyncJournal); },
            load_sync_index_cmd: (_args, attempt) => { if (attempt === 0) throw new Error('index temporarily unavailable'); return index; },
            save_sync_index_cmd: args => { index = structuredClone(args!.index as SyncIndex); savedTokens = args!.intentTokens; },
        });
        const config = baseConfig({ retryPolicy: { ...RETRY, max_retries: 0 } });
        await runRemoteSync([file('old-remote', 'delete-remote', { baselineKey: 'old-local' }), file('later', 'upload')],
            noDirs, config, {}, noWaitDeps(invoke, { writeIndex: true }));
        expect(journal?.completed).toBe(false);
        const resume = structuredClone(journal!);
        const deleted = resume.entries.find(entry => entry.action === 'delete')!;
        expect(deleted.status).toBe('completed'); expect(deleted.intent_token).toBeTruthy();
        expect(index.files['old-local']).toBeDefined();
        remoteFiles.add('old-remote'); // Recreated by another writer before Resume.
        const report = await runRemoteSync(filesFromJournal(resume), noDirs, config, {},
            noWaitDeps(invoke, { resumeJournal: resume, writeIndex: true }));
        expect(report.uploaded).toBe(1);
        expect(index.files['old-local']).toBeUndefined();
        expect(index.remote_spellings?.['old-local']).toBeUndefined();
        expect(index.files.later).toBeDefined();
        expect(savedTokens).toContain(deleted.intent_token);
        expect(remoteFiles.has('old-remote')).toBe(true);
        expect(calls.filter(call => call.cmd === 'delete_remote_file' && call.args?.path === '/srv/data/old-remote')).toHaveLength(1);
    });
});

// C2: the sync-start gate. A hard refusal stops the run (no question, no
// launch), whatever the profile's opt-out; otherwise an eligible session or
// an opted-out question launches, and an unavailable one asks.
describe('deltaGateDecision', () => {
    it.each([
        [{ eligible: false, hard_error: 'PinnedKeyMismatch' }, false, 'stop'],
        [{ eligible: false, hard_error: 'PinnedKeyMismatch' }, true, 'stop'],
        [{ eligible: true, hard_error: 'HostKeyRejected' }, true, 'stop'],
        [{ eligible: true, hard_error: null }, false, 'launch'],
        [{ eligible: false, hard_error: null }, true, 'launch'],
        [{ eligible: false }, false, 'ask'],
    ] as const)('%j with skip=%s: %s', (verdict, skip, expected) => {
        expect(deltaGateDecision(verdict, skip)).toBe(expected);
    });
});

// D2: the gate around the probe. The generation read at the request is
// compared before every way out; a changed connection launches nothing,
// and a failed probe launches nothing either (fail closed).
describe('runDeltaGate', () => {
    type Verdict = Awaited<ReturnType<DeltaGateDeps['probe']>>;
    const eligible: Verdict = { eligible: true, reason: null, hard_error: null, server_identity: null };
    const unavailable: Verdict = { eligible: false, reason: 'no rsync', hard_error: null, server_identity: null };

    /** A gate whose probe answers only when the test releases it. */
    const gate = (verdict: Verdict | Error, skip: boolean) => {
        let generation = 1;
        let release!: () => void;
        const held = new Promise<void>((resolve) => { release = resolve; });
        let onContinue: (() => void) | undefined;
        const deps = {
            requested: Promise.resolve(1),
            probeDelta: true,
            generation: vi.fn(async () => generation),
            skipPrompt: vi.fn(async () => skip),
            probe: vi.fn(async () => {
                await held;
                if (verdict instanceof Error) throw verdict;
                return verdict;
            }),
            launch: vi.fn(),
            refuse: vi.fn(),
            failed: vi.fn(),
            changed: vi.fn(),
            ask: vi.fn((_verdict: Verdict, answer: () => void) => { onContinue = answer; }),
        } satisfies DeltaGateDeps;
        return { deps, switchConnection: () => { generation += 1; }, release, answer: () => onContinue?.() };
    };

    it.each([
        ['an opted-out profile', unavailable, true],
        ['an eligible verdict', eligible, false],
        ['a failed probe', new Error('probe failed'), false],
        ['an unavailable verdict (no question shown)', unavailable, false],
    ] as const)('launches nothing and asks nothing after a switch, for %s', async (_case, verdict, skip) => {
        const g = gate(verdict, skip);
        const pending = runDeltaGate(g.deps);
        await vi.waitFor(() => expect(g.deps.probe).toHaveBeenCalled());
        g.switchConnection();
        g.release();
        await pending;
        expect(g.deps.launch).not.toHaveBeenCalled();
        expect(g.deps.ask).not.toHaveBeenCalled();
        expect(g.deps.failed).not.toHaveBeenCalled();
        expect(g.deps.changed).toHaveBeenCalledTimes(1);
    });

    it('launches nothing from Continue answered after a switch', async () => {
        const g = gate(unavailable, false);
        g.release();
        await runDeltaGate(g.deps);
        expect(g.deps.ask).toHaveBeenCalledTimes(1);
        g.switchConnection();
        g.answer();
        await vi.waitFor(() => expect(g.deps.changed).toHaveBeenCalledTimes(1));
        expect(g.deps.launch).not.toHaveBeenCalled();
    });

    it('fails closed when the probe command fails, on the same connection', async () => {
        const g = gate(new Error('probe failed'), true);
        g.release();
        await runDeltaGate(g.deps);
        expect(g.deps.failed).toHaveBeenCalledTimes(1);
        expect(g.deps.launch).not.toHaveBeenCalled();
    });

    it('checks a run that is not probed against the generation before it launches (E3)', async () => {
        const switched = gate(eligible, false);
        switched.switchConnection();
        await runDeltaGate({ ...switched.deps, probeDelta: false });
        expect(switched.deps.probe).not.toHaveBeenCalled();
        expect(switched.deps.launch).not.toHaveBeenCalled();
        expect(switched.deps.changed).toHaveBeenCalledTimes(1);
        const same = gate(eligible, false);
        await runDeltaGate({ ...same.deps, probeDelta: false });
        expect(same.deps.probe).not.toHaveBeenCalled();
        expect(same.deps.launch).toHaveBeenCalledTimes(1);
    });

    it('launches on the same connection, and stops on a refusal even without a profile', async () => {
        for (const [verdict, skip] of [[eligible, false], [unavailable, true]] as const) {
            const g = gate(verdict, skip);
            g.release();
            await runDeltaGate(g.deps);
            expect(g.deps.launch).toHaveBeenCalledTimes(1);
        }
        const answered = gate(unavailable, false);
        answered.release();
        await runDeltaGate(answered.deps);
        answered.answer();
        await vi.waitFor(() => expect(answered.deps.launch).toHaveBeenCalledTimes(1));
        // An ad-hoc session (no saved profile: the question is off) still
        // stops on a hard refusal.
        const adHoc = gate({ ...unavailable, hard_error: 'PinnedKeyMismatch' }, true);
        adHoc.release();
        await runDeltaGate(adHoc.deps);
        expect(adHoc.deps.refuse).toHaveBeenCalledWith('PinnedKeyMismatch');
        expect(adHoc.deps.launch).not.toHaveBeenCalled();
    });

    // CodeRabbit 4573911803, the complement: saved profiles that cannot be
    // read have no place to keep an answer either, so the question is not
    // asked; the probe and a hard refusal still apply.
    it('skips the question when the saved profiles cannot be read, and still stops on a refusal', async () => {
        for (const [verdict, launches] of [[unavailable, true], [{ ...unavailable, hard_error: 'PinnedKeyMismatch' }, false]] as const) {
            const g = gate(verdict, false);
            g.deps.skipPrompt.mockRejectedValueOnce(new Error('saved profiles unreadable'));
            g.release();
            await runDeltaGate(g.deps);
            expect(g.deps.probe).toHaveBeenCalledTimes(1);
            expect(g.deps.ask).not.toHaveBeenCalled();
            expect(g.deps.launch).toHaveBeenCalledTimes(launches ? 1 : 0);
            expect(g.deps.refuse).toHaveBeenCalledTimes(launches ? 0 : 1);
        }
    });
});

/**
 * E1: App's own sync actions, run from its source with their collaborators
 * injected. The connection generation the gate compares is read by the
 * action at its start, before the plan is built and before a canary trial
 * and its approval; a run never reads a fresh one for itself.
 */
describe('the sync actions read the generation at the action (E1)', () => {
    const block = (start: string, end: string) => {
        const a = APP.indexOf(start);
        const b = APP.indexOf(end, a + start.length);
        if (a < 0 || b < 0) throw new Error(`Missing block: ${start}`);
        return APP.slice(a, b);
    };
    const ACTIONS = ['executeSyncPresetPlan', 'handleCompareMirrorLeftToRight', 'handleCompareMirrorRightToLeft', 'handleResumeJournal'] as const;
    type Action = (typeof ACTIONS)[number];
    let compiled: string | undefined;
    const compile = async () => {
        compiled ??= (await transformWithOxc([
            // #1561's GUI target capture, which the comparison's capture uses.
            block('  const captureGuiTarget = async', '  const getOverwriteDestination ='),
            block('  const connectionAtAction = useLatestCallback(', '  // GAP-10: shared local-local sync launcher.'),
            block('  const handleCompareMirrorLeftToRight = useCallback(', '  const { dismissJournal: handleDismissJournal'),
            block('  const executeSyncPresetPlan = useLatestCallback(', '  const clipboardCut'),
        ].join('\n'), 'sync-actions.ts')).code;
        return compiled;
    };
    const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
    const FILE = { relative_path: 'a.txt', action: 'upload' };
    /** A journal #1561's resume accepts: a safe policy, and the session's
     *  saved profile as its owner. */
    const JOURNAL = { direction: 'local_to_remote', verify_policy: 'size_only', retry_policy: undefined, resume_safe: true, profile_id: 'p1', entries: [] };

    /** App's actions on one session (SFTP unless named) whose generation
     *  and protocol the test moves. */
    const station = async (protocol = 'sftp', options: {
        verdict?: { eligible: boolean; reason: string | null; hard_error: string | null; server_identity: null };
        saved?: () => Promise<unknown[]>;
    } = {}) => {
        let generation = 1;
        const profile = { protocol, savedProfileId: 'p1', name: 'S' };
        const order: string[] = [];
        let finishCanary!: (result: unknown) => void;
        let approve: (() => void) | undefined;
        const plan = <T>(value: T) => { order.push('plan'); return value; };
        const ctx: Record<string, unknown> = {
            useLatestCallback: <F>(fn: F) => fn,
            useCallback: <F>(fn: F) => fn,
            activeUnifiedRemoteProfile: profile,
            invoke: vi.fn((command: string) => {
                if (command === 'provider_connection_generation') {
                    order.push('generation');
                    return Promise.resolve(generation);
                }
                if (command === 'sftp_probe_delta_eligibility') {
                    return Promise.resolve(options.verdict ?? { eligible: true, reason: null, hard_error: null, server_identity: null });
                }
                if (command === 'sync_canary_run') return new Promise((resolve) => { finishCanary = resolve; });
                // #1561's target binding: one target throughout (this suite
                // moves the connection generation, not the target).
                if (command === 'provider_gui_target_epoch') return Promise.resolve(7);
                if (command === 'provider_gui_target_binding') return Promise.resolve('owner-p1');
                return Promise.reject(new Error(`unexpected ${command}`));
            }),
            notify: { info: vi.fn(), warning: vi.fn(), error: vi.fn() },
            t: (key: string) => key,
            usesProviderApi: () => true,
            localStorage: { getItem: () => null },
            currentLocalPath: '/local',
            currentRemotePath: '/remote',
            isCryptOverlayActive: () => false,
            remoteSyncRunningRef: { current: false },
            batchCancelledRef: { current: false },
            cancelLevelRef: { current: 0 },
            window: { addEventListener: vi.fn(), removeEventListener: vi.fn() },
            TRANSFER_EVENT_BRIDGE: 'transfer-event',
            runRemoteSync: vi.fn(async () => ({ uploaded: 0, downloaded: 0, deleted: 0 })),
            syncRunQueueBridge: () => () => undefined,
            transferQueue: {},
            setRemoteSyncResult: vi.fn(),
            loadRemoteFiles: vi.fn(async () => undefined),
            loadLocalFiles: vi.fn(async () => undefined),
            setSyncCancelling: vi.fn(),
            runDeltaGate,
            debugMode: false,
            loadSavedServerProfiles: options.saved ?? (async () => []),
            setDeltaEligibilityPrompt: vi.fn(),
            aeroSync: { context: { pairKind: 'local-remote', activeProfileId: 'p1' }, connection: null as unknown },
            setAeroSync: vi.fn(),
            buildMirrorSyncInput: () => plan({ files: [FILE], dirs: { remote: [], local: [] } }),
            buildRemoteSyncInput: () => plan({ files: [FILE], dirs: { remote: [], local: [] } }),
            filesFromJournal: () => plan([FILE]),
            runLocalLocalSync: vi.fn(),
            SPEED_PRESETS: { normal: { deltaSyncEnabled: false } },
            canaryRunningRef: { current: false },
            setCanaryResult: (shown: { onApprove: () => void } | null) => { approve = shown?.onApprove; },
            // #1561: the target, its preflight and the batch and transfer
            // counters a run checks before it launches.
            snapshotQueuedTarget: () => ({ profileId: 'p1', params: { protocol: profile.protocol } }),
            queuedTargetGuard: () => () => undefined,
            QueuedTargetChangedError: class QueuedTargetChangedError extends Error {},
            stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 },
            syncPreflightPendingRef: { current: null }, setSyncPreflightPending: vi.fn(),
            batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null },
            syncCancelPendingRef: { current: null }, syncRunnerPendingRef: { current: null },
            runTrackedSync: async (_ref: unknown, run: () => Promise<unknown>) => run(),
            syncRefreshAbortRef: { current: null }, normalizedSyncRoot: (root: string) => root,
        };
        const state = ctx.aeroSync as { connection: unknown };
        const actions = new Function(...Object.keys(ctx), `${await compile()}; return { ${ACTIONS.join(', ')}, connectionAtAction };`)(
            ...Object.values(ctx),
        ) as Record<Action, (...args: unknown[]) => void> & { connectionAtAction: () => unknown };
        // The dialog's comparison, as `openAeroSync` builds its state: the
        // connection every plan of this state comes from, stored with it.
        state.connection = actions.connectionAtAction();
        return {
            actions,
            ctx: ctx as { runRemoteSync: ReturnType<typeof vi.fn>; notify: { error: ReturnType<typeof vi.fn> }; invoke: ReturnType<typeof vi.fn> },
            order,
            /** Another connection: a new generation, and its protocol. */
            switchConnection: (to = profile.protocol) => { generation += 1; profile.protocol = to; },
            finishCanary: () => finishCanary({ sampled: 1 }),
            approve: () => approve?.(),
            approvalShown: () => approve !== undefined,
            state,
        };
    };
    const runtime = (canary: boolean) => ({
        speedMode: 'normal',
        verifyPolicy: 'size_only',
        retryPolicy: undefined,
        transferBudget: 0,
        versionedBackup: null,
        errorCorrection: null,
        canary: canary ? { percent: 10, selection: 'random' } : undefined,
    });
    const preset = { preset: 'mirror', direction: 'left-to-right' };

    it('launches nothing when the connection switches while the canary approval waits', async () => {
        const s = await station();
        s.actions.executeSyncPresetPlan(preset, runtime(true));
        s.finishCanary();
        await vi.waitFor(() => expect(s.approvalShown()).toBe(true));
        // The plan was built for generation 1; the user switches, then approves.
        s.switchConnection();
        s.approve();
        await vi.waitFor(() => expect(s.ctx.notify.error).toHaveBeenCalledWith('aerosync.title', 'toast.remoteTargetChanged'));
        await tick();
        expect(s.ctx.runRemoteSync).not.toHaveBeenCalled();
    });

    it('launches the approved canary run on the same connection', async () => {
        const s = await station();
        s.actions.executeSyncPresetPlan(preset, runtime(true));
        s.finishCanary();
        await vi.waitFor(() => expect(s.approvalShown()).toBe(true));
        s.approve();
        await vi.waitFor(() => expect(s.ctx.runRemoteSync).toHaveBeenCalledTimes(1));
        expect(s.ctx.notify.error).not.toHaveBeenCalled();
    });

    it.each([
        ['the preset run', (s: Awaited<ReturnType<typeof station>>) => s.actions.executeSyncPresetPlan(preset, runtime(false))],
        ['the left-to-right mirror', (s: Awaited<ReturnType<typeof station>>) => s.actions.handleCompareMirrorLeftToRight([{ relativePath: 'a.txt' }])],
        ['the right-to-left mirror', (s: Awaited<ReturnType<typeof station>>) => s.actions.handleCompareMirrorRightToLeft([{ relativePath: 'a.txt' }])],
        ['the journal resume', (s: Awaited<ReturnType<typeof station>>) => s.actions.handleResumeJournal(JOURNAL)],
    ] as const)('%s reads the generation before it builds its plan, and stops after a switch', async (_name, act) => {
        const s = await station();
        act(s);
        expect(s.order.slice(0, 2)).toEqual(['generation', 'plan']);
        // No wait in this action before the gate: the switch lands while the
        // gate probes, and the run is still the earlier connection's.
        s.switchConnection();
        await vi.waitFor(() => expect(s.ctx.notify.error).toHaveBeenCalledWith('aerosync.title', 'toast.remoteTargetChanged'));
        expect(s.ctx.runRemoteSync).not.toHaveBeenCalled();
        // The same action on an unchanged connection runs.
        const same = await station();
        act(same);
        await vi.waitFor(() => expect(same.ctx.runRemoteSync).toHaveBeenCalledTimes(1));
    });

    // CodeRabbit 4573911803: the question keeps its answer in a saved
    // profile. An ad-hoc session (its fallback id, here `p1`, names no saved
    // profile) and an unreadable profile list run without it; a saved
    // profile that keeps the question asks it.
    it.each([
        ['an ad-hoc session', async () => [], false],
        ['an unreadable profile list', async () => { throw new Error('saved profiles unreadable'); }, false],
        ['a saved profile that keeps the question', async () => [{ id: 'p1' }], true],
    ] as const)('asks about continuing without delta for %s only when a saved profile can keep the answer', async (_name, saved, asks) => {
        const unavailable = { eligible: false, reason: 'no rsync', hard_error: null, server_identity: null };
        const s = await station('sftp', { verdict: unavailable, saved });
        const prompt = (s.ctx as unknown as { setDeltaEligibilityPrompt: ReturnType<typeof vi.fn> }).setDeltaEligibilityPrompt;
        s.actions.handleCompareMirrorLeftToRight([{ relativePath: 'a.txt' }]);
        if (asks) {
            await vi.waitFor(() => expect(prompt).toHaveBeenCalledTimes(1));
            expect(s.ctx.runRemoteSync).not.toHaveBeenCalled();
        } else {
            await vi.waitFor(() => expect(s.ctx.runRemoteSync).toHaveBeenCalledTimes(1));
            expect(prompt).not.toHaveBeenCalled();
        }
        // The probe carries the comparison's GUI target epoch (#1561).
        expect(s.ctx.invoke).toHaveBeenCalledWith('sftp_probe_delta_eligibility', { expectedGuiTarget: 7 });
    });

    it('every call of runConnectedRemoteSync is one of these actions, with its generation', () => {
        const calls = APP.split('runConnectedRemoteSync(').length - 1;
        expect(calls).toBe(4);
        const run = block('  const runConnectedRemoteSync = useLatestCallback(', '  // GAP-10: shared local-local sync launcher.');
        // Its only read is the gate's "now"; the "requested" one is the caller's.
        expect(run.split("'provider_connection_generation'").length - 1).toBe(1);
        expect(run).toContain("generation: () => invoke<number>('provider_connection_generation'),");
        expect(run).toContain('requested: connection.generation,');
        // E3: no launch path around the gate (its `launch` is the one call of
        // `launchRun`, and only while the preflight is still this run's),
        // and the probe follows the action's protocol.
        expect(run.split('launchRun()').length - 1).toBe(1);
        expect(run).toContain('launch: () => {\n            if (!ownsPreflight()) return finishPreflight();\n            launched = true;\n            void launchRun().finally(finishPreflight);');
        expect(run).toContain("probeDelta: protocol === 'sftp',");
        expect(run).toContain('const protocol = connection.protocol;');
    });

    // P1: the connection moves after the compare and before the click (an
    // overlay armed or cleared, a reconnect: the dialog stays open). Every
    // action's plan comes from that compare, so nothing launches.
    it.each([
        ['the preset run', (s: Awaited<ReturnType<typeof station>>) => s.actions.executeSyncPresetPlan(preset, runtime(false))],
        ['the left-to-right mirror', (s: Awaited<ReturnType<typeof station>>) => s.actions.handleCompareMirrorLeftToRight([{ relativePath: 'a.txt' }])],
        ['the right-to-left mirror', (s: Awaited<ReturnType<typeof station>>) => s.actions.handleCompareMirrorRightToLeft([{ relativePath: 'a.txt' }])],
        ['the journal resume', (s: Awaited<ReturnType<typeof station>>) => s.actions.handleResumeJournal(JOURNAL)],
    ] as const)('%s launches nothing after a switch between the compare and the click', async (_name, act) => {
        const s = await station();
        s.switchConnection();
        act(s);
        await vi.waitFor(() => expect(s.ctx.notify.error).toHaveBeenCalledWith('aerosync.title', 'toast.remoteTargetChanged'));
        await tick();
        expect(s.ctx.runRemoteSync).not.toHaveBeenCalled();
    });

    // P1: a Rescan builds a new state with its own capture; the click on
    // the entries still displayed acts on the state they belong to, whose
    // connection is the earlier one: nothing launches.
    it('launches nothing from the entries of a comparison whose rescan started on another connection', async () => {
        const s = await station();
        const earlier = s.state.connection;
        // The rescan on B: openAeroSync captures B into the NEW state only.
        s.switchConnection();
        const rescan = { connection: s.actions.connectionAtAction() };
        expect(rescan.connection).not.toBe(earlier);
        expect(s.state.connection).toBe(earlier);
        s.actions.handleCompareMirrorLeftToRight([{ relativePath: 'a.txt' }]);
        await vi.waitFor(() => expect(s.ctx.notify.error).toHaveBeenCalledWith('aerosync.title', 'toast.remoteTargetChanged'));
        await tick();
        expect(s.ctx.runRemoteSync).not.toHaveBeenCalled();
    });

    it.each([
        ['no connection captured', (): unknown => null],
        ['a generation read that rejected', (s: { actions: { connectionAtAction: () => unknown } }): unknown => {
            const generation = Promise.reject(new Error('read failed'));
            generation.catch(() => undefined);
            return { ...(s.actions.connectionAtAction() as object), generation };
        }],
    ] as const)('fails closed with %s', async (_name, capture) => {
        for (const act of ACTIONS) {
            const s = await station();
            s.state.connection = capture(s);
            if (act === 'executeSyncPresetPlan') s.actions.executeSyncPresetPlan(preset, runtime(false));
            else if (act === 'handleResumeJournal') s.actions.handleResumeJournal(JOURNAL);
            else s.actions[act]([{ relativePath: 'a.txt' }]);
            await vi.waitFor(() => expect(s.ctx.notify.error).toHaveBeenCalledWith('aerosync.title', 'toast.remoteTargetChanged'));
            await tick();
            expect(s.ctx.runRemoteSync, act).not.toHaveBeenCalled();
        }
    });

    it('stores the connection with the comparison openAeroSync builds, and every action reads it there', () => {
        const open = block('  const openAeroSync = useLatestCallback(', '  // GAP-5: shared connected-remote sync launcher.');
        expect(open).toContain('const connection = connectionAtAction();');
        // Replaced together: the remote state carries this call's capture,
        // the others none; nothing else writes it.
        const states = open.split('\n').filter(line => /^\s+connection(: null)?,$/.test(line));
        expect(states.map(line => line.trim()).sort()).toEqual(['connection,', 'connection: null,', 'connection: null,']);
        expect(APP).not.toMatch(/\.connection = /);
        expect(APP.split('const requestedConnection = aeroSync?.connection ?? null;').length - 1).toBe(4);
        // The context hook keeps the comparison's own connection.
        expect(APP).toContain('useAeroSyncContextActions(aeroSync, setAeroSyncComparison, aeroSyncCompareSeqRef);');
        expect(APP).toContain("return next ? { ...next, connection: prev?.connection ?? null } : null;");
        // No fallback to a read at the click: no capture, no run.
        const run = block('  const runConnectedRemoteSync = useLatestCallback(', '  // GAP-10: shared local-local sync launcher.');
        expect(run).toContain('const connection = opts.requestedConnection;\n    if (!connection) {');
    });

    // E3: the connection switches to another protocol while the canary
    // approval waits; the earlier plan must not launch on it.
    it.each([
        ['SFTP to FTP', 'sftp', 'ftp'],
        ['FTP to SFTP', 'ftp', 'sftp'],
        ['FTP to S3', 'ftp', 's3'],
    ] as const)('launches nothing after a %s switch while the canary approval waits', async (_name, from, to) => {
        const s = await station(from);
        s.actions.executeSyncPresetPlan(preset, runtime(true));
        s.finishCanary();
        await vi.waitFor(() => expect(s.approvalShown()).toBe(true));
        s.switchConnection(to);
        s.approve();
        await vi.waitFor(() => expect(s.ctx.notify.error).toHaveBeenCalledWith('aerosync.title', 'toast.remoteTargetChanged'));
        await tick();
        expect(s.ctx.runRemoteSync).not.toHaveBeenCalled();
        // The probe is chosen by the action's protocol, not the new one.
        const probed = s.ctx.invoke.mock.calls.some(([command]) => command === 'sftp_probe_delta_eligibility');
        expect(probed).toBe(from === 'sftp');
    });

    it.each(['ftp', 's3'])('launches an approved %s canary run on the same connection', async (protocol) => {
        const s = await station(protocol);
        s.actions.executeSyncPresetPlan(preset, runtime(true));
        s.finishCanary();
        await vi.waitFor(() => expect(s.approvalShown()).toBe(true));
        s.approve();
        await vi.waitFor(() => expect(s.ctx.runRemoteSync).toHaveBeenCalledTimes(1));
        expect(s.ctx.notify.error).not.toHaveBeenCalled();
        expect(s.ctx.invoke.mock.calls.some(([command]) => command === 'sftp_probe_delta_eligibility')).toBe(false);
    });
});

/**
 * CodeRabbit 4573911765: what a provider reports while it connects (a GitHub
 * working branch an earlier version made, with commits the protected branch
 * lacks) reaches the user of the page. App's own `runConnect`, from its
 * source: a `provider_connect` answer's warnings are each shown once, titled
 * with the existing warning key, and kept in the activity log; none, none
 * shown; an older backend's plain string holds none.
 */
describe('a connect shows what its provider reported', () => {
    const LEGACY = 'GitHub: a working branch named aeroftp/alice-smith/main, as earlier AeroFTP versions named it, has commits the protected branch does not.';
    const station = async (answers: unknown[]) => {
        const a = APP.indexOf('  // What a provider reported while it connected');
        const b = APP.indexOf('  // Esc aborts an in-progress connection.', a);
        if (a < 0 || b < 0) throw new Error('Missing block: runConnect');
        const { code } = await transformWithOxc(APP.slice(a, b), 'run-connect.ts');
        const queue = [...answers];
        const ctx = {
            useCallback: <F>(fn: F) => fn,
            cancellableConnect: (run: (token: string) => Promise<unknown>) => run('token-1'),
            wentOnOverScpRef: { current: false },
            invoke: vi.fn(async () => queue.shift()),
            notify: { warning: vi.fn() },
            humanLog: { log: vi.fn() },
            t: (key: string) => key,
        };
        const runConnect = new Function(...Object.keys(ctx), `${code}; return runConnect;`)(...Object.values(ctx)) as
            (command: string, params: Record<string, unknown>) => Promise<unknown>;
        return { runConnect, ctx };
    };

    it('shows a pending legacy branch once, and keeps it in the activity log', async () => {
        const s = await station([{ message: 'Connected to repo via github', warnings: [LEGACY] }]);
        await expect(s.runConnect('provider_connect', { protocol: 'github' })).resolves.toEqual({ message: 'Connected to repo via github', warnings: [LEGACY] });
        expect(s.ctx.notify.warning).toHaveBeenCalledTimes(1);
        expect(s.ctx.notify.warning).toHaveBeenCalledWith('common.warning', LEGACY);
        expect(s.ctx.humanLog.log).toHaveBeenCalledTimes(1);
        expect(s.ctx.humanLog.log).toHaveBeenCalledWith('CONNECT', `common.warning: ${LEGACY}`, 'pending');
    });

    it.each([
        ['no warning', { message: 'Connected', warnings: [] }],
        ['an older backend (a plain string)', 'Connected to repo via github'],
        ['no answer', undefined],
    ] as const)('shows nothing for %s', async (_name, answer) => {
        const s = await station([answer]);
        await expect(s.runConnect('provider_connect', {})).resolves.toEqual(answer);
        expect(s.ctx.notify.warning).not.toHaveBeenCalled();
        expect(s.ctx.humanLog.log).not.toHaveBeenCalled();
    });

    it('shows a warning only with the connect that reported it, never again on a reconnect', async () => {
        const s = await station([{ message: 'Connected', warnings: [LEGACY] }, { message: 'Connected', warnings: [] }]);
        await s.runConnect('provider_connect', {});
        await s.runConnect('provider_connect', {});
        expect(s.ctx.notify.warning).toHaveBeenCalledTimes(1);
    });

    it('reads warnings from provider_connect only', async () => {
        const s = await station([{ warnings: [LEGACY] }]);
        await s.runConnect('connect_ftp', {});
        expect(s.ctx.notify.warning).not.toHaveBeenCalled();
    });
});
