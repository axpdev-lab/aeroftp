// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { runCompletionRefresh, runTrackedSync } from './completionRefresh';
import { createFileBatchDispatcher, rearmedOnUserAction } from '../components/transferQueueActions';
import { localPanelShows } from './localSelection';
import { wrapIntentInvoke } from './testFixtures/syncIntents';
import { describe, expect, it, vi } from 'vitest';
import { normalizedSyncRoot, runRemoteSync, type RemoteSyncConfig, type SyncRunFile } from './remoteSyncRunner';
import { QueuedTargetChangedError } from './queuedTarget';
import type { SyncJournal } from '../types';

const file = (relativePath: string, action: SyncRunFile['action'] = 'upload', extra: Partial<SyncRunFile> = {}): SyncRunFile =>
    ({ relativePath, action, size: 10, mtime: null, ...extra });

const config = (extra: Partial<RemoteSyncConfig> = {}): RemoteSyncConfig => ({
    localRoot: '/local', remoteRoot: '/remote', isProvider: true, isFtp: false,
    retryPolicy: { max_retries: 2, base_delay_ms: 1, max_delay_ms: 1, timeout_ms: 0, backoff_multiplier: 1 },
    verifyPolicy: 'none', deltaSyncEnabled: false, transferBudget: 0, direction: 'bidirectional', ...extra,
});

const remote = new Set(['provider_bulk_support', 'provider_sync_transfer_bulk', 'provider_mkdir',
    'provider_upload_file', 'provider_download_file', 'provider_delete_file', 'sync_backup_archive_remote',
    'sync_remove_empty_dir', 'sync_ec_generate', 'sync_ec_verify_repair']);

type Trigger = 'first-file' | 'mkdir' | 'retry' | 'archive' | 'sidecar' | 'bulk' | 'directory';
function harness(trigger: Trigger) {
    let current: 'A' | 'B' = 'A';
    const operations = { A: [] as string[], B: [] as string[] };
    const attempts: string[] = [];
    let journal: SyncJournal | undefined;
    const invoke = async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
        if (cmd === 'save_sync_journal_cmd') journal = structuredClone(args?.journal) as SyncJournal;
        if (cmd === 'sync_backup_run_stamp') return 'stamp' as T;
        if (cmd === 'classify_transfer_error') return { kind: 'unknown', message: 'temporary', retryable: true } as T;
        if (remote.has(cmd)) {
            attempts.push(cmd);
            // The backend acquires the ownership gate and checks the epoch
            // before selecting its provider; a rejected call never touches B.
            if (args?.expectedGuiTarget !== 1 || current !== 'A') throw 'CONNECTION_CHANGED';
            operations[current].push(cmd);
            if (cmd === 'provider_bulk_support') return { upload: trigger === 'bulk', download: false } as T;
            if (trigger === 'bulk' && cmd === 'provider_sync_transfer_bulk') {
                current = 'B';
                throw 'CONNECTION_CHANGED';
            }
            if (trigger === 'retry' && cmd === 'provider_upload_file') throw new Error('temporary');
            if ((trigger === 'first-file' && cmd === 'provider_upload_file')
                || (trigger === 'mkdir' && cmd === 'provider_mkdir')
                || (trigger === 'archive' && cmd === 'sync_backup_archive_remote')
                || (trigger === 'directory' && cmd === 'sync_remove_empty_dir')
                || (trigger === 'sidecar' && cmd === 'provider_delete_file' && String(args?.path).endsWith('.aerocorrect'))) current = 'B';
            if (cmd === 'sync_backup_archive_remote') return '/kept' as T;
            if (cmd === 'sync_remove_empty_dir') return 'removed' as T;
        }
        return undefined as T;
    };
    return {
        operations, attempts, journal: () => journal,
        deps: {
            invoke: wrapIntentInvoke(invoke),
            remoteTarget: { ownerBinding: 'test-owner-v1', epoch: 1, assertCurrent: () => { if (current !== 'A') throw new QueuedTargetChangedError('target changed'); } },
            delay: async () => { if (trigger === 'retry') current = 'B'; },
            now: () => 0, makeId: () => 'target-test', writeIndex: true,
        },
    };
}

describe('remote sync target ownership', () => {
    it.each([
        ['first-file', [file('one'), file('two')], {}],
        ['mkdir', [file('nested/one'), file('nested/two')], {}],
        ['retry', [file('one'), file('two')], {}],
        ['archive', [file('one', 'upload', { overwritesExisting: true }), file('two')], { versionedBackup: { dir: '.versions' } }],
        ['sidecar', [file('one', 'delete-remote'), file('two')], {}],
        ['bulk', [file('one'), file('two')], {}],
        ['directory', [file('one', 'delete-remote', { isDir: true }), file('two', 'delete-remote', { isDir: true })], {}],
    ] as const)('stops without touching B after a change during %s', async (trigger, files, extra) => {
        const h = harness(trigger);
        const report = await runRemoteSync([...files], { remote: [], local: [] }, config(extra), {}, h.deps);
        expect(h.operations.B).toEqual([]);
        expect(report.cancelled).toBe(true);
        expect(report.errors.some(error => !error.retryable)).toBe(true);
        expect(h.journal()?.completed).toBe(false);
        if (trigger === 'bulk') expect(h.attempts.filter(cmd => cmd === 'provider_upload_file')).toEqual([]);
        if (trigger === 'retry') expect(h.attempts.filter(cmd => cmd === 'provider_upload_file')).toHaveLength(1);
    });

    it('requires remote ownership and leaves local-local runs independent', async () => {
        const invoke = wrapIntentInvoke(async <T,>() => undefined as T);
        await expect(runRemoteSync([], { remote: [], local: [] }, config(), {}, { invoke })).rejects.toThrow('bound target');
        const report = await runRemoteSync([], { remote: [], local: [] }, config({ isLocalLocal: true }), {}, { invoke: wrapIntentInvoke(invoke) });
        expect(report.cancelled).toBe(false);
    });

    it.each([
        [null, true],
        [{ dir: '.versions' }, true],
    ])('records whether the run policy can be reconstructed (%s)', async (versionedBackup, safe) => {
        const h = harness('first-file');
        await runRemoteSync([file('one')], { remote: [], local: [] },
            config({ versionedBackup }), {}, h.deps);
        expect(h.journal()?.resume_safe).toBe(safe);
    });

    it('does not retry a timed-out command that may still be running', async () => {
        vi.useFakeTimers();
        try {
            const h = harness('first-file');
            const calls: string[] = [];
            let finish!: () => void;
            const native = new Promise<void>(resolve => { finish = resolve; });
            const invoke: typeof h.deps.invoke = async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
                if (cmd === 'provider_upload_file') {
                    calls.push(cmd);
                    await native;
                    return undefined as T;
                }
                return h.deps.invoke<T>(cmd, args);
            };
            const pending = runRemoteSync([file('one'), file('two')], { remote: [], local: [] },
                config({ retryPolicy: { ...config().retryPolicy, timeout_ms: 50 } }), {}, { ...h.deps, invoke });
            await vi.advanceTimersByTimeAsync(100);
            finish();
            const report = await pending;
            expect(calls).toHaveLength(1);
            expect(report.cancelled).toBe(true);
            expect(report.errors[0].retryable).toBe(false);
            expect(h.journal()?.completed).toBe(false);
        } finally {
            vi.useRealTimers();
        }
    });

    it('leaves the upload resumable when the target changes before EC generation', async () => {
        const h = harness('first-file');
        const report = await runRemoteSync([file('one')], { remote: [], local: [] },
            config({ errorCorrection: { enabled: true, pct: 10 } }), {}, h.deps);
        expect(report.cancelled).toBe(true);
        expect(h.operations.B).toEqual([]);
        expect(h.journal()?.entries[0].status).toBe('transferred');
        expect(h.journal()?.completed).toBe(false);
    });
});

it.each([undefined, 'B'])('refuses a resume journal with owner %s on profile A', async (owner) => {
    const { transformWithOxc } = await import('vite');
    const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('  const runConnectedRemoteSync =');
    const end = app.indexOf('  // GAP-10: shared local-local sync launcher', start);
    const { code } = await transformWithOxc(app.slice(start, end), 'aerosync-resume.ts');
    const notify = { error: vi.fn(), info: vi.fn() };
    const context = {
        useLatestCallback: (fn: unknown) => fn, stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 }, syncPreflightPendingRef: { current: null as symbol | null }, setSyncPreflightPending: vi.fn(), batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null },
        snapshotQueuedTarget: () => ({ profileId: 'A', params: { protocol: 'sftp' } }),
        remoteJournalOwnersRef: { current: new Map([['j', { profileId: 'A', params: { protocol: 'sftp' } }]]) },
        queuedTargetProblem: () => null,
        queuedTargetGuard: () => () => {}, notify, t: (key: string) => key,
    };
    const launch = new Function(...Object.keys(context), `${code}; return runConnectedRemoteSync;`)(...Object.values(context));
    launch([file('one')], { remote: [], local: [] }, { profileId: 'A', resumeJournal: { id: 'j', profile_id: owner } });
    expect(notify.error).toHaveBeenCalledWith('aerosync.title', 'toast.remoteTargetChanged');
});

it('requires backend verification of the persisted owner before any resumed mutation', async () => {
    const h = harness('first-file');
    await runRemoteSync([file('one')], { remote: [], local: [] }, config({ profileId: 'A' }), { isCancelled: () => true }, h.deps);
    const journal = h.journal()!;
    const before = [...h.operations.A];
    const verify = vi.fn();
    await expect(runRemoteSync([file('one')], { remote: [], local: [] }, config({ profileId: 'A' }), {}, {
        ...h.deps, resumeJournal: journal,
        invoke: async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
            if (cmd === 'provider_gui_verify_resume_owner') { verify(args); throw new Error('CONNECTION_CHANGED'); }
            return h.deps.invoke<T>(cmd, args);
        },
    })).rejects.toThrow('CONNECTION_CHANGED');
    expect(verify).toHaveBeenCalledWith(expect.objectContaining({ binding: journal.owner_binding, expectedGuiTarget: 1 }));
    expect(h.operations.A).toEqual(before);
    expect(h.operations.B).toEqual([]);
});

it.each([undefined, false])('refuses a journal without safe resume policy (%s)', async (resumeSafe) => {
    const { transformWithOxc } = await import('vite');
    const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('  const handleResumeJournal =');
    const end = app.indexOf('  const { dismissJournal:', start);
    const { code } = await transformWithOxc(app.slice(start, end), 'aerosync-resume-policy.ts');
    const notify = { error: vi.fn() };
    const run = vi.fn();
    const context = {
        useCallback: (fn: unknown) => fn,
        aeroSync: { context: { pairKind: 'local-remote' } }, setAeroSync: vi.fn(),
        notify, t: (key: string) => key,
        runConnectedRemoteSync: run, runLocalLocalSync: run,
    };
    const resume = new Function(...Object.keys(context), `${code}; return handleResumeJournal;`)(...Object.values(context));
    resume({ resume_safe: resumeSafe, entries: [] });
    expect(run).not.toHaveBeenCalled();
    expect(context.setAeroSync).not.toHaveBeenCalled();
    expect(notify.error).toHaveBeenCalledWith('aerosync.title', 'syncPanel.resumeNeedsCompare');
});

it('passes the saved backup and EC policy through the actual resume handler', async () => {
    const { transformWithOxc } = await import('vite');
    const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('  const handleResumeJournal =');
    const end = app.indexOf('  const { dismissJournal:', start);
    const { code } = await transformWithOxc(app.slice(start, end), 'aerosync-resume-policy.ts');
    const run = vi.fn();
    const context = {
        useCallback: (fn: unknown) => fn,
        aeroSync: { context: { pairKind: 'local-remote', activeProfileId: 'A' } }, setAeroSync: vi.fn(),
        notify: { error: vi.fn() }, t: (key: string) => key,
        runConnectedRemoteSync: run, runLocalLocalSync: vi.fn(), filesFromJournal: () => [],
    };
    const resume = new Function(...Object.keys(context), `${code}; return handleResumeJournal;`)(...Object.values(context));
    resume({ resume_safe: true, intent_protocol: 1, entries: [], backup_dir: '.v', ec_enabled: true, ec_pct: 12, ec_max_file_size: 1024 });
    expect(run).toHaveBeenCalledWith([], { remote: [], local: [] }, expect.objectContaining({
        versionedBackup: { dir: '.v' }, errorCorrection: { enabled: true, pct: 12, maxFileSize: 1024 },
    }));
});

// The prompt retains App's actual launch closure, not a substitute helper.
it('does not launch the App delta continuation after a tab switch', async () => {
    const { transformWithOxc } = await import('vite');
    const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('  const runConnectedRemoteSync =');
    const end = app.indexOf('  // GAP-10: shared local-local sync launcher', start);
    expect(start).toBeGreaterThan(0);
    const captureStart = app.indexOf('  const captureGuiTarget =');
    const captureEnd = app.indexOf('  const getOverwriteDestination =', captureStart);
    const { code } = await transformWithOxc(app.slice(captureStart, captureEnd) + app.slice(start, end), 'aerosync-launch.ts');
    let current = 'A';
    let prompt: { onContinue: () => void } | undefined;
    const runner = vi.fn();
    const invoke = vi.fn(async (cmd: string) => {
        if (cmd === 'provider_gui_target_epoch') return 7;
        if (cmd === 'provider_gui_target_binding') return 'test-owner-v1';
        if (cmd === 'sftp_probe_delta_eligibility') return { eligible: false, reason: 'unavailable' };
        throw new Error(`unexpected ${cmd}`);
    });
    const context = {
        useLatestCallback: (fn: unknown) => fn, stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 }, syncPreflightPendingRef: { current: null as symbol | null }, setSyncPreflightPending: vi.fn(), batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null },
        snapshotQueuedTarget: () => ({ profileId: 'A', params: { protocol: 'sftp' } }),
        queuedTargetGuard: (target: { profileId: string }) => () => {
            if (current !== target.profileId) throw new QueuedTargetChangedError('target changed');
        },
        QueuedTargetChangedError,
        activeUnifiedRemoteProfile: { protocol: 'sftp', savedProfileId: 'A', name: 'A' },
        currentLocalPath: '/local', currentRemotePath: '/remote', usesProviderApi: () => true,
        localStorage: { getItem: () => null }, isCryptOverlayActive: () => false,
        invoke, loadSavedServerProfiles: async () => [{ id: 'A' }],
        setDeltaEligibilityPrompt: (value: typeof prompt) => { prompt = value; },
        remoteSyncRunningRef: { current: false }, syncCancelPendingRef: { current: null }, syncRunnerPendingRef: { current: null }, runTrackedSync, notify: { error: vi.fn(), info: vi.fn() },
        t: (key: string) => key, debugMode: false, runRemoteSync: runner,
    };
    const launch = new Function(...Object.keys(context), `${code}; return runConnectedRemoteSync;`)(...Object.values(context));
    launch([file('one')], { remote: [], local: [] }, { direction: 'local_to_remote', deltaSyncEnabled: true, verifyPolicy: 'none', profileId: 'A' });
    await vi.waitFor(() => expect(prompt).toBeDefined());
    expect(invoke).toHaveBeenCalledWith('sftp_probe_delta_eligibility', { expectedGuiTarget: 7 });
    current = 'B';
    prompt!.onContinue();
    expect(runner).not.toHaveBeenCalled();
    await vi.waitFor(() => expect(context.notify.error).toHaveBeenCalled());
});

// CodeRabbit 4558710258: a pinned session key that is not the login key is a
// hard refusal at the sync-start gate, fresh or cached (both answer the same
// typed verdict), and also with the "do not ask again" opt-out. The run stops
// there: no runner, no prompt to continue, nothing written.
it.each([['fresh', false, false], ['cached', false, false], ['opted out of the prompt', true, false], ['saved profiles unreadable, delta off', false, true]] as const)('stops the run at the sync-start gate on a hard delta refusal (%s)', async (_label, skipPrompt, profilesFail) => {
    const { transformWithOxc } = await import('vite');
    const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('  const runConnectedRemoteSync =');
    const end = app.indexOf('  // GAP-10: shared local-local sync launcher', start);
    const captureStart = app.indexOf('  const captureGuiTarget =');
    const captureEnd = app.indexOf('  const getOverwriteDestination =', captureStart);
    const { code } = await transformWithOxc(app.slice(captureStart, captureEnd) + app.slice(start, end), 'aerosync-hard-gate.ts');
    let prompt: unknown;
    const runner = vi.fn();
    const hard = 'native hard rejection (PinnedKeyMismatch): the pinned key is not the login key';
    const invoke = vi.fn(async (cmd: string) => {
        if (cmd === 'provider_gui_target_epoch') return 7;
        if (cmd === 'provider_gui_target_binding') return 'test-owner-v1';
        if (cmd === 'sftp_probe_delta_eligibility') return { eligible: false, reason: hard, hard_error: hard, server_identity: null };
        throw new Error(`unexpected ${cmd}`);
    });
    const context = {
        useLatestCallback: (fn: unknown) => fn, stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 }, syncPreflightPendingRef: { current: null as symbol | null }, setSyncPreflightPending: vi.fn(), batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null },
        snapshotQueuedTarget: () => ({ profileId: 'A', params: { protocol: 'sftp' } }),
        queuedTargetGuard: () => () => undefined, QueuedTargetChangedError,
        activeUnifiedRemoteProfile: { protocol: 'sftp', savedProfileId: 'A', name: 'A' },
        currentLocalPath: '/local', currentRemotePath: '/remote', usesProviderApi: () => true,
        localStorage: { getItem: () => null }, isCryptOverlayActive: () => false,
        invoke, loadSavedServerProfiles: async () => { if (profilesFail) throw new Error('profiles unreadable'); return [{ id: 'A', skipDeltaEligibilityPrompt: skipPrompt }]; },
        setDeltaEligibilityPrompt: (value: unknown) => { prompt = value; },
        remoteSyncRunningRef: { current: false }, syncCancelPendingRef: { current: null }, syncRunnerPendingRef: { current: null }, runTrackedSync, notify: { error: vi.fn(), info: vi.fn() },
        t: (key: string) => key, debugMode: false, runRemoteSync: runner,
    };
    const launch = new Function(...Object.keys(context), `${code}; return runConnectedRemoteSync;`)(...Object.values(context));
    launch([file('one')], { remote: [], local: [] }, { direction: 'local_to_remote', deltaSyncEnabled: !profilesFail, verifyPolicy: 'none', profileId: 'A' });
    await vi.waitFor(() => expect(context.notify.error).toHaveBeenCalledWith('aerosync.title', hard));
    await tick();
    expect(invoke).toHaveBeenCalledWith('sftp_probe_delta_eligibility', { expectedGuiTarget: 7 });
    expect(runner).not.toHaveBeenCalled();
    expect(prompt).toBeUndefined();
    expect(context.syncPreflightPendingRef.current).toBeNull();
});

it('keeps the preset target and roots captured before the canary approval', async () => {
    const { transformWithOxc } = await import('vite');
    const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('  const executeSyncPresetPlan =');
    const end = app.indexOf('  const clipboardCut =', start);
    const { code } = await transformWithOxc(app.slice(start, end), 'aerosync-canary.ts');
    let current = 'A';
    let prompt: { onApprove: () => void } | undefined;
    const run = vi.fn();
    const context = {
        useLatestCallback: (fn: unknown) => fn, stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 }, syncPreflightPendingRef: { current: null as symbol | null }, setSyncPreflightPending: vi.fn(), batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null },
        aeroSync: { context: { pairKind: 'local-remote', activeProfileId: 'A' } },
        setAeroSync: vi.fn(), debugMode: false,
        snapshotQueuedTarget: () => ({ profileId: current, params: { protocol: 'sftp' } }),
        queuedTargetGuard: (target: { profileId: string }) => () => {
            if (target.profileId !== current) throw new QueuedTargetChangedError('target changed');
        },
        prepareGuiTarget: (check: () => void) => { check(); return Promise.resolve({ epoch: 7 }); },
        currentLocalPath: '/local-A', currentRemotePath: '/remote-A', isCryptOverlayActive: () => false,
        buildRemoteSyncInput: () => ({ files: [file('one')], dirs: { remote: [], local: [] } }),
        SPEED_PRESETS: { balanced: { deltaSyncEnabled: false } }, runConnectedRemoteSync: run,
        runLocalLocalSync: vi.fn(), canaryRunningRef: { current: false }, normalizedSyncRoot,
        invoke: async () => ({ sample: [] }),
        setCanaryResult: (value: typeof prompt) => { prompt = value; },
        notify: { error: vi.fn(), info: vi.fn() }, t: (key: string) => key,
    };
    const launch = new Function(...Object.keys(context), `${code}; return executeSyncPresetPlan;`)(...Object.values(context));
    launch({ preset: 'mirror', direction: 'left-to-right' }, { speedMode: 'balanced', verifyPolicy: 'none', canary: { percent: 10, selection: 'first' } });
    await vi.waitFor(() => expect(prompt).toBeDefined());
    current = 'B';
    context.currentLocalPath = '/local-B';
    context.currentRemotePath = '/remote-B';
    prompt!.onApprove();
    const options = run.mock.calls[0][2];
    expect(options.target.profileId).toBe('A');
    expect(options.localRoot).toBe('/local-A');
    expect(options.remoteRoot).toBe('/remote-A');
    expect(await options.preparedTarget).toEqual({ epoch: 7 });
});


it('stops on a typed bulk owner change with an unchanged UI and journals confirmed completions', async () => {
    const calls: string[] = []; let saved: SyncJournal | undefined;
    const invoke = async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
        calls.push(cmd);
        if (cmd === 'save_sync_journal_cmd') saved = structuredClone(args!.journal as SyncJournal);
        if (cmd === 'provider_bulk_support') return { upload: true, download: false } as T;
        if (cmd === 'provider_sync_transfer_bulk') return [{ status: 'completed' }, { status: 'target_changed' }, { status: 'not_attempted' }] as T;
        return undefined as T;
    };
    const report = await runRemoteSync([file('one'), file('two'), file('three')], { remote: [], local: [] }, config(), {}, {
        invoke: wrapIntentInvoke(invoke), remoteTarget: { ownerBinding: 'test-owner-v1', epoch: 1, assertCurrent: () => {} }, makeId: () => 'bulk-owner', delay: async () => {},
    });
    expect(report.cancelled).toBe(true);
    expect(calls.filter(cmd => cmd === 'provider_sync_transfer_bulk')).toHaveLength(1);
    expect(calls).not.toContain('provider_upload_file');
    expect(saved!.entries.map(entry => entry.status)).toEqual(['transferred', 'failed', 'pending']);
    expect(saved!.completed).toBe(false);
});

it('validates the backend auth owner before archiving the next download destination', async () => {
    let authChanged = false; const archives: unknown[] = []; const downloads: unknown[] = [];
    const invoke = async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
        if (cmd === 'provider_bulk_support') return null as T;
        if (cmd === 'sync_backup_run_stamp') return 'stamp' as T;
        if (cmd === 'sync_backup_archive_local') {
            expect(args?.expectedGuiTarget).toBe(1);
            expect(args?.isProvider).toBe(true);
            if (authChanged) throw 'CONNECTION_CHANGED';
            archives.push(args?.rel); return '/kept' as T;
        }
        if (cmd === 'provider_download_file') { downloads.push(args); authChanged = true; }
        return undefined as T;
    };
    const report = await runRemoteSync([file('one', 'download', { overwritesExisting: true }), file('two', 'download', { overwritesExisting: true })],
        { remote: [], local: [] }, config({ versionedBackup: { dir: '.v' } }), {}, {
            invoke: wrapIntentInvoke(invoke), remoteTarget: { ownerBinding: 'test-owner-v1', epoch: 1, assertCurrent: () => {} }, makeId: () => 'download-owner', delay: async () => {},
        });
    expect(report.cancelled).toBe(true);
    expect(downloads).toHaveLength(1);
    expect(archives).toEqual(['one']);
});

it.each([false, true])('runs an unsaved quick-connect sync with an epoch owner (resume=%s)', async resume => {
    const { transformWithOxc } = await import('vite');
    const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('  const runConnectedRemoteSync =');
    const end = app.indexOf('  // GAP-10: shared local-local sync launcher', start);
    const captureStart = app.indexOf('  const captureGuiTarget =');
    const captureEnd = app.indexOf('  const getOverwriteDestination =', captureStart);
    const { code } = await transformWithOxc(app.slice(captureStart, captureEnd) + app.slice(start, end), 'quick-sync.ts');
    const runner = vi.fn((..._args: unknown[]) => new Promise(() => {}));
    const notify = { error: vi.fn(), info: vi.fn() };
    const context = {
        useLatestCallback: (fn: unknown) => fn, stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 }, syncPreflightPendingRef: { current: null as symbol | null }, setSyncPreflightPending: vi.fn(), batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null },
        snapshotQueuedTarget: () => ({ profileId: null, params: { protocol: 'sftp' } }),
        queuedTargetGuard: () => () => {}, QueuedTargetChangedError,
        activeUnifiedRemoteProfile: { id: 'unsaved server name', protocol: 'sftp', savedProfileId: null },
        currentLocalPath: '/local', currentRemotePath: '/remote', usesProviderApi: () => true,
        localStorage: { getItem: () => null }, isCryptOverlayActive: () => false,
        invoke: vi.fn(async (cmd: string) => cmd === 'provider_gui_target_epoch' ? 7 : 'epoch-v1:test:7'),
        loadSavedServerProfiles: async () => [], remoteSyncRunningRef: { current: false }, syncCancelPendingRef: { current: null }, syncRunnerPendingRef: { current: null }, runTrackedSync,
        batchCancelledRef: { current: false }, cancelLevelRef: { current: 0 },
        window: { addEventListener: vi.fn() }, TRANSFER_EVENT_BRIDGE: 'test-transfer',
        syncRunQueueBridge: () => ({}), transferQueue: {},
        notify, t: (key: string) => key, debugMode: false, runRemoteSync: runner,
    };
    const launch = new Function(...Object.keys(context), `${code}; return runConnectedRemoteSync;`)(...Object.values(context));
    launch([file('one')], { remote: [], local: [] }, {
        direction: 'local_to_remote', deltaSyncEnabled: false, verifyPolicy: 'none', profileId: 'unsaved server name',
        ...(resume ? { resumeJournal: { profile_id: null, owner_binding: 'epoch-v1:test:7' } } : {}),
    });
    await vi.waitFor(() => expect(runner).toHaveBeenCalledOnce());
    expect(runner.mock.calls[0][2]).toMatchObject({ profileId: null });
    expect(notify.error).not.toHaveBeenCalled();
});


it.each(['owner','folder'] as const)('discards a local sync refresh after a deferred %s change', async change => {
    const { transformWithOxc }=await import('vite');const {default:app}=await import('../App.tsx?raw');
    const start=app.indexOf('  const loadLocalFiles =');const end=app.indexOf('  const loadLocalFiles2 =',start);
    const {code}=await transformWithOxc(app.slice(start,end),'owned-local-refresh.ts');
    let current=true;let release!:(value:unknown[])=>void;const pending=new Promise<unknown[]>(resolve=>{release=resolve;});
    const context={useCallback:(fn:unknown)=>fn,invoke:()=>pending,showHiddenFiles:false,
        loadLocalCallIdRef:{current:0},loadLocalPendingRef:{current:null as number | null},loadLocalWaitersRef:{current:new Set<() => void>()},currentLocalPathRef:{current:'/A'},setLocalFiles:vi.fn(),setCurrentLocalPath:vi.fn(),setSelectedLocalFiles:vi.fn(),
        notify:{error:vi.fn()},t:(key:string)=>key,activityLog:{log:vi.fn()},isGvfsMtpPath:()=>false,needsRemovableMediaPlug:()=>false,
    };
    const load=new Function(...Object.keys(context),`${code};return loadLocalFiles;`)(...Object.values(context));
    const result=load('/A',{check:()=>{if(!current)throw new QueuedTargetChangedError('changed');}});
    if(change==='owner')current=false;else context.currentLocalPathRef.current='/B';
    release([]);
    if(change==='owner')await expect(result).rejects.toThrow('changed');else expect(await result).toBe(false);
    expect(context.setLocalFiles).not.toHaveBeenCalled();expect(context.setCurrentLocalPath).not.toHaveBeenCalled();
});

it('stops the sync refresh continuation when remote refresh discards a switched target', async () => {
    const { transformWithOxc }=await import('vite');const {default:app}=await import('../App.tsx?raw');
    const start=app.indexOf('          if (!report.cancelled && (report.uploaded > 0');
    const end=app.indexOf('\n        } catch (err)',start);
    const {code}=await transformWithOxc(`async function refreshAfterSync(){${app.slice(start,end)}}`,'sync-refresh-continuation.ts');
    let current=true;let release!:(value:null)=>void;const pending=new Promise<null>(resolve=>{release=resolve;});
    const context={runCompletionRefresh,syncRefreshAbortRef:{current:null},batchCancelledRef:{current:false},report:{cancelled:false,uploaded:1,downloaded:0,deleted:0},epoch:1,checkTarget:()=>{if(!current)throw new Error('changed');},
        loadRemoteFiles:()=>pending,loadLocalFiles:vi.fn(),runConfig:{localRoot:'/old'},currentLocalPathRef:{current:'/old'},localPanelShows,loadLocalPendingRef:{current:null},loadLocalWaitersRef:{current:new Set<() => void>()},isLocalFlattenSearchActive:()=>false,
    };
    const refresh=new Function(...Object.keys(context),`${code};return refreshAfterSync;`)(...Object.values(context));
    const result=refresh();current=false;release(null);await expect(result).rejects.toThrow('changed');
    expect(context.loadLocalFiles).not.toHaveBeenCalled();
});

it('refreshes local changes after a remote listing failure while ownership remains current', async () => {
    const { transformWithOxc } = await import('vite'); const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('          if (!report.cancelled && (report.uploaded > 0');
    const end = app.indexOf('\n        } catch (err)', start);
    const { code } = await transformWithOxc(`async function refreshAfterSync(){${app.slice(start, end)}}`, 'sync-remote-listing-failure.ts');
    const context = {
        runCompletionRefresh, syncRefreshAbortRef: { current: null }, batchCancelledRef: { current: false },
        report: { cancelled: false, uploaded: 0, downloaded: 1, deleted: 1 }, epoch: 1, checkTarget: vi.fn(),
        loadRemoteFiles: vi.fn(async () => null), loadLocalFiles: vi.fn(), runConfig: { localRoot: '/A' },
        currentLocalPathRef: { current: '/A' }, localPanelShows, loadLocalPendingRef: { current: null },
        loadLocalWaitersRef: { current: new Set<() => void>() }, isLocalFlattenSearchActive: () => false,
    };
    const refresh = new Function(...Object.keys(context), `${code};return refreshAfterSync;`)(...Object.values(context));
    await refresh();
    expect(context.loadLocalFiles).toHaveBeenCalledWith('/A', { check: expect.any(Function) });
    expect(context.syncRefreshAbortRef.current).toBe(null);
});

it.each(['result', 'error'] as const)('keeps a GVfs phone connected when an aborted refresh later returns a %s', async outcome => {
    vi.useFakeTimers();
    try {
        const { transformWithOxc } = await import('vite'); const { default: app } = await import('../App.tsx?raw');
        const start = app.indexOf('  const loadRemoteFiles = useLatestCallback(');
        const end = app.indexOf('\n  // Transfer events (backend progress updates)', start);
        const { code } = await transformWithOxc(app.slice(start, end), 'late-mtp-refresh.ts');
        let resolve!: (value: unknown) => void; let reject!: (reason: unknown) => void;
        const pending = new Promise((ok, fail) => { resolve = ok; reject = fail; });
        const context = {
            useLatestCallback: (fn: unknown) => fn, stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 }, syncPreflightPendingRef: { current: null as symbol | null }, setSyncPreflightPending: vi.fn(), batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null }, remoteRefreshStartedAtRef: { current: 0 }, remoteNavCounter: { current: 1 },
            liveConnectionParams: { protocol: 'mtp' }, usesProviderApi: () => true, aeroVaultOverlaySession: null,
            logger: { debug: vi.fn() }, invoke: vi.fn(() => pending), humanLog: { logRaw: vi.fn() },
            activityLog: { log: vi.fn() }, applyRemoteFileList: vi.fn(), QueuedTargetChangedError,
            isListingCancelled: () => false, mtpGvfsMountPathRef: { current: '/run/user/1000/gvfs/mtp:host=phone' }, isGvfsMtpPath: () => true,
            setActivePortableDeviceId: vi.fn(), setIsConnected: vi.fn(), setRemoteFiles: vi.fn(), setCurrentRemotePath: vi.fn(),
            setCurrentRemoteDisplayPath: vi.fn(), setSessions: vi.fn(), setActiveSessionId: vi.fn(), setShowRemotePanel: vi.fn(),
            setShowConnectionScreen: vi.fn(), setActivePanel: vi.fn(), notify: { error: vi.fn() }, t: (key: string) => key,
            setRemoteListLoading: vi.fn(), setRemoteListReason: vi.fn(),
        };
        const load = new Function(...Object.keys(context), `${code};return loadRemoteFiles;`)(...Object.values(context));
        const controller = new AbortController(); let listing!: Promise<unknown>;
        const refresh = runCompletionRefresh(signal => {
            listing = load(undefined, true, null, { epoch: 1, check: () => signal.throwIfAborted() });
            return listing.then(() => undefined);
        }, controller);
        await vi.advanceTimersByTimeAsync(2001);
        await refresh;
        expect(controller.signal.aborted).toBe(true);
        if (outcome === 'result') resolve({ current_path: '/', files: [] }); else reject(new Error('Slow phone IO'));
        await expect(listing).resolves.toBe(null);
        expect(context.invoke).toHaveBeenCalledExactlyOnceWith('provider_list_files', { path: null, expectedGuiTarget: 1 });
        expect(context.mtpGvfsMountPathRef.current).toContain('mtp:host=phone');
        expect(context.setSessions).not.toHaveBeenCalled();
        expect(context.setIsConnected).not.toHaveBeenCalled();
        expect(context.applyRemoteFileList).not.toHaveBeenCalled();
        expect(context.notify.error).not.toHaveBeenCalled();
    } finally { vi.useRealTimers(); }
});

it.each(['settled', 'stopped', 'retargeted', 'removed', 'cancelled', 'completed'] as const)('gates a queued executor on cancellation or runner exit and rechecks %s', async outcome => {
    const { transformWithOxc } = await import('vite'); const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('  const boundToQueuedServer =');
    const end = app.indexOf('\n  /** Whether two connections', start);
    const { code } = await transformWithOxc(app.slice(start, end), 'queue-after-cancellation.ts');
    let settle!: () => void; const pending = new Promise<void>(resolve => { settle = resolve; }); let problem: string | null = null;
    let row: { status: string } | undefined = { status: 'pending' };
    const context = { useLatestCallback: (fn: unknown) => fn, syncCancelPendingRef: { current: null as Promise<void> | null },
        syncRunnerPendingRef: { current: pending as Promise<void> | null }, runTrackedSync,
        stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 }, syncPreflightPendingRef: { current: null as symbol | null }, setSyncPreflightPending: vi.fn(), batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null }, queuedTargetProblem: () => problem, transferQueue: { getItemNow: () => row, failTransfer: vi.fn(), cancelTransfer: vi.fn() }, t: (key: string) => key };
    const bind = new Function(...Object.keys(context), `${code};return boundToQueuedServer;`)(...Object.values(context));
    const executor = vi.fn(); const retry = bind({}, 'retry', executor)();
    expect(executor).not.toHaveBeenCalled();
    if (outcome === 'stopped') context.stopCountRef.current++;
    if (outcome === 'retargeted') problem = 'target changed';
    if (outcome === 'removed') row = undefined;
    if (outcome === 'cancelled') row = { status: 'error' };
    if (outcome === 'completed') row = { status: 'completed' };
    context.syncRunnerPendingRef.current = null; settle(); await retry;
    expect(executor).toHaveBeenCalledTimes(outcome === 'settled' ? 1 : 0);
    if (outcome === 'retargeted') expect(context.transferQueue.failTransfer).toHaveBeenCalledWith('retry', 'target changed');
    // A Stop during the wait settles the still-pending row instead of leaving it.
    if (outcome === 'stopped') expect(context.transferQueue.cancelTransfer).toHaveBeenCalledWith('retry', 'transfer.cancelledByUser');
    else expect(context.transferQueue.cancelTransfer).not.toHaveBeenCalled();
});

it('rejects an overlapping tracked runner without replacing the first runner or losing Stop', async () => {
    let finish!: () => void; const pending = new Promise<void>(resolve => { finish = resolve; });
    const pendingRef = { current: null as Promise<void> | null }; let cancelled = false;
    const nextDelete = vi.fn();
    const first = runTrackedSync(pendingRef, async () => {
        await pending;
        if (!cancelled) nextDelete();
        return { cancelled };
    });
    const firstExit = pendingRef.current;
    const second = vi.fn(async () => undefined);
    await expect(runTrackedSync(pendingRef, second)).rejects.toThrow('Sync runner already active');
    expect(second).not.toHaveBeenCalled();
    expect(pendingRef.current).toBe(firstExit);
    cancelled = true; finish();
    await expect(first).resolves.toEqual({ cancelled: true });
    expect(nextDelete).not.toHaveBeenCalled();
    expect(pendingRef.current).toBe(null);
});

it('does not rearm Retry after native Stop acknowledgement until the actual sync runner exits', async () => {
    const { transformWithOxc } = await import('vite'); const { default: app } = await import('../App.tsx?raw');
    const runStart = app.indexOf('  const runConnectedRemoteSync =');
    const workerStart = app.indexOf('      void (async () => {', runStart);
    const workerEnd = app.indexOf('      })();', workerStart) + '      })();'.length;
    const workerCode = (await transformWithOxc(app.slice(workerStart, workerEnd).replace('void (async', 'return (async'), 'stopped-runner-exit.ts')).code;
    let transferDone!: () => void; const transfer = new Promise<void>(resolve => { transferDone = resolve; });
    let transferEntered!: () => void; const entered = new Promise<void>(resolve => { transferEntered = resolve; });
    let nativeDone!: () => void; const native = new Promise<void>(resolve => { nativeDone = resolve; });
    const batchCancelledRef = { current: false }; const cancelLevelRef = { current: 0 }; const commands: string[] = [];
    const invoke = wrapIntentInvoke(async <T,>(command: string): Promise<T> => {
        commands.push(command);
        if (command === 'provider_bulk_support') return { upload: false, download: false } as T;
        if (command === 'provider_upload_file') {
            transferEntered(); await transfer;
            expect(batchCancelledRef.current).toBe(true);
        }
        if (command === 'cancel_transfer') await native;
        return undefined as T;
    });
    const retried = vi.fn();
    const context = {
        runTrackedSync, runCompletionRefresh, runRemoteSync, runFiles: [file('first'), file('later', 'delete-remote')],
        runDirs: { local: [], remote: [] }, runConfig: config(), opts: {}, invoke, deltaStats: new Map(),
        batchCancelledRef, cancelLevelRef, epoch: 1, ownerBinding: 'test-owner', checkTarget: vi.fn(),
        syncRunQueueBridge: () => vi.fn(), setRemoteSyncResult: vi.fn(),
        syncRunnerPendingRef: { current: null as Promise<void> | null }, syncCancelPendingRef: { current: null as Promise<void> | null },
        syncRefreshAbortRef: { current: null }, remoteSyncRunningRef: { current: true }, activeTransferRef: { current: null },
        stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 }, syncPreflightPendingRef: { current: null as symbol | null }, setSyncPreflightPending: vi.fn(), batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null }, loadLocalWaitersRef: { current: new Set<() => void>() }, batchResumeResolverRef: { current: null },
        transferQueue: { getItemNow: () => ({ id: 'retry', status: 'error', restored: false }), items: [{ id: 'retry', status: 'error', restored: false }], retryItem: vi.fn(), stopAll: vi.fn(), hasActiveTransfersNow: () => true },
        retryCallbacksRef: { current: new Map([['retry', rearmedOnUserAction({ batchCancelled: batchCancelledRef, cancelLevel: cancelLevelRef }, retried)]]) },
        resumeRestoredTransfers: vi.fn(), setSyncCancelling: vi.fn(), dispatchTransferToast: vi.fn(), setIsBatchPaused: vi.fn(), setBatchPauseReason: vi.fn(),
        window: { removeEventListener: vi.fn() }, TRANSFER_EVENT_BRIDGE: 'transfer', onTransferEvent: vi.fn(),
        notify: { error: vi.fn() }, t: (key: string) => key,
    };
    const run = new Function(...Object.keys(context), workerCode)(...Object.values(context)) as Promise<void>;
    await Promise.race([entered, run.then(() => {
        throw new Error(`Runner exited before upload: ${JSON.stringify({ commands, errors: context.notify.error.mock.calls, reports: context.setRemoteSyncResult.mock.calls })}`);
    })]);
    const cancelStart = app.indexOf('  const cancelTransfer = async');
    const cancelEnd = app.indexOf('  // Circuit breaker: attempt reconnection', cancelStart);
    const cancelCode = (await transformWithOxc(app.slice(cancelStart, cancelEnd), 'stop-before-runner-exit.ts')).code;
    const cancel = new Function(...Object.keys(context), `${cancelCode};return cancelTransfer;`)(...Object.values(context));
    const stopping = cancel(); nativeDone(); await stopping;
    expect(context.syncCancelPendingRef.current).toBe(null);
    expect(context.syncRunnerPendingRef.current).not.toBe(null);
    const retryStart = app.indexOf('onRetryItem={async (id: string) => {');
    const retryEnd = app.indexOf('\n          onStartItem=', retryStart);
    const retrySource = app.slice(retryStart, retryEnd).trim().slice('onRetryItem={'.length, -1);
    const retryCode = (await transformWithOxc(`const retry = ${retrySource};`, 'retry-before-runner-exit.ts')).code;
    const retry = new Function(...Object.keys(context), `${retryCode};return retry;`)(...Object.values(context));
    const retrying = retry('retry');
    expect(retried).not.toHaveBeenCalled();
    expect(batchCancelledRef.current).toBe(true);
    transferDone(); await run; await retrying;
    expect(context.syncRunnerPendingRef.current).toBe(null);
    expect(commands.filter(command => command === 'provider_upload_file')).toHaveLength(1);
    expect(commands).not.toContain('provider_delete_file');
    expect(context.setRemoteSyncResult.mock.calls[0][0].report.cancelled).toBe(true);
    expect(retried).toHaveBeenCalledOnce();
    expect(batchCancelledRef.current).toBe(false);
});

it.each(['before', 'during'] as const)('preserves local navigation %s the remote completion refresh', async when => {
    const { transformWithOxc } = await import('vite'); const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('          if (!report.cancelled && (report.uploaded > 0');
    const end = app.indexOf('\n        } catch (err)', start);
    const { code } = await transformWithOxc(`async function refreshAfterSync(){${app.slice(start, end)}}`, 'sync-local-navigation.ts');
    let release!: (value: boolean) => void;
    const pending = new Promise<boolean>(resolve => { release = resolve; });
    const context = {
        runCompletionRefresh, syncRefreshAbortRef: {current:null}, batchCancelledRef:{current:false}, report: { cancelled: false, uploaded: 1, downloaded: 0, deleted: 0 }, epoch: 1, checkTarget: vi.fn(),
        loadRemoteFiles: () => pending, loadLocalFiles: vi.fn(), runConfig: { localRoot: '/A' },
        currentLocalPathRef: { current: when === 'before' ? '/B' : '/A' }, localPanelShows,loadLocalPendingRef:{current:null},loadLocalWaitersRef:{current:new Set<() => void>()},isLocalFlattenSearchActive:()=>false,
    };
    const refresh = new Function(...Object.keys(context), `${code};return refreshAfterSync;`)(...Object.values(context));
    const run = refresh(); context.currentLocalPathRef.current = '/B'; release(true); await run;
    expect(context.loadLocalFiles).not.toHaveBeenCalled();
    expect(context.currentLocalPathRef.current).toBe('/B');
});

it.each(['/A/sub', '/A', '/'])('refreshes the affected visible folder %s with no filesystem watcher', async visible => {
    const { transformWithOxc } = await import('vite'); const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('          if (!report.cancelled && (report.uploaded > 0');
    const end = app.indexOf('\n        } catch (err)', start);
    const { code } = await transformWithOxc(`async function refreshAfterSync(){${app.slice(start, end)}}`, 'sync-affected-visible-folder.ts');
    const context = {
        runCompletionRefresh, syncRefreshAbortRef: {current:null}, batchCancelledRef:{current:false}, report: { cancelled: false, uploaded: 0, downloaded: 1, deleted: 1 }, epoch: 1, checkTarget: vi.fn(),
        loadRemoteFiles: async () => true, loadLocalFiles: vi.fn(), runConfig: { localRoot: '/A' },
        currentLocalPathRef: { current: visible }, localPanelShows,loadLocalPendingRef:{current:null},loadLocalWaitersRef:{current:new Set<() => void>()},isLocalFlattenSearchActive:()=>visible === '/',
    };
    const refresh = new Function(...Object.keys(context), `${code};return refreshAfterSync;`)(...Object.values(context));
    await refresh();
    expect(context.loadLocalFiles).toHaveBeenCalledWith(visible, expect.objectContaining({ check: expect.any(Function) }));
    expect(context.currentLocalPathRef.current).toBe(visible);
});

it('keeps an in-flight user navigation when a sync requests a passive local refresh', async () => {
    const { transformWithOxc } = await import('vite'); const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('  const loadLocalFiles ='); const end = app.indexOf('  const loadLocalFiles2 =', start);
    const { code } = await transformWithOxc(app.slice(start, end), 'sync-navigation-priority.ts');
    let release!: (value: unknown[]) => void; const pending = new Promise<unknown[]>(resolve => { release = resolve; });
    const context = {
        useCallback: (fn: unknown) => fn, invoke: vi.fn(() => pending), showHiddenFiles: false,
        loadLocalCallIdRef: { current: 0 }, loadLocalPendingRef: { current: null as number | null }, loadLocalWaitersRef: { current: new Set<() => void>() },
        currentLocalPathRef: { current: '/A/sub' }, setLocalFiles: vi.fn(), setCurrentLocalPath: vi.fn(), setSelectedLocalFiles: vi.fn(),
        notify: { error: vi.fn() }, t: (key: string) => key, activityLog: { log: vi.fn() },
        isGvfsMtpPath: () => false, needsRemovableMediaPlug: () => false,
    };
    const load = new Function(...Object.keys(context), `${code};return loadLocalFiles;`)(...Object.values(context));
    const navigation = load('/B');
    expect(await load('/A/sub', { check: vi.fn() })).toBe(false);
    expect(context.invoke).toHaveBeenCalledOnce();
    release([]); expect(await navigation).toBe(true);
    expect(context.setCurrentLocalPath).toHaveBeenCalledWith('/B');
    expect(context.currentLocalPathRef.current).toBe('/B');
    expect(context.loadLocalPendingRef.current).toBe(null);
    expect(await load('/A/sub', { check: vi.fn() })).toBe(false);
    expect(context.invoke).toHaveBeenCalledOnce();
});

it.each([false, true])('refreshes an ancestor listing only with flattened search=%s', async flattened => {
    const { transformWithOxc } = await import('vite'); const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('          if (!report.cancelled && (report.uploaded > 0');
    const end = app.indexOf('\n        } catch (err)', start);
    const { code } = await transformWithOxc(`async function refreshAfterSync(){${app.slice(start, end)}}`, 'sync-ancestor-selection.ts');
    const context = {
        runCompletionRefresh, syncRefreshAbortRef: {current:null}, batchCancelledRef:{current:false}, report: { cancelled: false, uploaded: 0, downloaded: 1, deleted: 0 }, epoch: 1, checkTarget: vi.fn(),
        loadRemoteFiles: async () => true, loadLocalFiles: vi.fn(), runConfig: { localRoot: '/home/work/sync' },
        currentLocalPathRef: { current: '/home' }, localPanelShows, loadLocalPendingRef:{current:null},loadLocalWaitersRef:{current:new Set<() => void>()},isLocalFlattenSearchActive:()=>flattened,
    };
    const refresh = new Function(...Object.keys(context), `${code};return refreshAfterSync;`)(...Object.values(context));
    await refresh();
    expect(context.loadLocalFiles).toHaveBeenCalledTimes(flattened ? 1 : 0);
});

it.each(['/A/sub', '/B'])('defers sync completion behind a pending listing for %s without watcher events', async destination => {
    const { transformWithOxc } = await import('vite'); const { default: app } = await import('../App.tsx?raw');
    const loadStart = app.indexOf('  const loadLocalFiles ='); const loadEnd = app.indexOf('  const loadLocalFiles2 =', loadStart);
    const refreshStart = app.indexOf('          if (!report.cancelled && (report.uploaded > 0');
    const refreshEnd = app.indexOf('\n        } catch (err)', refreshStart);
    const { code } = await transformWithOxc(`${app.slice(loadStart, loadEnd)};async function refreshAfterSync(){${app.slice(refreshStart, refreshEnd)}}`, 'sync-listing-settlement.ts');
    let release!: (value: unknown[]) => void; const pending = new Promise<unknown[]>(resolve => { release = resolve; });
    const fresh = [{ name: 'downloaded-after-old-snapshot' }];
    const invoke = vi.fn().mockImplementationOnce(() => pending).mockResolvedValue(fresh);
    const context = {
        useCallback: (fn: unknown) => fn, invoke, showHiddenFiles: false,
        loadLocalCallIdRef: { current: 0 }, loadLocalPendingRef: { current: null as number | null },
        loadLocalWaitersRef: { current: new Set<() => void>() }, currentLocalPathRef: { current: '/A/sub' },
        setLocalFiles: vi.fn(), setCurrentLocalPath: vi.fn(), setSelectedLocalFiles: vi.fn(),
        notify: { error: vi.fn() }, t: (key: string) => key, activityLog: { log: vi.fn() },
        isGvfsMtpPath: () => false, needsRemovableMediaPlug: () => false,
        runCompletionRefresh, syncRefreshAbortRef: {current:null}, batchCancelledRef:{current:false}, report: { cancelled: false, uploaded: 0, downloaded: 1, deleted: 0 }, epoch: 1, checkTarget: vi.fn(),
        loadRemoteFiles: async () => true, runConfig: { localRoot: '/A' }, localPanelShows,
        isLocalFlattenSearchActive: () => false,
    };
    const { load, refresh } = new Function(...Object.keys(context), `${code};return {load:loadLocalFiles,refresh:refreshAfterSync};`)(...Object.values(context));
    const navigation = load(destination); const completion = refresh();
    await vi.waitFor(() => expect(context.loadLocalWaitersRef.current.size).toBe(1));
    expect(invoke).toHaveBeenCalledOnce();
    release([]); await navigation; await completion;
    expect(context.currentLocalPathRef.current).toBe(destination);
    expect(context.loadLocalWaitersRef.current.size).toBe(0);
    expect(invoke).toHaveBeenCalledTimes(destination === '/B' ? 1 : 2);
    if (destination !== '/B') expect(context.setLocalFiles).toHaveBeenLastCalledWith(fresh);
});

it.each(['stop-pending', 'stop-retry', 'timeout-pending', 'timeout-read'] as const)('releases sync cleanup for %s even while local IPC remains unresolved', async scenario => {
    vi.useFakeTimers();
    try {
        const { transformWithOxc } = await import('vite'); const { default: app } = await import('../App.tsx?raw');
        const runStart = app.indexOf('  const runConnectedRemoteSync =');
        const start = app.indexOf('      void (async () => {', runStart);
        const end = app.indexOf('      })();', start) + '      })();'.length;
        const workerCode = (await transformWithOxc(app.slice(start, end).replace('void (async', 'return (async'), 'sync-refresh-latch.ts')).code;
        let release!: (files: unknown[]) => void; const stalled = new Promise<unknown[]>(resolve => { release = resolve; });
        let finishCancel!: () => void; const nativeCancellation = new Promise<void>(resolve => { finishCancel = resolve; });
        let queueActive = false;
        const retryExecution = vi.fn(() => { queueActive = true; });
        const context = {
            runFiles: [], runDirs: { local: [], remote: [] }, runConfig: { localRoot: '/A' }, opts: {},
            batchCancelledRef: { current: false }, deltaStats: new Map(), invoke: vi.fn(() => scenario === 'stop-retry' ? nativeCancellation : Promise.resolve()),
            syncRunQueueBridge: () => ({}), transferQueue: {
                getItemNow: () => ({ id: 'retry', status: 'error', restored: false }), items: [{ id: 'retry', status: 'error', restored: false }], retryItem: vi.fn(),
                stopPending: vi.fn(), stopAll: vi.fn(() => { queueActive = false; }), hasActiveTransfersNow: () => queueActive,
            }, epoch: 1, ownerBinding: 'owner',
            retryCallbacksRef: { current: new Map([['retry', retryExecution]]) }, resumeRestoredTransfers: vi.fn(),
            runRemoteSync: vi.fn(async () => ({ cancelled: false, uploaded: 0, downloaded: 1, deleted: 0 })),
            setRemoteSyncResult: vi.fn(), checkTarget: vi.fn(), loadRemoteFiles: vi.fn(async () => true),
            loadLocalFiles: vi.fn(() => stalled), loadLocalPendingRef: { current: scenario === 'timeout-read' ? null : 1 as number | null },
            loadLocalWaitersRef: { current: new Set<() => void>() }, currentLocalPathRef: { current: '/A' },
            syncRefreshAbortRef: { current: null as AbortController | null }, runCompletionRefresh,
            syncCancelPendingRef: { current: null as Promise<void> | null }, syncRunnerPendingRef: { current: null }, runTrackedSync, activeTransferRef: { current: null },
            localPanelShows, isLocalFlattenSearchActive: () => false,
            window: { removeEventListener: vi.fn() }, TRANSFER_EVENT_BRIDGE: 'transfer', onTransferEvent: vi.fn(),
            notify: { error: vi.fn() }, t: (key: string) => key,
            remoteSyncRunningRef: { current: true }, setSyncCancelling: vi.fn(), stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 }, syncPreflightPendingRef: { current: null as symbol | null }, setSyncPreflightPending: vi.fn(), batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null },
            cancelLevelRef: { current: 0 }, batchResumeResolverRef: { current: null }, isBatchPaused: false, setIsBatchPaused: vi.fn(), setBatchPauseReason: vi.fn(), dispatchTransferToast: vi.fn(),
        };
        const run = new Function(...Object.keys(context), workerCode)(...Object.values(context)) as Promise<void>;
        await vi.advanceTimersByTimeAsync(0);
        expect(context.remoteSyncRunningRef.current).toBe(true);
        if (scenario === 'stop-pending' || scenario === 'stop-retry') {
            const cancelStart = app.indexOf('  const cancelTransfer = async');
            const cancelEnd = app.indexOf('  // Circuit breaker: attempt reconnection', cancelStart);
            const cancelCode = (await transformWithOxc(app.slice(cancelStart, cancelEnd), 'stop-sync-refresh.ts')).code;
            const cancel = new Function(...Object.keys(context), `${cancelCode};return cancelTransfer;`)(...Object.values(context));
            if (scenario === 'stop-retry') {
                const retryStart = app.indexOf('onRetryItem={async (id: string) => {');
                const retryEnd = app.indexOf('\n          onStartItem=', retryStart);
                const retrySource = app.slice(retryStart, retryEnd).trim().slice('onRetryItem={'.length, -1);
                const retryCode = (await transformWithOxc(`const retry = ${retrySource};`, 'retry-during-sync-refresh.ts')).code;
                const retry = new Function(...Object.keys(context), `${retryCode};return retry;`)(...Object.values(context));
                await retry('retry');
                expect(queueActive).toBe(true);
                const stopping = cancel();
                await vi.advanceTimersByTimeAsync(0);
                expect(context.transferQueue.stopAll).toHaveBeenCalledOnce();
                expect(context.invoke).toHaveBeenCalledExactlyOnceWith('cancel_transfer');
                expect(context.remoteSyncRunningRef.current).toBe(true);
                const secondStopping = cancel();
                expect(context.invoke).toHaveBeenCalledExactlyOnceWith('cancel_transfer');
                const secondRetry = retry('retry');
                expect(retryExecution).toHaveBeenCalledTimes(1);
                finishCancel();
                await Promise.all([stopping, secondStopping, secondRetry]);
                expect(retryExecution).toHaveBeenCalledTimes(2);
            } else {
                await cancel();
                expect(context.invoke).not.toHaveBeenCalled();
                await run;
                // A subsequent real transfer still uses native cancellation.
                context.remoteSyncRunningRef.current = true;
                await cancel();
                expect(context.invoke).toHaveBeenCalledExactlyOnceWith('cancel_transfer');
                context.remoteSyncRunningRef.current = false;
            }
        } else await vi.advanceTimersByTimeAsync(2001);
        await run;
        expect(context.remoteSyncRunningRef.current).toBe(false);
        expect(context.syncRefreshAbortRef.current).toBe(null);
        expect(context.loadLocalWaitersRef.current.size).toBe(0);
        expect(vi.getTimerCount()).toBe(0);
        expect(context.notify.error).not.toHaveBeenCalled();
        if (scenario !== 'timeout-read') expect(context.loadLocalFiles).not.toHaveBeenCalled();
        release([]);
    } finally { vi.useRealTimers(); }
});

it.each(['provider_gui_target_epoch', 'provider_gui_target_binding'] as const)('keeps Stop All while the App sync launcher waits for %s', async heldCommand => {
    const { transformWithOxc } = await import('vite');
    const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('  const runConnectedRemoteSync =');
    const end = app.indexOf('  // GAP-10: shared local-local sync launcher', start);
    const { code } = await transformWithOxc(app.slice(start, end), 'sync-launch-stop.ts');
    let finish!: (value: string | number) => void;
    const held = new Promise<string | number>(resolve => { finish = resolve; });
    const invoke = vi.fn(async (cmd: string) => cmd === heldCommand ? held : cmd === 'provider_gui_target_epoch' ? 7 : 'owner');
    const runner = vi.fn(async () => ({ cancelled: true }));
    const stopCountRef = { current: 0 };
    const stopAllCountRef = { current: 0 };
    const batchCancelledRef = { current: false };
    const context = {
        useLatestCallback: (fn: unknown) => fn, stopCountRef, stopAllCountRef, batchCancelledRef, syncPreflightPendingRef: { current: null as symbol | null }, setSyncPreflightPending: vi.fn(), batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null },
        snapshotQueuedTarget: () => ({ profileId: 'A', params: { protocol: 'webdav' } }),
        queuedTargetGuard: () => () => {}, QueuedTargetChangedError,
        captureGuiTarget: async () => invoke('provider_gui_target_epoch'),
        currentLocalPath: '/local', currentRemotePath: '/remote', usesProviderApi: () => true,
        localStorage: { getItem: () => null }, isCryptOverlayActive: () => false,
        invoke, remoteSyncRunningRef: { current: false }, cancelLevelRef: { current: 0 },
        runRemoteSync: runner, runTrackedSync, syncRunnerPendingRef: { current: null }, syncCancelPendingRef: { current: null },
        window: { addEventListener: vi.fn(), removeEventListener: vi.fn() }, TRANSFER_EVENT_BRIDGE: 'test',
        syncRunQueueBridge: () => ({}), transferQueue: { hasActiveTransfersNow: () => false }, syncRefreshAbortRef: { current: null }, loadLocalWaitersRef: { current: new Set() }, batchResumeResolverRef: { current: null }, activeTransferRef: { current: null }, setDeltaEligibilityPrompt: vi.fn(), setRemoteSyncResult: vi.fn(), setSyncCancelling: vi.fn(),
        notify: { error: vi.fn(), info: vi.fn() }, t: (key: string) => key, debugMode: false,
    };
    const launch = new Function(...Object.keys(context), `${code};return runConnectedRemoteSync;`)(...Object.values(context));
    launch([file('one')], { remote: [], local: [] }, { direction: 'local_to_remote', deltaSyncEnabled: false, verifyPolicy: 'none', profileId: 'A' });
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith(heldCommand, ...(heldCommand === 'provider_gui_target_binding' ? [expect.anything()] : [])));
    expect(context.syncPreflightPendingRef.current).not.toBeNull();
    const stopStart = app.indexOf('  const cancelTransfer = async');
    const stopEnd = app.indexOf('  // Circuit breaker: attempt reconnection', stopStart);
    const stopCode = (await transformWithOxc(app.slice(stopStart, stopEnd), 'stop-preflight.ts')).code;
    const stop = new Function(...Object.keys(context), `${stopCode};return cancelTransfer;`)(...Object.values(context));
    await stop();
    finish(heldCommand === 'provider_gui_target_epoch' ? 7 : 'owner');
    await new Promise(resolve => setTimeout(resolve, 0));
    if (heldCommand === 'provider_gui_target_epoch') expect(invoke.mock.calls.some(([cmd]) => cmd === 'provider_gui_target_binding')).toBe(false);
    expect(context.syncPreflightPendingRef.current).toBeNull();
    expect(runner).not.toHaveBeenCalled();
    expect(context.remoteSyncRunningRef.current).toBe(false);
    expect(batchCancelledRef.current).toBe(true);
});

it.each(['uploadMultipleFiles', 'downloadMultipleFiles'] as const)('abandons outer %s after Stop All during epoch capture', async name => {
    const { transformWithOxc } = await import('vite');
    const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf(`  const ${name} = withBatchLatch(`);
    const end = app.indexOf(name === 'uploadMultipleFiles' ? '  // === Bulk Operations ===' : '  // Wire cross-panel drag & drop callback', start);
    const { code } = await transformWithOxc(app.slice(start, end), 'outer-transfer-stop.ts');
    let finish!: (epoch: number) => void;
    const epoch = new Promise<number>(resolve => { finish = resolve; });
    const stopCountRef = { current: 0 };
    const invoke = vi.fn(async () => epoch);
    const context = { withBatchLatch: (fn: unknown) => fn, isConnected: true, stopCountRef,
        focusedLocalPanelId: 'local', currentRemotePath: '/remote', currentLocalPath: '/local',
        snapshotQueuedTarget: () => ({}), queuedTargetGuard: () => () => {},
        captureGuiTarget: () => invoke(), notify: { error: vi.fn() }, t: (key: string) => key };
    const run = new Function(...Object.keys(context), `${code};return ${name};`)(...Object.values(context));
    const pending = run(['/one']);
    stopCountRef.current++;
    finish(7);
    await pending;
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(context.notify.error).not.toHaveBeenCalled();
});

it.each(['unrelated-scan', 'stop-all', 'stop-restart', 'parked-stop', 'parked-cancel'] as const)('handles %s while the SFTP eligibility probe is pending', async cancellation => {
    const { transformWithOxc } = await import('vite');
    const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('  const runConnectedRemoteSync =');
    const end = app.indexOf('  // GAP-10: shared local-local sync launcher', start);
    const { code } = await transformWithOxc(app.slice(start, end), 'sync-probe-stop.ts');
    let finish!: (value: unknown) => void;
    const held = new Promise(resolve => { finish = resolve; });
    let prompt: { onContinue: () => void } | null | undefined;
    let finishSecond!: (value: unknown) => void;
    const secondProbe = new Promise(resolve => { finishSecond = resolve; });
    let probes = 0;
    const invoke = vi.fn(async (cmd: string) => cmd === 'sftp_probe_delta_eligibility' ? (++probes === 1 ? held : secondProbe) : 'owner');
    const runner = vi.fn(async () => ({ cancelled: true }));
    const context = { useLatestCallback: (fn: unknown) => fn, stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 }, syncPreflightPendingRef: { current: null as symbol | null }, setSyncPreflightPending: vi.fn(), batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null },
        batchCancelledRef: { current: false }, cancelLevelRef: { current: 0 },
        snapshotQueuedTarget: () => ({ profileId: 'A', params: { protocol: 'sftp' } }),
        queuedTargetGuard: () => () => {}, QueuedTargetChangedError, captureGuiTarget: async () => 7,
        currentLocalPath: '/local', currentRemotePath: '/remote', usesProviderApi: () => true,
        localStorage: { getItem: () => null }, isCryptOverlayActive: () => false,
        invoke, loadSavedServerProfiles: async () => [{ id: 'A' }],
        setDeltaEligibilityPrompt: (value: typeof prompt) => { prompt = value; }, activeUnifiedRemoteProfile: { name: 'A' },
        remoteSyncRunningRef: { current: false }, runRemoteSync: runner, runTrackedSync, syncRunnerPendingRef: { current: null }, syncCancelPendingRef: { current: null },
        window: { addEventListener: vi.fn(), removeEventListener: vi.fn() }, TRANSFER_EVENT_BRIDGE: 'test', syncRunQueueBridge: () => ({}), transferQueue: { hasActiveTransfersNow: () => false }, syncRefreshAbortRef: { current: null }, loadLocalWaitersRef: { current: new Set() }, batchResumeResolverRef: { current: null }, activeTransferRef: { current: null },
        setRemoteSyncResult: vi.fn(), setSyncCancelling: vi.fn(), notify: { error: vi.fn(), info: vi.fn() }, t: (key: string) => key, debugMode: false };
    let finishFolder!: () => void;
    let laterFolderWrites = 0;
    const folderWait = new Promise<void>(resolve => { finishFolder = resolve; });
    const batchStart = app.indexOf('  const withBatchLatch =');
    const batchEnd = app.indexOf('  // Issue #591: every GUI entry point', batchStart);
    const batchCode = (await transformWithOxc(app.slice(batchStart, batchEnd), 'actual-batch-latch.ts')).code;
    const withTransferRun = (await import('./transferRun')).withTransferRun;
    const batchContext = { batchOpInFlightRef: { current: false }, batchGenerationRef: context.batchGenerationRef, batchOperationPendingRef: context.batchOperationPendingRef, withTransferRun };
    const wrapBatch = new Function(...Object.keys(batchContext), `${batchCode};return withBatchLatch;`)(...Object.values(batchContext));
    const folder = cancellation === 'unrelated-scan' ? wrapBatch(async () => {
        await folderWait;
        if (!context.batchCancelledRef.current) laterFolderWrites++;
    })() : Promise.resolve();
    const launch = new Function(...Object.keys(context), `${code};return runConnectedRemoteSync;`)(...Object.values(context));
    launch([file('one')], { remote: [], local: [] }, { direction: 'local_to_remote', deltaSyncEnabled: true, verifyPolicy: 'none', profileId: 'A' });
    await vi.waitFor(() => expect(invoke.mock.calls.some(([cmd]) => cmd === 'sftp_probe_delta_eligibility')).toBe(true));
    const stopStart = app.indexOf('  const cancelTransfer = async');
    const stopEnd = app.indexOf('  // Circuit breaker: attempt reconnection', stopStart);
    const stopCode = (await transformWithOxc(app.slice(stopStart, stopEnd), 'stop-owned-preflight.ts')).code;
    const stop = new Function(...Object.keys(context), `${stopCode};return cancelTransfer;`)(...Object.values(context));
    if (cancellation === 'parked-stop' || cancellation === 'parked-cancel') {
        finish({ eligible: false, reason: 'unavailable' });
        await vi.waitFor(() => expect(prompt).toBeTruthy());
        const staleContinue = prompt!.onContinue;
        expect(context.syncPreflightPendingRef.current).not.toBeNull();
        const line = app.split('\n').find(line => line.includes('stop: async () => { await cancelActiveConnect();'))!;
        const controllerContext = { ...context, cancelTransfer: stop, cancelActiveConnect: async () => {}, hasActiveTransfer: false, hasQueueActivity: false };
        const controllerStop = new Function(...Object.keys(controllerContext), `return ({${line.trim()}}).stop;`)(...Object.values(controllerContext));
        if (cancellation === 'parked-cancel') {
            const dialogStart = app.indexOf('<DeltaEligibilityDialog');
            const cancelStart = app.indexOf('onCancel={', dialogStart) + 'onCancel={'.length;
            const cancelEnd = app.indexOf('          onContinue=', cancelStart);
            const cancelSource = app.slice(cancelStart, cancelEnd).trim().slice(0, -1);
            const cancelContext = { ...context, deltaEligibilityPrompt: prompt };
            const cancelDialog = new Function(...Object.keys(cancelContext), `return (${cancelSource});`)(...Object.values(cancelContext));
            cancelDialog();
        } else await controllerStop();
        expect(context.syncPreflightPendingRef.current).toBeNull();
        expect(prompt).toBeFalsy();
        staleContinue();
        await new Promise(resolve => setTimeout(resolve, 0));
        expect(runner).not.toHaveBeenCalled();
        return;
    }
    if (cancellation === 'unrelated-scan') {
        context.stopCountRef.current++;
        context.batchCancelledRef.current = true;
    } else {
        await stop();
        expect(context.syncPreflightPendingRef.current).toBeNull();
    }
    if (cancellation === 'stop-restart') {
        launch([file('two')], { remote: [], local: [] }, { direction: 'local_to_remote', deltaSyncEnabled: true, verifyPolicy: 'none', profileId: 'A' });
        await vi.waitFor(() => expect(probes).toBe(2));
        const newerOwner = context.syncPreflightPendingRef.current;
        expect(newerOwner).not.toBeNull();
        const stateUpdates = context.setSyncPreflightPending.mock.calls.length;
        finish({ eligible: false, reason: 'obsolete' });
        await new Promise(resolve => setTimeout(resolve, 0));
        expect(context.syncPreflightPendingRef.current).toBe(newerOwner);
        expect(context.setSyncPreflightPending.mock.calls).toHaveLength(stateUpdates);
        expect(prompt).toBeFalsy();
        finishSecond({ eligible: true });
        await vi.waitFor(() => expect(runner).toHaveBeenCalledTimes(1));
        return;
    }
    finish({ eligible: false, reason: 'unavailable' });
    if (cancellation === 'unrelated-scan') {
        await vi.waitFor(() => expect(prompt).toBeDefined());
        prompt!.onContinue();
        await new Promise(resolve => setTimeout(resolve, 0));
        expect(runner).not.toHaveBeenCalled();
        expect(context.batchCancelledRef.current).toBe(true);
        finishFolder();
        await folder;
        expect(laterFolderWrites).toBe(0);
        // The plan predates the folder operation it waited for: it is refused.
        await vi.waitFor(() => expect(context.notify.error).toHaveBeenCalledWith('aerosync.title', 'syncPanel.planStaleAfterBatch'));
        await new Promise(resolve => setTimeout(resolve, 0));
        expect(runner).not.toHaveBeenCalled();
        expect(context.batchCancelledRef.current).toBe(true);
        expect(context.syncPreflightPendingRef.current).toBeNull();
    } else {
        await new Promise(resolve => setTimeout(resolve, 0));
        expect(prompt).toBeFalsy();
        expect(runner).not.toHaveBeenCalled();
    }
});

it('enables toolbar Stop and GUI-controller Stop during sync preflight with an empty queue', async () => {
    const { default: app } = await import('../App.tsx?raw');
    const buttonStart = app.indexOf('onClick={cancelTransfer}');
    const expression = app.slice(buttonStart).match(/disabled=\{([^}]+)\}/)![1];
    const disabled = new Function('syncPreflightPending', 'syncCancelling', 'isForceStopMode', 'hasActiveTransfer', 'hasQueueActivity', `const remoteSyncRunningRef = { current: false };return ${expression};`);
    expect(disabled(true, false, false, false, false)).toBe(false);
    expect(disabled(false, false, false, false, false)).toBe(true);
    expect(app).toContain('if (syncPreflightPendingRef.current || remoteSyncRunningRef.current || batchOperationPendingRef.current || hasActiveTransfer || hasQueueActivity) await cancelTransfer();');
});

async function appSlice(start: string, end: string, name: string) {
    const { transformWithOxc } = await import('vite');
    const { default: app } = await import('../App.tsx?raw');
    const from = app.indexOf(start);
    const to = app.indexOf(end, from);
    if (from < 0 || to < 0) throw new Error(`Missing App closure: ${start}`);
    return (await transformWithOxc(app.slice(from, to), name)).code;
}
const tick = () => new Promise(resolve => setTimeout(resolve, 0));

it.each([true, false])('does not run a saved Mirror delete after waiting for a batch that replaced the file (batch: %s)', async withBatch => {
    const launchCode = await appSlice('  const runConnectedRemoteSync =', '  // GAP-10: shared local-local sync launcher', 'stale-mirror-plan.ts');
    const batchCode = await appSlice('  const withBatchLatch =', '  // Issue #591: every GUI entry point', 'stale-mirror-batch.ts');
    const { withTransferRun } = await import('./transferRun');
    // The remote folder: the compared plan saw `old` and marked it an orphan.
    const remoteFiles = new Map([['/remote/orphan.txt', 'old']]);
    const backend = wrapIntentInvoke(async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
        if (cmd === 'provider_gui_target_binding') return 'owner' as T;
        if (cmd === 'provider_bulk_support') return { upload: false, download: false } as T;
        if (cmd === 'provider_delete_file') remoteFiles.delete(String(args?.path));
        return undefined as T;
    });
    const invoke = vi.fn(<T,>(cmd: string, args?: Record<string, unknown>) => backend<T>(cmd, args));
    const context = {
        useLatestCallback: (fn: unknown) => fn, stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 },
        syncPreflightPendingRef: { current: null as symbol | null }, setSyncPreflightPending: vi.fn(),
        batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null }, batchCancelledRef: { current: false }, cancelLevelRef: { current: 0 },
        snapshotQueuedTarget: () => ({ profileId: 'A', params: { protocol: 'webdav' } }),
        queuedTargetGuard: () => () => {}, QueuedTargetChangedError, captureGuiTarget: async () => 7,
        currentLocalPath: '/local', currentRemotePath: '/remote', usesProviderApi: () => true,
        localStorage: { getItem: () => null }, isCryptOverlayActive: () => false, invoke,
        remoteSyncRunningRef: { current: false }, runRemoteSync, runTrackedSync, syncRunnerPendingRef: { current: null }, syncCancelPendingRef: { current: null },
        window: { addEventListener: vi.fn(), removeEventListener: vi.fn() }, TRANSFER_EVENT_BRIDGE: 'test',
        syncRunQueueBridge: () => () => undefined, transferQueue: {}, syncRefreshAbortRef: { current: null },
        runCompletionRefresh, loadRemoteFiles: vi.fn(async () => true), loadLocalPendingRef: { current: null }, loadLocalWaitersRef: { current: new Set() },
        currentLocalPathRef: { current: '' }, localPanelShows, isLocalFlattenSearchActive: () => false, loadLocalFiles: vi.fn(),
        setDeltaEligibilityPrompt: vi.fn(), setRemoteSyncResult: vi.fn(), setSyncCancelling: vi.fn(),
        notify: { error: vi.fn(), info: vi.fn() }, t: (key: string) => key, debugMode: false,
    };
    const batchContext = { batchOpInFlightRef: { current: false }, batchGenerationRef: context.batchGenerationRef, batchOperationPendingRef: context.batchOperationPendingRef, withTransferRun };
    const wrapBatch = new Function(...Object.keys(batchContext), `${batchCode};return withBatchLatch;`)(...Object.values(batchContext));
    let finishUpload!: () => void;
    const uploading = new Promise<void>(resolve => { finishUpload = resolve; });
    // An upload batch that replaces the orphan while the sync waits for it.
    const batch = withBatch ? wrapBatch(async () => { await uploading; remoteFiles.set('/remote/orphan.txt', 'new'); })() : Promise.resolve();
    const launch = new Function(...Object.keys(context), `${launchCode};return runConnectedRemoteSync;`)(...Object.values(context));
    launch([file('orphan.txt', 'delete-remote')], { remote: [], local: [] }, { direction: 'local_to_remote', deltaSyncEnabled: false, verifyPolicy: 'none', profileId: 'A' });
    const commands = () => invoke.mock.calls.map(([cmd]) => cmd);
    if (!withBatch) {
        // Control: with no batch in the way, this very plan does delete the file.
        await vi.waitFor(() => expect(context.setRemoteSyncResult).toHaveBeenCalled());
        expect(remoteFiles.has('/remote/orphan.txt')).toBe(false);
        expect(context.notify.error).not.toHaveBeenCalled();
        return;
    }
    await vi.waitFor(() => expect(commands()).toContain('provider_gui_target_binding'));
    await tick();
    expect(commands()).not.toContain('sync_run_lease_begin_cmd');
    finishUpload();
    await batch;
    await vi.waitFor(() => expect(context.notify.error).toHaveBeenCalledWith('aerosync.title', 'syncPanel.planStaleAfterBatch'));
    await tick();
    expect(remoteFiles.get('/remote/orphan.txt')).toBe('new');
    expect(commands()).not.toContain('provider_delete_file');
    expect(commands()).not.toContain('sync_run_lease_begin_cmd');
    expect(context.setRemoteSyncResult).not.toHaveBeenCalled();
    expect(context.remoteSyncRunningRef.current).toBe(false);
    expect(context.syncPreflightPendingRef.current).toBeNull();
});

// A batch that is over by the time the plan is launched leaves no pending
// latch to wait for. The plan keeps the batch counter its comparison began
// with, so the batch is seen either way: one that started and ended while the
// launch waited for the target binding, or one that ran between the
// comparison and the click on Sync. Control: no batch, and the plan runs.
it.each(['during-preflight', 'before-launch', 'none'] as const)('refuses a saved Mirror delete after a batch that is already over (%s)', async when => {
    const launchCode = await appSlice('  const runConnectedRemoteSync =', '  // GAP-10: shared local-local sync launcher', 'stale-mirror-over.ts');
    const batchCode = await appSlice('  const withBatchLatch =', '  // Issue #591: every GUI entry point', 'stale-mirror-over-batch.ts');
    const { withTransferRun } = await import('./transferRun');
    const remoteFiles = new Map([['/remote/orphan.txt', 'old']]);
    let releaseBinding!: () => void;
    const binding = new Promise<void>(resolve => { releaseBinding = resolve; });
    const backend = wrapIntentInvoke(async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
        if (cmd === 'provider_gui_target_binding') { await binding; return 'owner' as T; }
        if (cmd === 'provider_bulk_support') return { upload: false, download: false } as T;
        if (cmd === 'provider_delete_file') remoteFiles.delete(String(args?.path));
        return undefined as T;
    });
    const invoke = vi.fn(<T,>(cmd: string, args?: Record<string, unknown>) => backend<T>(cmd, args));
    const context = {
        useLatestCallback: (fn: unknown) => fn, stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 },
        syncPreflightPendingRef: { current: null as symbol | null }, setSyncPreflightPending: vi.fn(),
        batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null }, batchCancelledRef: { current: false }, cancelLevelRef: { current: 0 },
        snapshotQueuedTarget: () => ({ profileId: 'A', params: { protocol: 'webdav' } }),
        queuedTargetGuard: () => () => {}, QueuedTargetChangedError, captureGuiTarget: async () => 7,
        currentLocalPath: '/local', currentRemotePath: '/remote', usesProviderApi: () => true,
        localStorage: { getItem: () => null }, isCryptOverlayActive: () => false, invoke,
        remoteSyncRunningRef: { current: false }, runRemoteSync, runTrackedSync, syncRunnerPendingRef: { current: null }, syncCancelPendingRef: { current: null },
        window: { addEventListener: vi.fn(), removeEventListener: vi.fn() }, TRANSFER_EVENT_BRIDGE: 'test',
        syncRunQueueBridge: () => () => undefined, transferQueue: {}, syncRefreshAbortRef: { current: null },
        runCompletionRefresh, loadRemoteFiles: vi.fn(async () => true), loadLocalPendingRef: { current: null }, loadLocalWaitersRef: { current: new Set() },
        currentLocalPathRef: { current: '' }, localPanelShows, isLocalFlattenSearchActive: () => false, loadLocalFiles: vi.fn(),
        setDeltaEligibilityPrompt: vi.fn(), setRemoteSyncResult: vi.fn(), setSyncCancelling: vi.fn(),
        notify: { error: vi.fn(), info: vi.fn() }, t: (key: string) => key, debugMode: false,
    };
    const batchContext = { batchOpInFlightRef: { current: false }, batchGenerationRef: context.batchGenerationRef, batchOperationPendingRef: context.batchOperationPendingRef, withTransferRun };
    const wrapBatch = new Function(...Object.keys(batchContext), `${batchCode};return withBatchLatch;`)(...Object.values(batchContext));
    // An upload batch that replaces the orphan and is over before the launch decides.
    const replace = () => wrapBatch(async () => { remoteFiles.set('/remote/orphan.txt', 'new'); })();
    // The value the comparison began with.
    const planBatchGeneration = context.batchGenerationRef.current;
    if (when === 'before-launch') await replace();
    const launch = new Function(...Object.keys(context), `${launchCode};return runConnectedRemoteSync;`)(...Object.values(context));
    launch([file('orphan.txt', 'delete-remote')], { remote: [], local: [] }, { direction: 'local_to_remote', deltaSyncEnabled: false, verifyPolicy: 'none', profileId: 'A', planBatchGeneration });
    const commands = () => invoke.mock.calls.map(([cmd]) => cmd);
    await vi.waitFor(() => expect(commands()).toContain('provider_gui_target_binding'));
    if (when === 'during-preflight') {
        await replace();
        expect(context.batchOperationPendingRef.current).toBeNull();
    }
    releaseBinding();
    if (when === 'none') {
        await vi.waitFor(() => expect(context.setRemoteSyncResult).toHaveBeenCalled());
        expect(remoteFiles.has('/remote/orphan.txt')).toBe(false);
        expect(context.notify.error).not.toHaveBeenCalled();
        return;
    }
    await vi.waitFor(() => expect(context.notify.error).toHaveBeenCalledWith('aerosync.title', 'syncPanel.planStaleAfterBatch'));
    await tick();
    expect(remoteFiles.get('/remote/orphan.txt')).toBe('new');
    expect(commands()).not.toContain('provider_delete_file');
    expect(commands()).not.toContain('sync_run_lease_begin_cmd');
    expect(context.setRemoteSyncResult).not.toHaveBeenCalled();
    expect(context.remoteSyncRunningRef.current).toBe(false);
});

// A staged upload queued before Compare, started after it, and a failed row
// retried while the launch waits: neither enters `withBatchLatch`. Both run
// the callback stored for the row (the staged effect and the Retry handler
// read the same map), so the stored callback must move the number.
it.each(['staged-start', 'retry'] as const)('refuses a saved Mirror delete after a %s that ran outside the batch latch', async path => {
    const launchCode = await appSlice('  const runConnectedRemoteSync =', '  // GAP-10: shared local-local sync launcher', 'stale-mirror-staged.ts');
    const generationCode = await appSlice('  const withTransferGeneration =', '  useEffect(() => {\n    batchOpInFlightRef', 'stale-mirror-generation.ts');
    const remoteFiles = new Map([['/remote/orphan.txt', 'old']]);
    let releaseBinding!: () => void;
    const binding = new Promise<void>(resolve => { releaseBinding = resolve; });
    const backend = wrapIntentInvoke(async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
        if (cmd === 'provider_gui_target_binding') { await binding; return 'owner' as T; }
        if (cmd === 'provider_bulk_support') return { upload: false, download: false } as T;
        if (cmd === 'provider_delete_file') remoteFiles.delete(String(args?.path));
        return undefined as T;
    });
    const invoke = vi.fn(<T,>(cmd: string, args?: Record<string, unknown>) => backend<T>(cmd, args));
    const context = {
        useLatestCallback: (fn: unknown) => fn, stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 },
        syncPreflightPendingRef: { current: null as symbol | null }, setSyncPreflightPending: vi.fn(),
        batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null }, batchCancelledRef: { current: false }, cancelLevelRef: { current: 0 },
        snapshotQueuedTarget: () => ({ profileId: 'A', params: { protocol: 'webdav' } }),
        queuedTargetGuard: () => () => {}, QueuedTargetChangedError, captureGuiTarget: async () => 7,
        currentLocalPath: '/local', currentRemotePath: '/remote', usesProviderApi: () => true,
        localStorage: { getItem: () => null }, isCryptOverlayActive: () => false, invoke,
        remoteSyncRunningRef: { current: false }, runRemoteSync, runTrackedSync, syncRunnerPendingRef: { current: null }, syncCancelPendingRef: { current: null },
        window: { addEventListener: vi.fn(), removeEventListener: vi.fn() }, TRANSFER_EVENT_BRIDGE: 'test',
        syncRunQueueBridge: () => () => undefined, transferQueue: {}, syncRefreshAbortRef: { current: null },
        runCompletionRefresh, loadRemoteFiles: vi.fn(async () => true), loadLocalPendingRef: { current: null }, loadLocalWaitersRef: { current: new Set() },
        currentLocalPathRef: { current: '' }, localPanelShows, isLocalFlattenSearchActive: () => false, loadLocalFiles: vi.fn(),
        setDeltaEligibilityPrompt: vi.fn(), setRemoteSyncResult: vi.fn(), setSyncCancelling: vi.fn(),
        notify: { error: vi.fn(), info: vi.fn() }, t: (key: string) => key, debugMode: false,
    };
    const retryCallbacksRef = { current: new Map<string, () => unknown>() };
    const generationContext = { batchGenerationRef: context.batchGenerationRef, transfersInFlightRef: context.transfersInFlightRef, retryCallbacksRef };
    const registerTransferCallback = new Function(...Object.keys(generationContext), `${generationCode};return registerTransferCallback;`)(...Object.values(generationContext));
    // The row is queued (staged, or failed once) before the comparison.
    registerTransferCallback('row-1', async () => { remoteFiles.set('/remote/orphan.txt', 'new'); });
    // What the staged effect and the Retry handler do with the row.
    const runRow = () => retryCallbacksRef.current.get('row-1')!();
    const planBatchGeneration = context.batchGenerationRef.current;
    if (path === 'staged-start') await runRow();
    const launch = new Function(...Object.keys(context), `${launchCode};return runConnectedRemoteSync;`)(...Object.values(context));
    launch([file('orphan.txt', 'delete-remote')], { remote: [], local: [] }, { direction: 'local_to_remote', deltaSyncEnabled: false, verifyPolicy: 'none', profileId: 'A', planBatchGeneration });
    const commands = () => invoke.mock.calls.map(([cmd]) => cmd);
    await vi.waitFor(() => expect(commands()).toContain('provider_gui_target_binding'));
    if (path === 'retry') await runRow();
    expect(context.batchOperationPendingRef.current).toBeNull();
    releaseBinding();
    await vi.waitFor(() => expect(context.notify.error).toHaveBeenCalledWith('aerosync.title', 'syncPanel.planStaleAfterBatch'));
    await tick();
    expect(remoteFiles.get('/remote/orphan.txt')).toBe('new');
    expect(commands()).not.toContain('provider_delete_file');
    expect(commands()).not.toContain('sync_run_lease_begin_cmd');
});

/** A saved Mirror plan that deletes `/remote/orphan.txt`, launched through the
 *  real `runConnectedRemoteSync` while its target binding is held, and the
 *  real generation wrappers over the same refs. */
async function mirrorLaunchHarness() {
    const launchCode = await appSlice('  const runConnectedRemoteSync =', '  // GAP-10: shared local-local sync launcher', 'mirror-live-launch.ts');
    const generationCode = await appSlice('  const withTransferGeneration =', '  useEffect(() => {\n    batchOpInFlightRef', 'mirror-live-generation.ts');
    const remoteFiles = new Map([['/remote/orphan.txt', 'old']]);
    let releaseBinding!: () => void;
    const binding = new Promise<void>(resolve => { releaseBinding = resolve; });
    const backend = wrapIntentInvoke(async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
        if (cmd === 'provider_gui_target_binding') { await binding; return 'owner' as T; }
        if (cmd === 'provider_bulk_support') return { upload: false, download: false } as T;
        if (cmd === 'provider_delete_file') remoteFiles.delete(String(args?.path));
        return undefined as T;
    });
    const invoke = vi.fn(<T,>(cmd: string, args?: Record<string, unknown>) => backend<T>(cmd, args));
    const context = {
        useLatestCallback: (fn: unknown) => fn, stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 },
        syncPreflightPendingRef: { current: null as symbol | null }, setSyncPreflightPending: vi.fn(),
        batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null }, batchCancelledRef: { current: false }, cancelLevelRef: { current: 0 },
        snapshotQueuedTarget: () => ({ profileId: 'A', params: { protocol: 'webdav' } }),
        queuedTargetGuard: () => () => {}, QueuedTargetChangedError, captureGuiTarget: async () => 7,
        currentLocalPath: '/local', currentRemotePath: '/remote', usesProviderApi: () => true,
        localStorage: { getItem: () => null }, isCryptOverlayActive: () => false, invoke,
        remoteSyncRunningRef: { current: false }, runRemoteSync, runTrackedSync, syncRunnerPendingRef: { current: null }, syncCancelPendingRef: { current: null },
        window: { addEventListener: vi.fn(), removeEventListener: vi.fn() }, TRANSFER_EVENT_BRIDGE: 'test',
        syncRunQueueBridge: () => () => undefined, transferQueue: {}, syncRefreshAbortRef: { current: null },
        runCompletionRefresh, loadRemoteFiles: vi.fn(async () => true), loadLocalPendingRef: { current: null }, loadLocalWaitersRef: { current: new Set() },
        currentLocalPathRef: { current: '' }, localPanelShows, isLocalFlattenSearchActive: () => false, loadLocalFiles: vi.fn(),
        setDeltaEligibilityPrompt: vi.fn(), setRemoteSyncResult: vi.fn(), setSyncCancelling: vi.fn(),
        notify: { error: vi.fn(), info: vi.fn() }, t: (key: string) => key, debugMode: false,
    };
    const retryCallbacksRef = { current: new Map<string, () => unknown>() };
    const generationContext = { batchGenerationRef: context.batchGenerationRef, transfersInFlightRef: context.transfersInFlightRef, retryCallbacksRef };
    const { withTransferGeneration, registerTransferCallback } = new Function(...Object.keys(generationContext),
        `${generationCode};return { withTransferGeneration, registerTransferCallback };`)(...Object.values(generationContext));
    const launchPlan = (planBatchGeneration: number) => {
        const launch = new Function(...Object.keys(context), `${launchCode};return runConnectedRemoteSync;`)(...Object.values(context));
        launch([file('orphan.txt', 'delete-remote')], { remote: [], local: [] }, { direction: 'local_to_remote', deltaSyncEnabled: false, verifyPolicy: 'none', profileId: 'A', planBatchGeneration });
    };
    const commands = () => invoke.mock.calls.map(([cmd]) => cmd);
    const expectRefused = async () => {
        await vi.waitFor(() => expect(context.notify.error).toHaveBeenCalledWith('aerosync.title', 'syncPanel.planStaleAfterBatch'));
        await tick();
        expect(commands()).not.toContain('provider_delete_file');
        expect(commands()).not.toContain('sync_run_lease_begin_cmd');
        expect(remoteFiles.get('/remote/orphan.txt')).toBe('new');
    };
    return { context, remoteFiles, releaseBinding, commands, launchPlan, expectRefused, withTransferGeneration, registerTransferCallback, retryCallbacksRef };
}

// Greptile 4558715864: a staged Start runs the callback App stores, which the
// real dispatcher answers at once while its batch keeps going. The work the
// dispatcher runs is what moves the number and counts as in flight, so the
// plan is refused whether the upload is still running at the launch or ended
// just before it.
it.each(['still-uploading', 'ended-before-launch'] as const)('refuses a saved Mirror delete while a staged upload started by the real dispatcher is %s', async when => {
    const h = await mirrorLaunchHarness();
    let finishUpload!: () => void;
    const uploading = new Promise<void>(resolve => { finishUpload = resolve; });
    // As App: `const launchBatch = withTransferGeneration(async (...) => ...)`.
    const launchBatch = h.withTransferGeneration(async (_entries: unknown[], ids: string[]) => {
        await uploading;
        h.remoteFiles.set('/remote/orphan.txt', 'new');
        return new Set(ids);
    });
    const dispatcher = createFileBatchDispatcher({ ids: ['row-1'], entries: ['orphan.txt'], run: launchBatch, statusOf: () => 'pending', firstStartRunsAll: true });
    h.registerTransferCallback('row-1', rearmedOnUserAction({ batchCancelled: { current: false }, cancelLevel: { current: 0 } }, dispatcher.callbackFor('row-1')));
    // Start: the stored callback returns while the batch is still sending...
    await h.retryCallbacksRef.current.get('row-1')!();
    expect(h.context.transfersInFlightRef.current).toBe(1);
    // ...and the comparison runs after it returned: the number it reads
    // already counts the start.
    const planBatchGeneration = h.context.batchGenerationRef.current;
    if (when === 'ended-before-launch') {
        finishUpload();
        await vi.waitFor(() => expect(h.context.transfersInFlightRef.current).toBe(0));
    }
    h.launchPlan(planBatchGeneration);
    await vi.waitFor(() => expect(h.commands()).toContain('provider_gui_target_binding'));
    h.releaseBinding();
    if (when === 'still-uploading') {
        await vi.waitFor(() => expect(h.context.notify.error).toHaveBeenCalled());
        finishUpload();
        await vi.waitFor(() => expect(h.context.transfersInFlightRef.current).toBe(0));
    }
    await h.expectRefused();
});

// CodeRabbit conversation item 1: a transfer that began before Compare moved
// the number before the plan read it, and is still writing when Sync is
// pressed. The count of transfers in flight refuses the plan. Control: one
// that ended before Compare leaves the plan to run.
it.each(['active-at-launch', 'ended-before-compare'] as const)('a transfer begun before Compare and %s', async when => {
    const h = await mirrorLaunchHarness();
    let finishUpload!: () => void;
    const uploading = new Promise<void>(resolve => { finishUpload = resolve; });
    const upload = h.withTransferGeneration(async () => { await uploading; h.remoteFiles.set('/remote/orphan.txt', 'new'); });
    const running = upload();
    if (when === 'ended-before-compare') {
        finishUpload();
        await running;
    }
    // The comparison begins now and reads the number.
    const planBatchGeneration = h.context.batchGenerationRef.current;
    h.launchPlan(planBatchGeneration);
    await vi.waitFor(() => expect(h.commands()).toContain('provider_gui_target_binding'));
    h.releaseBinding();
    if (when === 'ended-before-compare') {
        await vi.waitFor(() => expect(h.context.setRemoteSyncResult).toHaveBeenCalled());
        expect(h.remoteFiles.has('/remote/orphan.txt')).toBe(false);
        expect(h.context.notify.error).not.toHaveBeenCalled();
        return;
    }
    await vi.waitFor(() => expect(h.context.notify.error).toHaveBeenCalled());
    finishUpload();
    await running;
    await h.expectRefused();
});

it('routes every stored transfer callback and every single transfer through the generation counter', async () => {
    const { default: app } = await import('../App.tsx?raw');
    // The one write to the map is inside registerTransferCallback.
    expect(app.match(/retryCallbacksRef\.current\.set\(/g)).toHaveLength(1);
    expect(app).toContain('retryCallbacksRef.current.set(id, withTransferGeneration(callback));');
    expect(app).toContain('const downloadFile = useLatestCallback(withTransferGeneration(async (');
    expect(app).toContain('const uploadFile = useLatestCallback(withTransferGeneration(async (');
    // The batch dispatcher's real work: its stored callbacks return earlier.
    expect(app).toContain('const launchBatch = withTransferGeneration(async (');
    expect(app).toContain('const runSingles = withTransferGeneration(async (');
});

it('keeps the local-local latch until a delayed cancel_transfer acknowledgement settles', async () => {
    const launchCode = await appSlice('  const runLocalLocalSync = useLatestCallback((', '  const handleCompareMirrorLeftToRight =', 'local-local-latch.ts');
    const stopCode = await appSlice('  const cancelTransfer = async', '  // Circuit breaker: attempt reconnection', 'local-local-stop.ts');
    let finishCopy!: () => void;
    const copying = new Promise<void>(resolve => { finishCopy = resolve; });
    let acknowledgeCancel!: () => void;
    const cancelling = new Promise<void>(resolve => { acknowledgeCancel = resolve; });
    let copies = 0;
    const backend = wrapIntentInvoke(async <T,>(cmd: string): Promise<T> => {
        if (cmd === 'copy_local_file' && ++copies === 1) await copying;
        if (cmd === 'cancel_transfer') await cancelling;
        return undefined as T;
    });
    const invoke = vi.fn(<T,>(cmd: string, args?: Record<string, unknown>) => backend<T>(cmd, args));
    const resets = () => invoke.mock.calls.filter(([cmd]) => cmd === 'reset_cancel_flag').length;
    const context = {
        useLatestCallback: (fn: unknown) => fn, currentLocalPath: '/left', currentLocalPath2: '/right',
        notify: { error: vi.fn(), info: vi.fn(), warning: vi.fn() }, t: (key: string) => key,
        remoteSyncRunningRef: { current: false }, batchCancelledRef: { current: false }, cancelLevelRef: { current: 0 },
        runRemoteSync, runTrackedSync, syncRunnerPendingRef: { current: null as Promise<void> | null }, syncCancelPendingRef: { current: null as Promise<void> | null },
        syncRunQueueBridge: () => () => undefined, transferQueue: { hasActiveTransfersNow: () => false, stopAll: vi.fn() }, invoke,
        setRemoteSyncResult: vi.fn(), loadLocalFiles: vi.fn(), loadLocalFiles2: vi.fn(), setSyncCancelling: vi.fn(),
        stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 }, syncPreflightPendingRef: { current: null }, setSyncPreflightPending: vi.fn(),
        setDeltaEligibilityPrompt: vi.fn(), syncRefreshAbortRef: { current: null }, loadLocalWaitersRef: { current: new Set() },
        batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null }, activeTransferRef: { current: null }, batchResumeResolverRef: { current: null },
        dispatchTransferToast: vi.fn(), setIsBatchPaused: vi.fn(), setBatchPauseReason: vi.fn(),
    };
    const launch = new Function(...Object.keys(context), `${launchCode};return runLocalLocalSync;`)(...Object.values(context));
    const stop = new Function(...Object.keys(context), `${stopCode};return cancelTransfer;`)(...Object.values(context));
    const options = { direction: 'local_to_remote', verifyPolicy: 'none' };
    launch([file('one')], { remote: [], local: [] }, options);
    await vi.waitFor(() => expect(copies).toBe(1));
    expect(resets()).toBe(1);
    const stopped = stop();
    await vi.waitFor(() => expect(invoke.mock.calls.some(([cmd]) => cmd === 'cancel_transfer')).toBe(true));
    // The JS runner exits while the native cancel is still unacknowledged.
    finishCopy();
    await vi.waitFor(() => expect(context.setRemoteSyncResult).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(context.syncRunnerPendingRef.current).toBeNull());
    await tick();
    expect(context.syncCancelPendingRef.current).not.toBeNull();
    launch([file('two')], { remote: [], local: [] }, options);
    await tick();
    expect(resets()).toBe(1);
    expect(copies).toBe(1);
    expect(context.notify.info).toHaveBeenCalledWith('aerosync.title', 'syncPanel.syncing');
    acknowledgeCancel();
    await stopped;
    await vi.waitFor(() => expect(context.remoteSyncRunningRef.current).toBe(false));
    // Once the stale cancel has settled, a run starts and re-arms the flag.
    launch([file('two')], { remote: [], local: [] }, options);
    await vi.waitFor(() => expect(context.setRemoteSyncResult).toHaveBeenCalledTimes(2));
    expect(resets()).toBe(2);
    expect(copies).toBe(2);
});

it('stops a multi-file remote delete between entries from the GUI controller', async () => {
    const { default: app } = await import('../App.tsx?raw');
    const batchCode = await appSlice('  const withBatchLatch =', '  // Issue #591: every GUI entry point', 'controller-stop-batch.ts');
    const deleteCode = await appSlice('  const deleteMultipleRemoteFiles = withBatchLatch(', '  // Moving local items to the trash', 'controller-stop-delete.ts');
    const stopCode = await appSlice('  const cancelTransfer = async', '  // Circuit breaker: attempt reconnection', 'controller-stop.ts');
    const { withTransferRun } = await import('./transferRun');
    let finishFirst!: () => void;
    const firstDelete = new Promise<void>(resolve => { finishFirst = resolve; });
    const deleted: string[] = [];
    const invoke = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
        if (cmd !== 'provider_delete_file') return undefined;
        deleted.push(String(args?.path));
        if (deleted.length === 1) await firstDelete;
        return undefined;
    });
    const remoteFiles = ['a', 'b', 'c'].map(name => ({ name, path: `/${name}`, is_dir: false }));
    const context = {
        batchOpInFlightRef: { current: false }, batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null }, withTransferRun,
        selectedRemoteFiles: new Set<string>(), remoteActionGuard: () => () => undefined,
        prepareGuiTarget: () => Promise.resolve({ epoch: 7 }), resolveGuiTarget: async (prepared: Promise<{ epoch: number }>) => (await prepared).epoch,
        notify: { error: vi.fn(), success: vi.fn() }, t: (key: string) => key,
        liveConnectionParams: { protocol: 'webdav' }, usesProviderApi: () => true, aeroVaultOverlaySession: null, currentRemotePath: '/',
        remoteFiles, humanLog: { logStart: () => 'log', updateEntry: vi.fn() }, batchCancelledRef: { current: false }, deleteGesturesRef: { current: [] }, cancelLevelRef: { current: 0 },
        invoke, setScanningState: vi.fn(), INITIAL_SCANNING_STATE: {}, loadRemoteFiles: vi.fn(), setSelectedRemoteFiles: vi.fn(),
        confirmBeforeDelete: false, QueuedTargetChangedError, setConfirmDialog: vi.fn(),
        // Stop, as the GUI controller reaches it with an idle queue.
        stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 }, syncPreflightPendingRef: { current: null }, setSyncPreflightPending: vi.fn(),
        setDeltaEligibilityPrompt: vi.fn(), syncRefreshAbortRef: { current: null }, loadLocalWaitersRef: { current: new Set() },
        remoteSyncRunningRef: { current: false }, activeTransferRef: { current: null }, batchResumeResolverRef: { current: null },
        transferQueue: { hasActiveTransfersNow: () => false, stopPending: vi.fn(), stopAll: vi.fn() }, dispatchTransferToast: vi.fn(),
        setActiveTransfer: vi.fn(), setIsBatchPaused: vi.fn(), setBatchPauseReason: vi.fn(), setSyncCancelling: vi.fn(),
        syncCancelPendingRef: { current: null }, cancelActiveConnect: async () => undefined, hasActiveTransfer: false, hasQueueActivity: false,
    };
    const deleteMany = new Function(...Object.keys(context), `${batchCode};${deleteCode};return deleteMultipleRemoteFiles;`)(...Object.values(context));
    const cancelTransfer = new Function(...Object.keys(context), `${stopCode};return cancelTransfer;`)(...Object.values(context));
    const line = app.split('\n').find(line => line.includes('stop: async () => { await cancelActiveConnect();'))!;
    const controllerContext = { ...context, cancelTransfer };
    const controllerStop = new Function(...Object.keys(controllerContext), `return ({${line.trim()}}).stop;`)(...Object.values(controllerContext));
    const batch = deleteMany(['a', 'b', 'c']);
    await vi.waitFor(() => expect(deleted).toEqual(['/a']));
    expect(context.batchOperationPendingRef.current).not.toBeNull();
    await controllerStop();
    expect(context.batchCancelledRef.current).toBe(true);
    finishFirst();
    await batch;
    expect(deleted).toEqual(['/a']);
    expect(invoke.mock.calls.filter(([cmd]) => cmd === 'provider_delete_file')).toHaveLength(1);
});

it('deletes nothing when Stop comes while the commit message is asked (K5)', async () => {
    const batchCode = await appSlice('  const withBatchLatch =', '  // Issue #591: every GUI entry point', 'k5-batch.ts');
    const deleteCode = await appSlice('  const deleteMultipleRemoteFiles = withBatchLatch(', '  // Moving local items to the trash', 'k5-delete.ts');
    const stopCode = await appSlice('  const cancelTransfer = async', '  // Circuit breaker: attempt reconnection', 'k5-stop.ts');
    const { withTransferRun } = await import('./transferRun');
    const invoke = vi.fn(async (_cmd: string, _args?: Record<string, unknown>) => undefined);
    const remoteFiles = [
        { name: 'a', path: '/a', is_dir: false },
        { name: 'd', path: '/d', is_dir: true },
    ];
    let cancelTransfer!: () => Promise<void>;
    const context = {
        batchOpInFlightRef: { current: false }, batchGenerationRef: { current: 0 }, transfersInFlightRef: { current: 0 }, batchOperationPendingRef: { current: null as Promise<void> | null }, withTransferRun,
        selectedRemoteFiles: new Set<string>(), remoteActionGuard: () => () => undefined,
        prepareGuiTarget: () => Promise.resolve({ epoch: 7 }), resolveGuiTarget: async (prepared: Promise<{ epoch: number }>) => (await prepared).epoch,
        notify: { error: vi.fn(), success: vi.fn() }, t: (key: string) => key,
        liveConnectionParams: { protocol: 'github' }, usesProviderApi: () => true, aeroVaultOverlaySession: null, currentRemotePath: '/',
        // The user presses Stop while the dialog is open, then submits it.
        requestGitHubBatchCommitMessage: async () => { await cancelTransfer(); return 'remove files'; },
        remoteFiles, humanLog: { logStart: () => 'log', updateEntry: vi.fn() }, batchCancelledRef: { current: false }, deleteGesturesRef: { current: [] }, cancelLevelRef: { current: 0 },
        invoke, setScanningState: vi.fn(), INITIAL_SCANNING_STATE: {}, loadRemoteFiles: vi.fn(), setSelectedRemoteFiles: vi.fn(),
        confirmBeforeDelete: false, QueuedTargetChangedError, setConfirmDialog: vi.fn(),
        stopCountRef: { current: 0 }, stopAllCountRef: { current: 0 }, syncPreflightPendingRef: { current: null }, setSyncPreflightPending: vi.fn(),
        setDeltaEligibilityPrompt: vi.fn(), syncRefreshAbortRef: { current: null }, loadLocalWaitersRef: { current: new Set() },
        remoteSyncRunningRef: { current: false }, activeTransferRef: { current: null }, batchResumeResolverRef: { current: null },
        transferQueue: { hasActiveTransfersNow: () => false, stopPending: vi.fn(), stopAll: vi.fn() }, dispatchTransferToast: vi.fn(),
        setActiveTransfer: vi.fn(), setIsBatchPaused: vi.fn(), setBatchPauseReason: vi.fn(), setSyncCancelling: vi.fn(),
        syncCancelPendingRef: { current: null }, cancelActiveConnect: async () => undefined, hasActiveTransfer: false, hasQueueActivity: false,
    };
    const deleteMany = new Function(...Object.keys(context), `${batchCode};${deleteCode};return deleteMultipleRemoteFiles;`)(...Object.values(context));
    cancelTransfer = new Function(...Object.keys(context), `${stopCode};return cancelTransfer;`)(...Object.values(context));
    await deleteMany(['a', 'd']);
    const destructive = invoke.mock.calls.filter(([cmd]) => /delete/.test(cmd));
    expect(destructive).toEqual([]);
});

/**
 * E2: every command a run issues after the sync-start gate carries the run's
 * target: folder creation, backup moves, bulk and single transfers, deletes,
 * folder removal and EC, on a provider (single and bulk) and on a legacy FTP
 * session. The backend refuses each one when that target is no longer the
 * live one (`every_run_command_after_the_gate_refuses_a_switched_target`,
 * `stale_gui_bulk_is_refused_before_the_replacement_provider_is_used`).
 */
describe('the target every run command carries (E2)', () => {
    const AFTER_GATE = new Set([
        'provider_mkdir', 'create_remote_folder', 'create_local_folder',
        'sync_backup_archive_remote', 'sync_backup_archive_local', 'sync_backup_verify_receipt',
        'provider_sync_transfer_bulk', 'provider_upload_file', 'provider_download_file', 'upload_file', 'download_file',
        'provider_delete_file', 'delete_remote_file', 'delete_local_file', 'sync_remove_empty_dir',
        'sync_ec_generate', 'sync_ec_verify_repair',
    ]);
    const run = async (extra: Partial<RemoteSyncConfig>, bulk: boolean, sent: { cmd: string; target: unknown }[]) => {
        const invoke = async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
            if (AFTER_GATE.has(cmd)) {
                const params = args?.params as Record<string, unknown> | undefined;
                sent.push({ cmd, target: params ? params.expected_gui_target : args?.expectedGuiTarget });
            }
            switch (cmd) {
                case 'sync_backup_run_stamp': return 'stamp' as T;
                case 'sync_backup_archive_remote':
                case 'sync_backup_archive_local': return `/kept/${String(args?.rel)}` as T;
                case 'sync_backup_verify_receipt': return true as T;
                case 'sync_remove_empty_dir': return 'removed' as T;
                case 'sync_ec_generate': return { status: 'generated' } as T;
                case 'sync_ec_verify_repair': return { status: 'verified' } as T;
                case 'provider_bulk_support': return { upload: bulk, download: bulk } as T;
                case 'provider_sync_transfer_bulk': return (args?.items as unknown[]).map(() => ({ status: 'completed' })) as T;
                case 'local_target_problems': return [] as T;
                default: return undefined as T;
            }
        };
        const report = await runRemoteSync([
            file('up.txt', 'upload', { overwritesExisting: true }),
            file('docs/new.txt', 'upload'),
            file('down.txt', 'download', { overwritesExisting: true }),
            file('sub/fetched.txt', 'download'),
            file('gone.txt', 'delete-remote'),
            file('old.txt', 'delete-local'),
            file('emptied', 'delete-remote', { isDir: true }),
        ], { remote: ['made-remote'], local: ['made-local'] },
        config({ errorCorrection: { enabled: true, pct: 10 }, ...extra }), {}, {
            invoke: wrapIntentInvoke(invoke), remoteTarget: { ownerBinding: 'test-owner-v1', epoch: 1, assertCurrent: () => {} },
            delay: async () => {}, now: () => 0, makeId: () => 'e2-target', writeIndex: true,
        });
        return report;
    };

    it.each([
        ['a provider, file by file', {}, false, ['provider_mkdir', 'create_local_folder', 'sync_backup_archive_remote', 'sync_backup_archive_local',
            'provider_upload_file', 'provider_download_file', 'provider_delete_file', 'delete_local_file', 'sync_remove_empty_dir', 'sync_ec_generate', 'sync_ec_verify_repair']],
        ['a provider, in bulk', {}, true, ['provider_mkdir', 'create_local_folder', 'sync_backup_archive_remote', 'sync_backup_archive_local',
            'provider_sync_transfer_bulk', 'provider_delete_file', 'delete_local_file', 'sync_remove_empty_dir', 'sync_ec_generate', 'sync_ec_verify_repair']],
        ['a legacy FTP session', { isProvider: false, isFtp: true }, false, ['create_remote_folder', 'create_local_folder', 'sync_backup_archive_remote',
            'sync_backup_archive_local', 'upload_file', 'download_file', 'delete_remote_file', 'delete_local_file', 'sync_remove_empty_dir',
            'sync_ec_generate', 'sync_ec_verify_repair']],
    ] as const)('on %s', async (_name, extra, bulk, expected) => {
        // With a versioned backup a replaced or deleted file is moved into
        // it; without one it is deleted: both runs, one list of calls.
        const sent: { cmd: string; target: unknown }[] = [];
        for (const versionedBackup of [{ dir: '.versions' }, null]) {
            const report = await run({ versionedBackup, ...extra }, bulk, sent);
            expect(report.errors).toEqual([]);
        }
        // The run reached every command family it has for this session.
        expect([...new Set(sent.map(call => call.cmd))].sort()).toEqual([...expected].sort());
        // And each call of each carries the run's target.
        expect(sent.filter(call => call.target !== 1)).toEqual([]);
    });
});
