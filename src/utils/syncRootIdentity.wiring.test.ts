// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// The backend names a pair's index, journal and intent files by a hash of the
// exact root strings. The runner normalizes its roots, so every other caller
// that names the pair (Compare, the journal banner, the canary, the journal
// dismissal) has to hand over the same spelling, or it reads another file.

import { transformWithOxc } from 'vite';
import { describe, expect, it, vi } from 'vitest';
import app from '../App.tsx?raw';
import actions from '../components/AeroSync/useAeroSyncContextActions.ts?raw';
import { normalizedSyncRoot, runRemoteSync, type RemoteSyncConfig } from './remoteSyncRunner';
import { wrapIntentInvoke } from './testFixtures/syncIntents';

type Call = [string, Record<string, unknown> | undefined];

function slice(start: string, end: string) {
    const from = app.indexOf(start);
    const to = app.indexOf(end, from);
    if (from < 0 || to < 0) throw new Error(`Missing App closure: ${start}`);
    return app.slice(from, to);
}
const { code: openCode } = await transformWithOxc(
    slice('  const openAeroSync = useLatestCallback((', '  // The compare the dialog is waiting for is stopped'), 'open-aerosync.ts');
const { code: presetCode } = await transformWithOxc(
    slice('  const executeSyncPresetPlan =', '  const clipboardCut ='), 'preset-canary.ts');

/** The pair the real runner names in its lease, intent and index commands. */
async function runnerPairs(localRoot: string, remoteRoot: string, isLocalLocal: boolean) {
    const calls: Call[] = [];
    const config: RemoteSyncConfig = {
        localRoot, remoteRoot, isProvider: !isLocalLocal, isFtp: false, isLocalLocal,
        retryPolicy: { max_retries: 1, base_delay_ms: 1, max_delay_ms: 1, timeout_ms: 0, backoff_multiplier: 1 },
        verifyPolicy: 'none', deltaSyncEnabled: false, transferBudget: 0, direction: 'local_to_remote',
    };
    // The intent fixture answers the lease and intent commands itself: record in front of it.
    const backend = wrapIntentInvoke(async <T,>() => undefined as T);
    await runRemoteSync([{ relativePath: 'one', action: 'upload', size: 1, mtime: null }], { remote: [], local: [] }, config, {}, {
        invoke: <T,>(cmd: string, args?: Record<string, unknown>) => { calls.push([cmd, args]); return backend<T>(cmd, args); },
        remoteTarget: { epoch: 1, ownerBinding: 'owner', assertCurrent: () => undefined },
        delay: async () => undefined, now: () => 0, makeId: () => 'journal', writeIndex: true,
    });
    const pair = (cmd: string) => {
        const args = calls.find(([name]) => name === cmd)?.[1];
        if (!args) throw new Error(`The runner did not call ${cmd}`);
        return [args.localPath, args.remotePath];
    };
    const journal = calls.find(([name]) => name === 'save_sync_journal_cmd')![1]!.journal as { local_path: string; remote_path: string };
    return { intent: pair('sync_intent_snapshot_cmd'), begin: pair('sync_intent_begin_cmd'), index: pair('load_sync_index_cmd'), journal: [journal.local_path, journal.remote_path] };
}

function openAeroSync(roots: { local: string; right: string; localLocal: boolean }) {
    const invoke = vi.fn(async (cmd: string, _args?: Record<string, unknown>) =>
        cmd.startsWith('compare_') || cmd === 'provider_compare_directories' ? { differences: [], summary: {}, modify_window: 0 } : null);
    const context = {
        useLatestCallback: (fn: unknown) => fn, aeroSyncCompareSeqRef: { current: 0 }, batchGenerationRef: { current: 0 }, AEROSYNC_DEFAULT_BACKUP_DIR: '.versions',
        showDualLocalPanel: roots.localLocal, isConnected: !roots.localLocal, showRemotePanel: !roots.localLocal,
        currentLocalPath: roots.local, currentLocalPath2: roots.localLocal ? roots.right : '', currentRemotePath: roots.localLocal ? '/' : roots.right,
        localFiles: [], localFiles2: [], remoteFiles: [], swapPanels: true,
        usesProviderApi: () => true, activeUnifiedRemoteProfile: { id: 'A', name: 'A', protocol: 'sftp', savedProfileId: 'A' },
        isCryptOverlayActive: () => false, aeroCryptVaultId: null, rcloneCryptVaultId: null,
        setAeroSync: vi.fn(), appliedCompareFilters: () => ({}), aeroSyncCompareOptions: () => ({}),
        adaptFileComparisons: () => ({}), compareOutcome: () => ({}), compareEntries: () => ({}),
        isCompareCancelled: () => false, isScanIncompleteError: () => false, describeScanIncompleteError: String,
        invoke, normalizedSyncRoot, notify: { error: vi.fn(), warning: vi.fn() }, t: (key: string) => key,
    };
    const open = new Function(...Object.keys(context), `${openCode}; return openAeroSync;`)(...Object.values(context));
    open('compare', [], '.versions', true);
    return invoke;
}

describe('one pair identity for Compare and the runner', () => {
    it.each([
        ['a trailing separator', '/home/u/work/', '/photos/', ['/home/u/work', '/photos']],
        ['a backslash drive root', 'C:\\', '/', ['C:/', '/']],
        ['a forward-slash drive root', 'C:/', '/', ['C:/', '/']],
    ] as const)('names the connected pair with %s as the runner does', async (_label, local, remote, expected) => {
        const runner = await runnerPairs(local, remote, false);
        expect(runner).toEqual({ intent: expected, begin: expected, index: expected, journal: expected });
        const invoke = openAeroSync({ local, right: remote, localLocal: false });
        await vi.waitFor(() => expect(invoke.mock.calls.map(([cmd]) => cmd).sort()).toEqual(['load_sync_journal_cmd', 'provider_compare_directories']));
        const args = (cmd: string) => invoke.mock.calls.find(([name]) => name === cmd)![1]!;
        // provider_compare_directories reads the index and the intents of the pair it is given.
        expect([args('provider_compare_directories').localPath, args('provider_compare_directories').remotePath]).toEqual(runner.index);
        expect([args('provider_compare_directories').localPath, args('provider_compare_directories').remotePath]).toEqual(runner.intent);
        expect([args('load_sync_journal_cmd').localPath, args('load_sync_journal_cmd').remotePath]).toEqual(runner.journal);
    });

    it.each([
        ['/home/u/left/', '/home/u/right/', ['/home/u/left', '/home/u/right']],
        ['C:\\', 'D:\\', ['C:/', 'D:/']],
    ] as const)('names the local-local pair %s and %s as the runner does', async (left, right, expected) => {
        const runner = await runnerPairs(left, right, true);
        expect(runner.index).toEqual(expected);
        const invoke = openAeroSync({ local: left, right, localLocal: true });
        await vi.waitFor(() => expect(invoke.mock.calls.map(([cmd]) => cmd).sort()).toEqual(['compare_local_directories', 'load_sync_journal_cmd']));
        const args = (cmd: string) => invoke.mock.calls.find(([name]) => name === cmd)![1]!;
        expect([args('compare_local_directories').leftPath, args('compare_local_directories').rightPath]).toEqual(runner.index);
        expect([args('load_sync_journal_cmd').localPath, args('load_sync_journal_cmd').remotePath]).toEqual(runner.journal);
    });

    it('samples the canary against the pair the runner indexes', async () => {
        const runner = await runnerPairs('/home/u/work/', '/photos/', false);
        const invoke = vi.fn(async (_cmd: string, _args?: Record<string, unknown>) => ({ sample: [] }));
        const shown = vi.fn();
        const context = {
            useLatestCallback: (fn: unknown) => fn, aeroSync: { context: { pairKind: 'local-remote', activeProfileId: 'A' } },
            setAeroSync: vi.fn(), debugMode: false, snapshotQueuedTarget: () => ({ profileId: 'A', params: { protocol: 'sftp' } }),
            queuedTargetGuard: () => () => undefined, prepareGuiTarget: () => Promise.resolve({ epoch: 7 }),
            currentLocalPath: '/home/u/work/', currentRemotePath: '/photos/', isCryptOverlayActive: () => false,
            buildRemoteSyncInput: () => ({ files: [{ relativePath: 'one', action: 'upload', size: 1, mtime: null }], dirs: { remote: [], local: [] } }),
            SPEED_PRESETS: { balanced: { deltaSyncEnabled: false } }, runConnectedRemoteSync: vi.fn(), runLocalLocalSync: vi.fn(),
            canaryRunningRef: { current: false }, normalizedSyncRoot, invoke, setCanaryResult: shown,
            notify: { error: vi.fn(), info: vi.fn() }, t: (key: string) => key,
        };
        const launch = new Function(...Object.keys(context), `${presetCode}; return executeSyncPresetPlan;`)(...Object.values(context));
        launch({ preset: 'mirror', direction: 'left-to-right' }, { speedMode: 'balanced', verifyPolicy: 'none', canary: { percent: 10, selection: 'first' } });
        await vi.waitFor(() => expect(shown).toHaveBeenCalled());
        const [cmd, args] = invoke.mock.calls[0];
        expect(cmd).toBe('sync_canary_run');
        expect([args!.localPath, args!.remotePath]).toEqual(runner.index);
    });

    it('dismisses the journal of the pair the runner wrote it under', async () => {
        const start = actions.indexOf('    const dismissJournal = useCallback(');
        const end = actions.indexOf('    const invalidateComparison =', start);
        if (start < 0 || end < 0) throw new Error('dismissJournal not found');
        const { code } = await transformWithOxc(actions.slice(start, end), 'dismiss-journal.ts');
        const runner = await runnerPairs('/home/u/work/', '/photos/', false);
        const invoke = vi.fn(async (_cmd: string, _args?: Record<string, unknown>) => undefined);
        const context = {
            useCallback: (fn: unknown) => fn, invoke, normalizedSyncRoot, setState: vi.fn(),
            context: { initialSource: '/home/u/work/', initialDestination: '/photos/' },
        };
        const dismiss = new Function(...Object.keys(context), `${code}; return dismissJournal;`)(...Object.values(context));
        await dismiss();
        const [cmd, args] = invoke.mock.calls[0];
        expect(cmd).toBe('delete_sync_journal_cmd');
        expect([args!.localPath, args!.remotePath]).toEqual(runner.journal);
    });
});
