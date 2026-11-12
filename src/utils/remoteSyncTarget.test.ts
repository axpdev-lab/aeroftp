// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { wrapIntentInvoke } from './testFixtures/syncIntents';
import { describe, expect, it, vi } from 'vitest';
import { runRemoteSync, type RemoteSyncConfig, type SyncRunFile } from './remoteSyncRunner';
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
        const invoke = async <T,>() => undefined as T;
        await expect(runRemoteSync([], { remote: [], local: [] }, config(), {}, { invoke })).rejects.toThrow('bound target');
        const report = await runRemoteSync([], { remote: [], local: [] }, config({ isLocalLocal: true }), {}, { invoke });
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
            const invoke: typeof h.deps.invoke = async (cmd, args) => {
                if (cmd === 'provider_upload_file') {
                    calls.push(cmd);
                    return new Promise(() => {});
                }
                return h.deps.invoke(cmd, args);
            };
            const pending = runRemoteSync([file('one'), file('two')], { remote: [], local: [] },
                config({ retryPolicy: { ...config().retryPolicy, timeout_ms: 50 } }), {}, { ...h.deps, invoke });
            await vi.advanceTimersByTimeAsync(100);
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
        useLatestCallback: (fn: unknown) => fn,
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
        useLatestCallback: (fn: unknown) => fn,
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
        remoteSyncRunningRef: { current: false }, notify: { error: vi.fn(), info: vi.fn() },
        t: (key: string) => key, debugMode: false, runRemoteSync: runner,
    };
    const launch = new Function(...Object.keys(context), `${code}; return runConnectedRemoteSync;`)(...Object.values(context));
    launch([file('one')], { remote: [], local: [] }, { direction: 'local_to_remote', deltaSyncEnabled: true, verifyPolicy: 'none', profileId: 'A' });
    await vi.waitFor(() => expect(prompt).toBeDefined());
    expect(invoke).toHaveBeenCalledWith('sftp_probe_delta_eligibility', { expectedGuiTarget: 7 });
    current = 'B';
    prompt!.onContinue();
    expect(runner).not.toHaveBeenCalled();
    expect(context.notify.error).toHaveBeenCalled();
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
        useLatestCallback: (fn: unknown) => fn,
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
        runLocalLocalSync: vi.fn(), canaryRunningRef: { current: false },
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
        useLatestCallback: (fn: unknown) => fn,
        snapshotQueuedTarget: () => ({ profileId: null, params: { protocol: 'sftp' } }),
        queuedTargetGuard: () => () => {}, QueuedTargetChangedError,
        activeUnifiedRemoteProfile: { id: 'unsaved server name', protocol: 'sftp', savedProfileId: null },
        currentLocalPath: '/local', currentRemotePath: '/remote', usesProviderApi: () => true,
        localStorage: { getItem: () => null }, isCryptOverlayActive: () => false,
        invoke: vi.fn(async (cmd: string) => cmd === 'provider_gui_target_epoch' ? 7 : 'epoch-v1:test:7'),
        loadSavedServerProfiles: async () => [], remoteSyncRunningRef: { current: false },
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
        loadLocalCallIdRef:{current:0},currentLocalPathRef:{current:'/A'},setLocalFiles:vi.fn(),setCurrentLocalPath:vi.fn(),setSelectedLocalFiles:vi.fn(),
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
    const context={report:{cancelled:false,uploaded:1,downloaded:0,deleted:0},epoch:1,checkTarget:()=>{if(!current)throw new Error('changed');},
        loadRemoteFiles:()=>pending,loadLocalFiles:vi.fn(),runConfig:{localRoot:'/old'},currentLocalPathRef:{current:'/old'},
    };
    const refresh=new Function(...Object.keys(context),`${code};return refreshAfterSync;`)(...Object.values(context));
    const result=refresh();current=false;release(null);await result;
    expect(context.loadLocalFiles).not.toHaveBeenCalled();
});

it.each(['before', 'during'] as const)('preserves local navigation %s the remote completion refresh', async when => {
    const { transformWithOxc } = await import('vite'); const { default: app } = await import('../App.tsx?raw');
    const start = app.indexOf('          if (!report.cancelled && (report.uploaded > 0');
    const end = app.indexOf('\n        } catch (err)', start);
    const { code } = await transformWithOxc(`async function refreshAfterSync(){${app.slice(start, end)}}`, 'sync-local-navigation.ts');
    let release!: (value: boolean) => void;
    const pending = new Promise<boolean>(resolve => { release = resolve; });
    const context = {
        report: { cancelled: false, uploaded: 1, downloaded: 0, deleted: 0 }, epoch: 1, checkTarget: vi.fn(),
        loadRemoteFiles: () => pending, loadLocalFiles: vi.fn(), runConfig: { localRoot: '/A' },
        currentLocalPathRef: { current: when === 'before' ? '/B' : '/A' },
    };
    const refresh = new Function(...Object.keys(context), `${code};return refreshAfterSync;`)(...Object.values(context));
    const run = refresh(); context.currentLocalPathRef.current = '/B'; release(true); await run;
    expect(context.loadLocalFiles).not.toHaveBeenCalled();
    expect(context.currentLocalPathRef.current).toBe('/B');
});
