// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import { describe, expect, it, vi } from 'vitest';
import APP from '../App.tsx?raw';
import CODE_BLOCK from '../components/DevTools/CodeBlockActions.tsx?raw';
import CODE_EDITOR from '../components/DevTools/CodeEditor.tsx?raw';
import DEVTOOLS from '../components/DevTools/DevToolsV2.tsx?raw';
import AI_CHAT from '../components/DevTools/AIChat.tsx?raw';
import MESSAGE_ROW from '../components/DevTools/ChatMessageRow.tsx?raw';
import MARKDOWN from '../components/DevTools/MarkdownRenderer.tsx?raw';
import PREVIEW from '../hooks/usePreview.ts?raw';
import DRAG from '../hooks/useDragAndDrop.ts?raw';
import { INBOX, LOCAL, REMOTE, SUB, fixture, local, remote } from '../components/appTransfers.testkit';
import { QueuedTargetChangedError } from './queuedTarget';
import { oauthLoginKey } from './oauthLogins';
import { connectionNumberOf, isSameConnection, savedProfileIdOf } from './liveConnection';
import { isFourSharedProvider, isOAuthProvider } from '../types';

/**
 * Requested 184: an action landed on the wrong target. "Apply" on a code block
 * wrote a remote file's path through the local filesystem (deep scan item 9),
 * and a queued upload started or retried after a tab switch went to the server
 * of the new tab (item 6).
 */

// A pin reads the code alone: a comment may name a call, say "await" or
// mention what a check looks for. Code run by a test keeps its comments.
// App's own list of the account-key options, read from its source.
// Interactive OAuth logins, as App's counter sees them; a test can add one.
const oauthLogins = new Map<string, number>();
const loginOn = (key: string) => oauthLogins.set(key, (oauthLogins.get(key) ?? 0) + 1);
const ACCOUNT_KEY_OPTIONS = new Function(`return ${APP.match(/const ACCOUNT_KEY_OPTIONS = (\[[^\]]*\])/)![1]};`)() as string[];

const codeOnly = (source: string) => source.split('\n').filter(line => !/^\s*(\/\/|\/\*|\*)/.test(line)).join('\n');
const APP_CODE = codeOnly(APP);

const between = (source: string, start: string, end: string) => {
    const a = source.indexOf(start);
    const b = source.indexOf(end, a + start.length);
    if (a < 0 || b < 0) throw new Error(`Missing block: ${start}`);
    return source.slice(a, b);
};

describe('Apply on a remote file', () => {
    it('goes into the editor buffer, never through the local filesystem', () => {
        const apply = between(codeOnly(CODE_BLOCK), 'const handleApplyToFile = useCallback(', '}, [code, language');
        const remote = between(apply, 'if (editorFilePath && editorFileIsRemote) {', '} else if (editorFilePath) {');
        expect(remote).toContain('applyToEditorBuffer({ path: editorFilePath, isRemote: true }, code)');
        expect(remote).not.toContain('writeTextFile');
        // Without the editor showing the file the action says so (CodeBlockActions.test.tsx).
        expect(remote).toContain("showUnreachable('apply')");
        const diff = between(codeOnly(CODE_BLOCK), 'const handleDiff = useCallback(', '}, [editorFilePath, editorFileIsRemote, showDiff, showUnreachable]);');
        const remoteDiff = between(diff, 'if (editorFileIsRemote) {', 'try {');
        expect(remoteDiff).toContain('readEditorBuffer({ path: editorFilePath, isRemote: true })');
        expect(remoteDiff).not.toContain('readTextFile');
    });

    it('knows the open file is remote, from DevTools down to the code block', () => {
        expect(DEVTOOLS).toContain('editorFileIsRemote={!!editorFile?.isRemote}');
        expect(AI_CHAT).toContain('editorFileIsRemote={editorFileIsRemote}');
        expect(MESSAGE_ROW).toContain('editorFileIsRemote={editorFileIsRemote}');
        expect(MESSAGE_ROW).toContain('prev.editorFileIsRemote === next.editorFileIsRemote');
        expect(MARKDOWN).toContain('editorFileIsRemote={editorFileIsRemote}');
        expect(MARKDOWN).toContain('prev.editorFileIsRemote === next.editorFileIsRemote');
        expect(CODE_EDITOR).toContain('window.addEventListener(EDITOR_APPLY_EVENT, handleApply);');
        expect(CODE_EDITOR).toContain('window.addEventListener(EDITOR_READ_EVENT, handleRead);');
        // An Apply is one undoable edit, so it never discards unsaved work.
        const apply = between(codeOnly(CODE_EDITOR), 'const handleApply = (e: Event) => {', 'detail.accept();');
        expect(apply).toContain("editor.executeEdits('aeroagent-apply'");
        expect(apply).toContain('editor.pushUndoStop();');
        expect(apply).not.toContain('setValue(');
    });
});

describe('a queued transfer', () => {
    it('is bound to its server on every Start and Retry the user queues', () => {
        const registrations = APP_CODE.split('retryCallbacksRef.current.set(').slice(1).map(rest => rest.slice(0, 120));
        // The restored-journal retry has its own profile ownership check.
        const restored = registrations.filter(r => r.startsWith('id, () => retryRestoredEntry('));
        const bound = registrations.filter(r => r.startsWith('id, boundToQueuedServer(queuedTarget, id, '));
        expect(restored).toHaveLength(1);
        expect(bound).toHaveLength(5);
        expect(registrations).toHaveLength(restored.length + bound.length);
    });

    // The listing and the overwrite questions before the registrations give
    // the user time to switch tabs: the target is taken before any of them.
    it.each(['const runFileBatch = useLatestCallback(', 'const uploadMultipleFiles = withBatchLatch(', 'const downloadMultipleFiles = withBatchLatch('])(
        'takes its target before the first await in %s',
        (head) => {
            const body = APP_CODE.slice(APP_CODE.indexOf(head));
            const snapshot = body.search(/const queuedTarget = (options\.queuedTarget \?\? )?snapshotQueuedTarget\(\);/);
            expect(snapshot).toBeGreaterThan(0);
            expect(snapshot).toBeLessThan(body.indexOf('await '));
        },
    );

    // A Retry runs App's own target check (the testkit runs the same
    // closures), with the real comparison of connections.
    const queued = { server: 'a.example', protocol: 'sftp' as const, port: 22, username: 'u', password: 'key-1', savedServerId: 'profile-1', options: { bucket: 'b1' } };
    // A vault id is the overlay's kind and owner: two vaults of one kind share it.
    type Overlay = { vaultId: string; kind: 'aerocrypt' | 'rclone-crypt'; remoteScope?: string } | null;
    const vault: Overlay = { vaultId: 'aerocrypt:owner', kind: 'aerocrypt', remoteScope: '/Vault' };
    it.each([
        ['runs on its server, account and overlay', {}, vault, 'run'],
        ['runs on the same vault unlocked again', {}, { ...vault! }, 'run'],
        ['fails on another host', { server: 'b.example' }, vault, 'transfer.queuedForAnotherServer'],
        ['fails on the same host with another port', { port: 2222 }, vault, 'transfer.queuedForAnotherServer'],
        ['fails on the same host with another bucket', { options: { bucket: 'b2' } }, vault, 'transfer.queuedForAnotherServer'],
        ['fails on the same fields with another API key (FileLu)', { password: 'key-2' }, vault, 'transfer.queuedForAnotherServer'],
        ['fails on the same fields with another profile (OAuth)', { savedServerId: 'profile-2' }, vault, 'transfer.queuedForAnotherServer'],
        ['fails when its overlay was locked', {}, null, 'transfer.queuedOverlayChanged'],
        ['fails under another vault of the same kind', {}, { ...vault!, remoteScope: '/Other' }, 'transfer.queuedOverlayChanged'],
    ] as const)('%s', async (_name, change, liveOverlay, outcome) => {
        const source = between(APP, '  const liveOverlay = useLatestCallback(', '\n  const queuedTargetGuard =');
        const run = vi.fn();
        const session = { id: 'A', connectionParams: { ...queued }, cryptOverlay: vault as Overlay };
        const ctx = {
            useLatestCallback: (fn: unknown) => fn, syncCancelPendingRef: { current: null }, syncRunnerPendingRef: { current: null }, stopCountRef: { current: 0 },
            isConnected: true,
            remoteConnectPhaseRef: { current: false }, backendTeardownsRef: { current: 0 }, backendUncertainRef: { current: false }, backendConfirmationsRef: { current: 0 },
            sessions: [session],
            activeSessionId: 'A',
            liveConnectionParams: session.connectionParams,
            isSameConnection,
            savedProfileIdOf,
            isOAuthProvider,
            isFourSharedProvider,
            connectionNumberOf,
            ACCOUNT_KEY_OPTIONS,
            oauthLoginCount: (key: string) => oauthLogins.get(key) ?? 0, oauthLoginKey,
            transferQueue: { failTransfer: vi.fn() },
            t: (key: string) => key,
        };
        const { code } = await transformWithOxc(source, 'bound.ts');
        const fns = new Function(...Object.keys(ctx), `${code}; return { snapshotQueuedTarget, boundToQueuedServer };`)(...Object.values(ctx));
        const queuedFor = fns.snapshotQueuedTarget();
        // The switch: the same session object as App's state would hold after it.
        Object.assign(session.connectionParams, change);
        session.cryptOverlay = liveOverlay;
        await fns.boundToQueuedServer(queuedFor, 'item-1', run)();
        if (outcome === 'run') {
            expect(run).toHaveBeenCalledTimes(1);
            expect(ctx.transferQueue.failTransfer).not.toHaveBeenCalled();
        } else {
            expect(run).not.toHaveBeenCalled();
            expect(ctx.transferQueue.failTransfer).toHaveBeenCalledWith('item-1', outcome);
        }
    });

    // switchSession activates the new tab, then reconnects the backend: in
    // between, the live tab names the new server while the backend is still
    // on the old one. Nothing bound to the new tab runs until it has.
    it('fails while a tab switch or a connect is still reconnecting the backend', async () => {
        const source = between(APP, '  const liveOverlay = useLatestCallback(', '\n  const queuedTargetGuard =');
        const session = { id: 'A', connectionParams: { ...queued }, cryptOverlay: null };
        const ctx = {
            useLatestCallback: (fn: unknown) => fn, syncCancelPendingRef: { current: null }, syncRunnerPendingRef: { current: null }, stopCountRef: { current: 0 }, isConnected: true, remoteConnectPhaseRef: { current: false as false | 'switch' }, backendTeardownsRef: { current: 0 }, backendUncertainRef: { current: false }, backendConfirmationsRef: { current: 0 },
            sessions: [session], activeSessionId: 'A', liveConnectionParams: session.connectionParams,
            isSameConnection, savedProfileIdOf, isOAuthProvider, isFourSharedProvider, connectionNumberOf, ACCOUNT_KEY_OPTIONS, oauthLoginCount: (key: string) => oauthLogins.get(key) ?? 0, oauthLoginKey, transferQueue: { failTransfer: vi.fn() }, t: (key: string) => key,
        };
        const { code } = await transformWithOxc(source, 'bound.ts');
        const fns = new Function(...Object.keys(ctx), `${code}; return { snapshotQueuedTarget, queuedTargetProblem };`)(...Object.values(ctx));
        // The new tab's target, taken while the backend is still the old one's.
        ctx.remoteConnectPhaseRef.current = 'switch';
        const target = fns.snapshotQueuedTarget();
        expect(fns.queuedTargetProblem(target)).toBe('transfer.queuedForAnotherServer');
        ctx.remoteConnectPhaseRef.current = false;
        expect(fns.queuedTargetProblem(target)).toBeNull();
    });

    // App's own target closures over one session the test changes in place.
    const targetChecks = async (params: Record<string, unknown>, extra: Record<string, unknown> = {}) => {
        const source = between(APP, '  const liveOverlay = useLatestCallback(', '\n  const queuedTargetGuard =')
            + (extra.withAbort ? between(APP, '  const boundedTeardown = async (', '\n  const lockAndDropBackendConnection =')
                + between(APP, '  const abortSwitchAfterCancel = async (', '\n  // #401') : '');
        const session = { id: 'A', connectionParams: { ...params }, cryptOverlay: null };
        const ctx = {
            useLatestCallback: (fn: unknown) => fn, syncCancelPendingRef: { current: null }, syncRunnerPendingRef: { current: null }, stopCountRef: { current: 0 }, isConnected: true,
            remoteConnectPhaseRef: { current: false as false | 'switch' }, backendTeardownsRef: { current: 0 }, backendUncertainRef: { current: false }, backendConfirmationsRef: { current: 0 },
            sessions: [session], activeSessionId: 'A', liveConnectionParams: session.connectionParams,
            isSameConnection, savedProfileIdOf, isOAuthProvider, isFourSharedProvider, connectionNumberOf, ACCOUNT_KEY_OPTIONS, oauthLoginCount: (key: string) => oauthLogins.get(key) ?? 0, oauthLoginKey, transferQueue: { failTransfer: vi.fn() }, t: (key: string) => key,
            BACKEND_TEARDOWN_BOUND_MS: 30000, clearCryptOverlayState: vi.fn(),
            lockAndDropBackendConnection: (extra.lockAndDrop as (() => Promise<boolean>) | undefined) ?? vi.fn(() => new Promise<boolean>(() => {})),
        };
        const { code } = await transformWithOxc(source, 'targets.ts');
        const names = ['snapshotQueuedTarget', 'queuedTargetProblem', ...(extra.withAbort ? ['abortSwitchAfterCancel'] : [])];
        const fns = new Function(...Object.keys(ctx), `${code}; return { ${names.join(', ')} };`)(...Object.values(ctx));
        return { fns, ctx, session };
    };

    // A Quick Connect is queued under no profile, and a reconnect can link the
    // session to a saved one: the same fields and key are the same account.
    it.each([
        ['runs once the session is linked to a saved profile', { savedServerId: 'profile-9' }, null],
        ['fails on another key after the link (FileLu)', { savedServerId: 'profile-9', password: 'key-2' }, 'transfer.queuedForAnotherServer'],
        ['fails on another host after the link', { savedServerId: 'profile-9', server: 'b.example' }, 'transfer.queuedForAnotherServer'],
    ] as const)('a Quick Connect retry %s', async (_name, change, outcome) => {
        const { savedServerId: _unlinked, ...quick } = queued;
        const { fns, session } = await targetChecks(quick);
        const target = fns.snapshotQueuedTarget();
        expect(target.profileId).toBeNull();
        Object.assign(session.connectionParams, change);
        expect(fns.queuedTargetProblem(target)).toBe(outcome);
    });

    // An OAuth account is in a token the params do not carry: two accounts of
    // one provider share every field, and only the profile tells them apart.
    it('an OAuth Quick Connect queued under no profile refuses a profile linked later', async () => {
        const oauth = { server: '', protocol: 'googledrive', port: 443, username: '', password: '' };
        const { fns, session } = await targetChecks(oauth);
        const target = fns.snapshotQueuedTarget();
        expect(target.profileId).toBeNull();
        expect(fns.queuedTargetProblem(target)).toBeNull();
        Object.assign(session.connectionParams, { savedServerId: 'profile-other-account' });
        expect(fns.queuedTargetProblem(target)).toBe('transfer.queuedForAnotherServer');
    });

    // Two OAuth Quick Connects of one provider with no saved profile share
    // every field: only the connection a batch was queued on is its account.
    it('an unlinked OAuth target refuses another unlinked OAuth connection of the same provider', async () => {
        const oauth = { server: '', protocol: 'googledrive', port: 443, username: '', password: '' };
        const { fns } = await targetChecks(oauth);
        const target = fns.snapshotQueuedTarget();
        expect(fns.queuedTargetProblem(target)).toBeNull();
        // Another login with the same fields: a connection of its own.
        const anotherLogin = await targetChecks(oauth);
        expect(anotherLogin.fns.queuedTargetProblem(target)).toBe('transfer.queuedForAnotherServer');
        // A batch queued in another tab, now looked at from this one.
        expect(fns.queuedTargetProblem({ ...target, sessionId: 'B' })).toBe('transfer.queuedForAnotherServer');
        // The same connection, untouched: still its own.
        expect(fns.queuedTargetProblem(target)).toBeNull();
    });

    // Switching back to an OAuth tab whose token expired logs in again on the
    // same tab and the same params object, and the user can pick another
    // account there: an OAuth target taken before a login is refused.
    it.each([
        ['with a saved profile', { savedServerId: 'gdrive-1' }],
        ['without one', {}],
    ])('an OAuth target %s refuses a Retry after a login on the same tab', async (_name, linked) => {
        const oauth = { server: '', protocol: 'googledrive', port: 443, username: '', password: '', ...linked };
        const { fns } = await targetChecks(oauth);
        const target = fns.snapshotQueuedTarget();
        expect(fns.queuedTargetProblem(target)).toBeNull();
        // A login on another profile's token leaves this target alone.
        loginOn(oauthLoginKey('googledrive', 'gdrive-unrelated'));
        expect(fns.queuedTargetProblem(target)).toBeNull();
        loginOn(oauthLoginKey('googledrive', (linked as { savedServerId?: string }).savedServerId));
        expect(fns.queuedTargetProblem(target)).toBe('transfer.queuedForAnotherServer');
        // A target taken after the login is the account chosen there.
        expect(fns.queuedTargetProblem(fns.snapshotQueuedTarget())).toBeNull();
    });

    // CodeRabbit on #1526: a Quick Connect login names Zoho `zoho_workdrive`,
    // its connection `zohoworkdrive`; the two must be the same token owner.
    it('an unlinked Zoho target refuses a Retry after a login spelled the OAuth way', async () => {
        const zoho = { server: '', protocol: 'zohoworkdrive', port: 443, username: '', password: '' };
        const { fns } = await targetChecks(zoho);
        const target = fns.snapshotQueuedTarget();
        expect(fns.queuedTargetProblem(target)).toBeNull();
        loginOn(oauthLoginKey('zoho_workdrive', undefined));
        expect(fns.queuedTargetProblem(target)).toBe('transfer.queuedForAnotherServer');
    });

    it('a target whose account is in the params is not refused by an OAuth login elsewhere', async () => {
        const { fns } = await targetChecks(queued);
        const target = fns.snapshotQueuedTarget();
        loginOn(oauthLoginKey('googledrive', null));
        expect(fns.queuedTargetProblem(target)).toBeNull();
    });

    // Filen logs in with an API key the target comparison leaves out as a
    // credential: another key is another account.
    it.each(['filen_api_key', 'accessKey', 'apiKey', 'clientSecret', 'sasToken'])('refuses another %s, which picks the account', async (key) => {
        const params = { ...queued, options: { bucket: 'b1', [key]: 'key-a' } };
        const { fns, session } = await targetChecks(params);
        const target = fns.snapshotQueuedTarget();
        expect(fns.queuedTargetProblem(target)).toBeNull();
        session.connectionParams.options = { bucket: 'b1', [key]: 'key-b' };
        expect(fns.queuedTargetProblem(target)).toBe('transfer.queuedForAnotherServer');
    });

    // End to end: an actual Retry and an actual editor read, through App's own
    // check, while a tab switch is still reconnecting the backend.
    it('a Retry clicked while a switch reconnects the backend uploads nothing', async () => {
        const f = fixture({ folders: { [LOCAL]: [local('up.txt')] }, fail: (command, call) => (command === 'provider_upload_file' && call === 1 ? 'Connection reset' : undefined) });
        await f.drop([local('up.txt')], false, REMOTE, 'local');
        f.connectPhase.current = 'switch';
        await f.retryAllFailedItems();
        expect(f.invoke.mock.calls.filter(([command]) => command === 'provider_upload_file')).toHaveLength(1);
        expect(f.row('q0')).toMatchObject({ status: 'error', error: 'transfer.queuedForAnotherServer' });
        f.connectPhase.current = false;
        await f.retryAllFailedItems();
        expect(f.row('q0')).toMatchObject({ status: 'completed' });
    });

    it('a Retry clicked while a cancelled switch still tears the backend down uploads nothing', async () => {
        const f = fixture({ folders: { [LOCAL]: [local('up.txt')] }, fail: (command, call) => (command === 'provider_upload_file' && call === 1 ? 'Connection reset' : undefined) });
        await f.drop([local('up.txt')], false, REMOTE, 'local');
        // The switch's phase is over; its teardown is still running.
        f.teardowns.current = 1;
        await f.retryAllFailedItems();
        expect(f.invoke.mock.calls.filter(([command]) => command === 'provider_upload_file')).toHaveLength(1);
        expect(f.row('q0')).toMatchObject({ status: 'error', error: 'transfer.queuedForAnotherServer' });
        f.teardowns.current = 0;
        await f.retryAllFailedItems();
        expect(f.row('q0')).toMatchObject({ status: 'completed' });
    });

    it('an editor opened while a switch reconnects the backend reads nothing', async () => {
        const { fns, ctx } = await targetChecks(queued);
        const source = between(PREVIEW, '  const readForEditor = useLatestCallback(', '\n  });\n') + '\n  });';
        const invoke = vi.fn(async (command: string) => (command === 'provider_connection_generation' ? 7 : 'old server'));
        const remoteTarget = {
            snapshot: () => fns.snapshotQueuedTarget(),
            changedSince: (target: unknown) => fns.queuedTargetProblem(target) !== null,
            changedMessage: () => 'toast.openedOnAnotherServer',
        };
        const { code } = await transformWithOxc(source, 'read-for-editor.ts');
        const readForEditor = new Function('useLatestCallback', 'invoke', 'remoteTarget', `${code}; return readForEditor;`)(
            (fn: unknown) => fn, invoke, remoteTarget);
        ctx.remoteConnectPhaseRef.current = 'switch';
        await expect(readForEditor('/srv/a.txt', true)).rejects.toThrow('toast.openedOnAnotherServer');
        expect(invoke).not.toHaveBeenCalledWith('preview_remote_file', expect.anything());
        ctx.remoteConnectPhaseRef.current = false;
        await expect(readForEditor('/srv/a.txt', true)).resolves.toMatchObject({ content: 'old server' });
    });

    it('still refuses a profile changed from the one a batch was queued under', async () => {
        const { fns, session } = await targetChecks(queued);
        const target = fns.snapshotQueuedTarget();
        Object.assign(session.connectionParams, { savedServerId: 'profile-2' });
        expect(fns.queuedTargetProblem(target)).toBe('transfer.queuedForAnotherServer');
    });

    // A teardown that hangs is abandoned after its bound: the backend is then
    // uncertain, so nothing runs on the live tab until a new connect or switch
    // replaces the connection, instead of until a restart.
    it('marks the backend uncertain when a cancelled switch\'s teardown hangs, until the next connect', async () => {
        vi.useFakeTimers();
        try {
            const { fns, ctx } = await targetChecks(queued, { withAbort: true });
            const target = fns.snapshotQueuedTarget();
            ctx.remoteConnectPhaseRef.current = 'switch';
            const aborting = fns.abortSwitchAfterCancel(() => true);
            expect(fns.queuedTargetProblem(target)).toBe('transfer.queuedForAnotherServer');
            await vi.advanceTimersByTimeAsync(30000);
            await aborting;
            expect(ctx.backendTeardownsRef.current).toBe(0);
            expect(ctx.backendUncertainRef.current).toBe(true);
            expect(fns.queuedTargetProblem(target)).toBe('transfer.queuedForAnotherServer');
            expect(ctx.clearCryptOverlayState).not.toHaveBeenCalled();
        } finally {
            vi.useRealTimers();
        }
        // A connect or switch that only starts does not clear it (one that fails
        // early, on a missing field or a refused host key, replaced nothing):
        // the points where a tab is confirmed connected do, and only they.
        expect(APP_CODE).not.toMatch(/remoteConnectPhaseRef\.current = '(connect|switch)';\n\s+confirmBackendConnection\(\);/);
        // The flag is cleared in one helper, which the confirmations call.
        expect(APP_CODE.match(/backendUncertainRef\.current = false;/g)).toHaveLength(1);
        expect(APP_CODE).toMatch(/const confirmBackendConnection = \(\) => \{\n\s+backendUncertainRef\.current = false;\n\s+backendConfirmationsRef\.current \+= 1;/);
        expect(APP_CODE.match(/confirmBackendConnection\(\);/g)).toHaveLength(4);
        for (const confirmed of [
            /confirmBackendConnection\(\);\n\s+const newSession: FtpSession = \{\n\s+id: `session_mtp_/,
            /confirmBackendConnection\(\);\n\s+const newSession: FtpSession = \{\n\s+id: `session_\$\{/,
            /s\.id === reusable\.id \? \{ \.\.\.s, status: 'connected' \} : s\)\);\n\s+confirmBackendConnection\(\);/,
            /s\.id === sessionId \? \{ \.\.\.s, status: 'connected' \} : s\)\);\n\s+confirmBackendConnection\(\);/,
        ]) expect(APP_CODE).toMatch(confirmed);
    });

    it('lets a teardown that timed out after a newer connection was confirmed leave it alone', async () => {
        vi.useFakeTimers();
        try {
            const { fns, ctx } = await targetChecks(queued, { withAbort: true });
            const target = fns.snapshotQueuedTarget();
            let owner = true;
            const aborting = fns.abortSwitchAfterCancel(() => owner);
            // A newer connect takes the backend over and connects a tab
            // before the bound ends.
            owner = false;
            ctx.backendConfirmationsRef.current += 1;
            await vi.advanceTimersByTimeAsync(30000);
            await aborting;
            expect(ctx.backendUncertainRef.current).toBe(false);
            expect(ctx.backendTeardownsRef.current).toBe(0);
            expect(fns.queuedTargetProblem(target)).toBeNull();
        } finally {
            vi.useRealTimers();
        }
    });

    // CodeRabbit on #1526: a newer connect took the backend over (so the old
    // teardown no longer owned it) and then stopped before connecting, on a
    // missing field or a refused host key. The old timeout left the backend
    // marked known, and a queued Retry ran on whatever it was.
    it('marks the backend uncertain when the newer connect that took over stopped before connecting', async () => {
        vi.useFakeTimers();
        try {
            const { fns, ctx } = await targetChecks(queued, { withAbort: true });
            const target = fns.snapshotQueuedTarget();
            let owner = true;
            const aborting = fns.abortSwitchAfterCancel(() => owner);
            // The newer connect takes over, then fails without confirming.
            owner = false;
            await vi.advanceTimersByTimeAsync(30000);
            await aborting;
            expect(ctx.backendUncertainRef.current).toBe(true);
            expect(ctx.backendTeardownsRef.current).toBe(0);
            expect(fns.queuedTargetProblem(target)).toBe('transfer.queuedForAnotherServer');
        } finally {
            vi.useRealTimers();
        }
    });

    // A cancelled switch ends its phase, then drops the half-open backend:
    // reads and retries stay blocked until that teardown is over.
    it('stays blocked while a cancelled switch is still tearing the backend down', async () => {
        let teardownDone!: (dropped: boolean) => void;
        const lockAndDrop = vi.fn(() => new Promise<boolean>(done => { teardownDone = done; }));
        const { fns, ctx } = await targetChecks(queued, { withAbort: true, lockAndDrop });
        const target = fns.snapshotQueuedTarget();
        ctx.remoteConnectPhaseRef.current = 'switch';
        const aborting = fns.abortSwitchAfterCancel(() => true);
        // The phase is over, the teardown is not.
        expect(ctx.remoteConnectPhaseRef.current).toBe(false);
        expect(fns.queuedTargetProblem(target)).toBe('transfer.queuedForAnotherServer');
        // End to end: the hook's own editor read, through App's own check,
        // reads nothing while the teardown is paused.
        const source = between(PREVIEW, '  const readForEditor = useLatestCallback(', '\n  });\n') + '\n  });';
        const invoke = vi.fn(async (command: string) => (command === 'provider_connection_generation' ? 7 : 'old server'));
        const remoteTarget = {
            snapshot: () => fns.snapshotQueuedTarget(),
            changedSince: (queuedFor: unknown) => fns.queuedTargetProblem(queuedFor) !== null,
            changedMessage: () => 'toast.openedOnAnotherServer',
        };
        const { code } = await transformWithOxc(source, 'read-for-editor.ts');
        const readForEditor = new Function('useLatestCallback', 'invoke', 'remoteTarget', `${code}; return readForEditor;`)(
            (fn: unknown) => fn, invoke, remoteTarget);
        await expect(readForEditor('/srv/a.txt', true)).rejects.toThrow('toast.openedOnAnotherServer');
        expect(invoke).not.toHaveBeenCalledWith('preview_remote_file', expect.anything());
        teardownDone(true);
        await aborting;
        expect(fns.queuedTargetProblem(target)).toBeNull();
        expect(ctx.backendTeardownsRef.current).toBe(0);
        expect(ctx.backendUncertainRef.current).toBe(false);
        // A cancelled connect tears down through the same bounded, counted step.
        const connect = between(APP_CODE, '  const abortConnectAfterCancel = async (', 'clearCryptOverlayState();');
        expect(connect).toContain('if (!await boundedTeardown(owns) || !owns()) return;');
        expect(connect).not.toContain('lockAndDropBackendConnection(');
    });

    it.each([
        ['another port', { port: 2222 }],
        ['another bucket', { options: { bucket: 'other' } }],
        ['another API key', { password: 'key-2' }],
        ['another profile', { savedServerId: 'profile-2' }],
    ] as const)('fails its Retry without uploading after a switch to the same host with %s', async (_name, change) => {
        const f = fixture({ folders: { [LOCAL]: [local('up.txt')] }, fail: (command, call) => (command === 'provider_upload_file' && call === 1 ? 'Connection reset' : undefined) });
        await f.drop([local('up.txt')], false, REMOTE, 'local');
        expect(f.row('q0')).toMatchObject({ status: 'error' });
        f.switchServer(change);
        // Retry All Failed: the queue reopens the row, then runs its callback.
        await f.retryAllFailedItems();
        expect(f.invoke.mock.calls.filter(([command]) => command === 'provider_upload_file')).toHaveLength(1);
        expect(f.row('q0')).toMatchObject({ status: 'error', error: 'transfer.queuedForAnotherServer' });
    });

    it('runs its Retry when the server it was queued for is live', async () => {
        const f = fixture({ folders: { [LOCAL]: [local('up.txt')] }, fail: (command, call) => (command === 'provider_upload_file' && call === 1 ? 'Connection reset' : undefined) });
        await f.drop([local('up.txt')], false, REMOTE, 'local');
        // Retry All Failed: the queue reopens the row, then runs its callback.
        await f.retryAllFailedItems();
        expect(f.written()).toEqual([`${REMOTE}/up.txt`, `${REMOTE}/up.txt`]);
        expect(f.row('q0')).toMatchObject({ status: 'completed' });
    });

    it('refuses the actual Retry when its overwrite prompt switches the target', async () => {
        const f = fixture({ folders: { [LOCAL]: [local('up.txt')], [REMOTE]: [remote('up.txt')] },
            policy: 'ask', fail: (command, call) => command === 'provider_upload_file' && call === 1 ? 'Connection reset' : undefined });
        f.checkOverwrite.mockResolvedValue({ action: 'overwrite', destinationExists: true });
        await f.drop([local('up.txt')], false, REMOTE, 'local');
        expect(f.row('q0')).toMatchObject({ status: 'error' });
        f.checkOverwrite.mockImplementation(async () => {
            f.switchServer();
            return { action: 'overwrite', destinationExists: true };
        });
        await f.retryAllFailedItems();
        expect(f.invoke.mock.calls.filter(([command]) => command === 'provider_upload_file')).toHaveLength(1);
        expect(f.invoke.mock.calls.filter(([command]) => command === 'reconnect_ftp')).toHaveLength(0);
        expect(f.row('q0')).toMatchObject({ status: 'error', error: 'transfer.queuedForAnotherServer' });
    });
});

/**
 * The check at the start of a transfer is not enough: the overwrite question
 * stays open as long as the user wants, and a tab switch meanwhile made the
 * write that followed land on the other server. Every write checks the target
 * again, after the last await before it.
 */
describe('a switch to another server while the overwrite question is open', () => {
    const answerAfterSwitch = (f: ReturnType<typeof fixture>) => f.checkOverwrite.mockImplementation(async () => {
        f.switchServer();
        return { action: 'overwrite', destinationExists: true };
    });

    // The file being asked about fails with the reason, and the files after it
    // fail with it too, without a question of their own.
    const expectNothingWritten = (f: ReturnType<typeof fixture>, files: number) => {
        expect(f.checkOverwrite).toHaveBeenCalledTimes(1);
        expect(f.written()).toEqual([]);
        // One file runs on the single-file path, which has queued its row; a
        // batch stops before it queues any, as a failed preflight does.
        const errors = f.transferQueue.failTransfer.mock.calls.map(([, error]) => error);
        if (files === 1) expect(errors).toEqual(['transfer.queuedForAnotherServer']);
        else {
            expect(errors).toEqual([]);
            expect(f.notify.error.mock.calls.map(([, reason]) => reason)).toEqual(['transfer.queuedForAnotherServer']);
        }
    };

    describe.each([['one file', ['up.txt']], ['three files', ['up.txt', 'b.txt', 'c.txt']]] as const)('%s', (_label, names) => {
        it('uploads nothing to the server switched to', async () => {
            const f = fixture({ folders: { [LOCAL]: names.map(n => local(n)), [REMOTE]: names.map(n => remote(n)) }, policy: 'ask' });
            answerAfterSwitch(f);
            await f.drop(names.map(n => local(n)), false, REMOTE, 'local');
            expectNothingWritten(f, names.length);
        });

        it('downloads nothing from the server switched to', async () => {
            const f = fixture({ folders: { [REMOTE]: names.map(n => remote(n)), [SUB]: names.map(n => local(n, SUB)) }, policy: 'ask' });
            answerAfterSwitch(f);
            await f.drop(names.map(n => remote(n)), true, SUB);
            expectNothingWritten(f, names.length);
        });
    });

    it('commits nothing to a GitHub repository switched to, and asks nothing more', async () => {
        const names = ['up.txt', 'b.txt', 'c.txt'];
        const f = fixture({ folders: { [LOCAL]: names.map(n => local(n)), [REMOTE]: names.map(n => remote(n)) }, policy: 'ask', protocol: 'github' });
        answerAfterSwitch(f);
        await f.drop(names.map(n => local(n)), false, REMOTE, 'local');
        expect(f.checkOverwrite).toHaveBeenCalledTimes(1);
        expect(f.commits()).toEqual([]);
        expect(f.written()).toEqual([]);
        const errors = f.transferQueue.failTransfer.mock.calls.map(([, error]) => error);
        expect(errors).toHaveLength(names.length);
        expect(new Set(errors)).toEqual(new Set(['transfer.queuedForAnotherServer']));
    });

    it('passes the check to every write of the three batch functions', () => {
        for (const head of [
            'const runFileBatch = useLatestCallback(',
            'const uploadMultipleFiles = withBatchLatch(',
            'const downloadMultipleFiles = withBatchLatch(',
        ]) {
            // Up to the next closure declared at App's own level.
            const body = between(APP_CODE, head, '\n  const ');
            expect(body).toContain('queuedTargetGuard(queuedTarget);');
            const writes = body.split(/await (?:uploadFile|downloadFile)\(/).slice(1).map(call => call.slice(0, call.indexOf(';')));
            for (const call of writes) expect(call).toMatch(/'throw', undefined, onQueuedTarget(?:, expectedGuiTarget)?\)$/);
        }
        const batch = between(APP_CODE, 'const runFileBatch = useLatestCallback(', '\n  const uploadMultipleFiles =');
        expect(batch).toMatch(/onQueuedTarget\(\);\n\s+if \(isProvider\) \{\n\s+const outcome = await invoke/);
        expect(batch).toMatch(/onQueuedTarget\(\);\n\s+if \(isDownload\) \{\n\s+await invoke\('provider_download_file'/);
        // The private step checks the target too, and names what it leaves.
        expect(batch).toContain('if (queuedTargetProblem(queuedTarget)) {');
        // uploadFile and downloadFile run it inside their ownership check,
        // which every one of their writes calls first.
        for (const name of ['downloadFile', 'uploadFile']) {
            const fn = between(APP_CODE, `const ${name} = useLatestCallback(`, 'const ');
            expect(fn).toContain('onTarget?: () => void');
        }
        // Without one, they bind to the server live at the call.
        expect(APP_CODE.match(/const checkTarget = onTarget \?\? queuedTargetGuard\(snapshotQueuedTarget\(\)\);/g)).toHaveLength(2);
        expect(APP_CODE.match(/const assertOwnership = \(\) => \{\n\s+checkTarget\(\);/g)).toHaveLength(2);
    });
});

/**
 * Remote files cut or copied are their server's: the clipboard named no
 * server, so a cut on A pasted after a switch to B downloaded B's files at the
 * same paths and then deleted them from B, or moved them there.
 */
describe('remote files cut on one server and pasted after a switch to another', () => {
    const names = ['a.txt', 'b.txt'];
    const cutOnA = (f: ReturnType<typeof fixture>) => f.clipboardCut(names.map(n => ({ name: n, path: `${REMOTE}/${n}`, is_dir: false })), true, REMOTE);
    const touched = (f: ReturnType<typeof fixture>) => f.invoke.mock.calls
        .filter(([command]) => /^provider_(download|delete|rename|server_copy)|^(delete|rename)_remote_file$/.test(command));

    it.each([
        ['into a local folder', false, SUB],
        ['into another folder of the server', true, INBOX],
    ] as const)('touches none of B\'s files %s, and keeps the clipboard', async (_label, targetIsRemote, targetDir) => {
        const f = fixture({ folders: { [REMOTE]: names.map(n => remote(n)) } });
        cutOnA(f);
        f.switchServer();
        await f.clipboardPaste(targetIsRemote, targetDir);
        expect(touched(f)).toEqual([]);
        expect(f.written()).toEqual([]);
        expect(f.fileClipboardRef.current).toMatchObject({ operation: 'cut', isRemote: true });
        expect(f.notify.error).toHaveBeenCalledWith('contextMenu.paste', 'transfer.pasteFromAnotherServer');
    });

    it('requires a fresh cut after the captured target was interrupted', async () => {
        const f = fixture({ folders: { [REMOTE]: names.map(n => remote(n)) } });
        cutOnA(f);
        f.switchServer();
        await f.clipboardPaste(false, SUB);
        f.switchServer({ server: 'a.example' });
        await f.clipboardPaste(false, SUB);
        expect(f.written()).toEqual([]);
        expect(f.fileClipboardRef.current).toMatchObject({ operation: 'cut', isRemote: true });
        cutOnA(f);
        await f.clipboardPaste(false, SUB);
        expect(f.written()).toEqual(names.map(n => `${SUB}/${n}`));
        expect(f.invoke.mock.calls.filter(([command]) => command === 'provider_delete_file').map(([, args]) => args!.path))
            .toEqual(names.map(n => `${REMOTE}/${n}`));
        expect(f.fileClipboardRef.current).toBeNull();
    });

    // Greptile on #1526: a switch that stops a move midway left the files it
    // had not moved on A, and cleared the clipboard, so Paste could not finish.
    it('keeps the files a switch stopped it from moving, and moves them once their server is back', async () => {
        const f = fixture({ folders: { [REMOTE]: names.map(n => remote(n)) } });
        cutOnA(f);
        const run = f.invoke.getMockImplementation()!;
        let renames = 0;
        f.invoke.mockImplementation(async (command: string, args?: Record<string, any>) => {
            if (command === 'provider_rename') {
                renames += 1;
                // The user switches tabs while the first move is under way.
                if (renames === 1) f.switchServer();
                return undefined;
            }
            return run(command, args);
        });
        await f.clipboardPaste(true, INBOX);
        expect(renames).toBe(1);
        expect(f.fileClipboardRef.current).toMatchObject({ operation: 'cut', isRemote: true, files: [{ name: 'b.txt' }] });
        f.switchServer({ server: 'a.example' });
        await f.clipboardPaste(true, INBOX);
        expect(renames).toBe(2);
        expect(f.fileClipboardRef.current).toBeNull();
    });

    it('deletes nothing on B when the switch comes while the paste asks about overwriting', async () => {
        const f = fixture({ folders: { [REMOTE]: names.map(n => remote(n)), [SUB]: names.map(n => local(n, SUB)) }, policy: 'ask' });
        cutOnA(f);
        f.checkOverwrite.mockImplementation(async () => {
            f.switchServer();
            return { action: 'overwrite', destinationExists: true };
        });
        await f.clipboardPaste(false, SUB);
        expect(touched(f)).toEqual([]);
        expect(f.notify.warning).toHaveBeenCalledWith('toast.cutPartial');
    });
});

/**
 * The editor keeps a remote file open across a tab switch, and its save sent
 * the content to the live server: the file at the same path on another server
 * received it. The file keeps the server it was read from.
 */
describe('the editor\'s save of a remote file', () => {
    const opened = (f: ReturnType<typeof fixture>, target = f.snapshotQueuedTarget()) =>
        ({ name: 'a.txt', path: `${REMOTE}/a.txt`, content: 'old', mimeType: 'text/plain', size: 3, isRemote: true, target });
    const saves = (f: ReturnType<typeof fixture>) => f.invoke.mock.calls.filter(([command]) => command === 'save_remote_file');

    it('refuses another server, and saves once its own is live again', async () => {
        const f = fixture();
        const file = opened(f);
        f.switchServer();
        // A refusal rejects, so the editor keeps the buffer marked modified.
        await expect(f.saveDevToolsFile('new', file)).rejects.toBeInstanceOf(QueuedTargetChangedError);
        expect(saves(f)).toEqual([]);
        expect(f.notify.error).toHaveBeenCalledWith('toast.saveFailed', 'toast.saveOnAnotherServer');
        f.switchServer({ server: 'a.example' });
        await f.saveDevToolsFile('new', file);
        // Bound to the connection read just before the write, held by the
        // backend through it (Greptile on #1526).
        expect(saves(f)).toEqual([['save_remote_file', { path: `${REMOTE}/a.txt`, content: 'new', expectedGeneration: 7 }]]);
    });

    it('rejects a save the backend refused or failed, saying why once', async () => {
        const changed = fixture({ fail: (command) => (command === 'save_remote_file' ? 'CONNECTION_CHANGED: the server changed while the file was open; nothing was done' : undefined) });
        await expect(changed.saveDevToolsFile('new', opened(changed))).rejects.toBeInstanceOf(QueuedTargetChangedError);
        expect(changed.notify.error).toHaveBeenCalledTimes(1);
        expect(changed.notify.error).toHaveBeenCalledWith('toast.saveFailed', 'toast.saveOnAnotherServer');
        const failed = fixture({ fail: (command) => (command === 'save_remote_file' ? 'Permission denied' : undefined) });
        await expect(failed.saveDevToolsFile('new', opened(failed))).rejects.toBe('Permission denied');
        expect(failed.notify.error).toHaveBeenCalledWith('toast.saveFailed', 'Permission denied');
    });

    it('refuses another account on the same server', async () => {
        const f = fixture();
        const file = opened(f);
        f.switchServer({ password: 'key-2' });
        await expect(f.saveDevToolsFile('new', file)).rejects.toBeInstanceOf(QueuedTargetChangedError);
        expect(saves(f)).toEqual([]);
    });

    it('refuses a remote file that does not say which server it came from', async () => {
        const f = fixture();
        await expect(f.saveDevToolsFile('new', { ...opened(f), target: undefined })).rejects.toBeInstanceOf(QueuedTargetChangedError);
        expect(saves(f)).toEqual([]);
    });

    it('saves a local file whichever server is live', async () => {
        const f = fixture();
        f.switchServer();
        await f.saveDevToolsFile('new', { ...opened(f), path: `${LOCAL}/a.txt`, isRemote: false, target: null });
        expect(f.invoke.mock.calls.filter(([command]) => command === 'save_local_file')).toHaveLength(1);
    });

    // The editor's read, bound to the connection it starts on (the hook's own
    // `readForEditor`, with the IPC mocked).
    const reader = async (read: (path: string) => string, generation: () => number, changed: () => boolean) => {
        const source = between(PREVIEW, '  const readForEditor = useLatestCallback(', '\n  });\n') + '\n  });';
        const invoke = vi.fn(async (command: string, args?: { path: string; expectedGeneration?: number }) => {
            if (command === 'provider_connection_generation') return generation();
            if (command === 'preview_remote_file') return read(args!.path);
            throw new Error(`Unexpected IPC: ${command}`);
        });
        const target = { params: { server: 'a.example' }, profileId: 'p1', overlay: null };
        const remoteTarget = { snapshot: () => target, changedSince: changed, changedMessage: () => 'toast.openedOnAnotherServer' };
        const { code } = await transformWithOxc(source, 'read-for-editor.ts');
        const readForEditor = new Function('useLatestCallback', 'invoke', 'remoteTarget', `${code}; return readForEditor;`)(
            (fn: unknown) => fn, invoke, remoteTarget);
        return { readForEditor, invoke, target };
    };

    it('reads a remote file on the connection it was opened on, and keeps its server', async () => {
        const r = await reader(path => `content of ${path}`, () => 7, () => false);
        expect(await r.readForEditor('/srv/a.txt', true)).toEqual({ content: 'content of /srv/a.txt', target: r.target });
        expect(r.invoke).toHaveBeenCalledWith('preview_remote_file', { path: '/srv/a.txt', expectedGeneration: 7 });
    });

    it('refuses the content when the backend saw another connection', async () => {
        const r = await reader(() => { throw 'CONNECTION_CHANGED: the server changed while the file was opened; nothing was read'; }, () => 7, () => false);
        await expect(r.readForEditor('/srv/a.txt', true)).rejects.toThrow('toast.openedOnAnotherServer');
    });

    it('reads nothing when the editor opens while a switch is reconnecting the backend', async () => {
        const r = await reader(() => 'old server', () => 7, () => true);
        await expect(r.readForEditor('/srv/a.txt', true)).rejects.toThrow('toast.openedOnAnotherServer');
        expect(r.invoke).not.toHaveBeenCalledWith('preview_remote_file', expect.anything());
        expect(r.invoke).not.toHaveBeenCalledWith('provider_connection_generation');
    });

    it('reads nothing when a switch starts while the generation is taken', async () => {
        let asked = false;
        const r = await reader(() => 'old server', () => { asked = true; return 7; }, () => asked);
        await expect(r.readForEditor('/srv/a.txt', true)).rejects.toThrow('toast.openedOnAnotherServer');
        expect(r.invoke).not.toHaveBeenCalledWith('preview_remote_file', expect.anything());
    });

    it('refuses the content when the tab changed while it was read', async () => {
        let switched = false;
        const r = await reader(() => { switched = true; return 'other server'; }, () => 7, () => switched);
        await expect(r.readForEditor('/srv/a.txt', true)).rejects.toThrow('toast.openedOnAnotherServer');
    });

    it('opens both editor entry points through the bound read', () => {
        for (const opener of ['const openDevToolsPreview = useCallback(', 'const openDevToolsFromData = useCallback(']) {
            const body = between(codeOnly(PREVIEW), opener, '}, [notify]);');
            expect(body).toContain('await readForEditor(');
            expect(body).not.toContain("invoke<string>('preview_remote_file'");
            expect(body).toContain('        target,\n');
        }
        expect(APP).toContain('snapshot: () => snapshotQueuedTarget(),');
        expect(APP).toContain('onSaveFile={saveDevToolsFile}');
    });
});

/**
 * A direct remote action (a delete, a rename, a new folder, a move by drag)
 * acts on the server it was started on. Its dialog or confirmation stays open
 * as long as the user wants, and a tab switch, a reconnect or a cancelled
 * connect's teardown meanwhile moved the write to another server. Each action
 * takes its target when it starts and checks it right before every write.
 */
describe('a direct remote action', () => {
    // The GitHub and GitLab batch delete names its command through a variable.
    const WRITE = /invoke(<[^>]*>)?\('(provider_delete_file|provider_delete_dir|delete_remote_file|provider_rename|rename_remote_file|provider_mkdir|create_remote_folder|aerovault_overlay_delete_entries|aerovault_overlay_rename_entry|aerovault_overlay_create_directory)'|invoke\(deleteCommand,/;
    const body = (name: string) => {
        const start = APP_CODE.indexOf(`\n  const ${name} = `);
        expect(start, name).toBeGreaterThan(0);
        const rest = APP_CODE.slice(start + 1);
        const end = rest.slice(10).search(/\n {2}const [A-Za-z_]+ = |\n {2}\/\/ ===|\n {2}\/\*\*/);
        return rest.slice(0, end + 10);
    };

    it.each([
        ['deleteMultipleRemoteFiles', 'remoteActionGuard()'],
        // Or the target it was started for, when it runs again after the commit dialog.
        ['deleteRemoteFile', 'startedFor ?? remoteActionGuard()'],
        ['renameFile', 'remoteActionGuard()'],
        ['commitInlineRename', 'remoteActionGuard(inlineRename.target)'],
        ['handleBatchRename', 'remoteActionGuard(batchRenameDialog?.target)'],
        ['createFolder', 'remoteActionGuard()'],
        ['handleSyncNavCreateFolder', 'remoteActionGuard(syncNavDialog?.target)'],
    ])('%s checks the server it started on right before each remote write', (name, guard) => {
        const fn = body(name);
        const taken = fn.indexOf(`const onTarget = ${guard};`);
        expect(taken, `${name} takes its target`).toBeGreaterThan(0);
        expect(taken).toBeLessThan(fn.search(/await |setInputDialog\(|setConfirmDialog\(/));
        const lines = fn.split('\n');
        const writes = lines.map((line, i) => [line, i] as const).filter(([line]) => WRITE.test(line));
        expect(writes.length, `${name} writes`).toBeGreaterThan(0);
        for (const [line, i] of writes) expect(lines[i - 1].trim(), `${name}: ${line.trim()}`).toBe('onTarget();');
    });

    it('takes the target when the dialog it is answered from opens', () => {
        expect(between(APP_CODE, 'const startInlineRename =', 'const commitInlineRename =')).toContain('prepareGuiTarget(remoteActionGuard(target))');
        expect(APP_CODE).toContain('setBatchRenameDialog({ files: selectedFiles, isRemote: true, siblings, target, preparedTarget: prepareGuiTarget(remoteActionGuard(target)) });');
        expect(APP.match(/setSyncNavDialog\(\{ [^}]*isRemote: true[^}]*preparedTarget: prepareGuiTarget\(remoteActionGuard\(target\)\) \}\)/g)).toHaveLength(2);
    });

    it('moves a dropped remote file only on the server it was dropped on', () => {
        const move = between(codeOnly(DRAG), '        if (dropsIntoItself(targetPath, sourcePaths)) return;', "humanLog.logError('MOVE'");
        expect(move).toContain('const onTarget = remoteOwner?.check;');
        expect(move).toMatch(/onTarget\?\.\(\);\n\s+if \(useProviderCmd\) \{\n\s+await invoke\('provider_rename'/);
        expect(move).toContain('const expectedGuiTarget = remoteOwner?.epoch;');
        expect(DRAG).toContain('remoteOwner = await resolveRemoteDragTarget(dragData)');
        expect(APP).toContain('remoteActionGuard: () => remoteActionGuard(),');
    });

    it('refuses once the server changed, a switch reconnects or a teardown runs', async () => {
        const source = between(APP, '  const liveOverlay = useLatestCallback(', '\n  const failQueuedRestIfRetargeted =');
        const session = { id: 'A', connectionParams: { server: 'a.example', protocol: 'sftp', username: 'u', password: 'k' }, cryptOverlay: null };
        const ctx = {
            useLatestCallback: (fn: unknown) => fn, syncCancelPendingRef: { current: null }, syncRunnerPendingRef: { current: null }, stopCountRef: { current: 0 }, isConnected: true,
            remoteConnectPhaseRef: { current: false as false | 'switch' }, backendTeardownsRef: { current: 0 }, backendUncertainRef: { current: false }, backendConfirmationsRef: { current: 0 },
            sessions: [session], activeSessionId: 'A', liveConnectionParams: session.connectionParams,
            isSameConnection, savedProfileIdOf, isOAuthProvider, isFourSharedProvider, connectionNumberOf, ACCOUNT_KEY_OPTIONS, oauthLoginCount: (key: string) => oauthLogins.get(key) ?? 0, oauthLoginKey, transferQueue: { failTransfer: vi.fn() }, t: (key: string) => key,
            QueuedTargetChangedError: class extends Error {},
        };
        const { code } = await transformWithOxc(source, 'action-guard.ts');
        const { remoteActionGuard } = new Function(...Object.keys(ctx), `${code}; return { remoteActionGuard };`)(...Object.values(ctx));
        const onTarget = remoteActionGuard();
        expect(() => onTarget()).not.toThrow();
        for (const [ref, value] of [[ctx.remoteConnectPhaseRef, 'switch'], [ctx.backendTeardownsRef, 1], [ctx.backendUncertainRef, true]] as const) {
            const previous = ref.current;
            (ref as { current: unknown }).current = value;
            expect(() => onTarget()).toThrow('toast.remoteTargetChanged');
            (ref as { current: unknown }).current = previous;
        }
        session.connectionParams.server = 'b.example';
        expect(() => onTarget()).toThrow('toast.remoteTargetChanged');
    });
});

/**
 * Greptile on #1526: OpenDrive and 4shared make a new upload public unless
 * told otherwise, and the batch sets the landed files private on the server
 * it was queued for. After a tab switch that step is not run on the other
 * server, and it used to be dropped in silence: the files left as the
 * provider made them are now named.
 */
describe('the private step after a batch upload', () => {
    it.each(['opendrive', 'fourshared'] as const)('names the %s files it could not set private after a switch', async (protocol) => {
        const f = fixture({ protocol, folders: { [LOCAL]: [local('one.txt'), local('two.txt')] } });
        const run = f.invoke.getMockImplementation()!;
        f.invoke.mockImplementation(async (command: string, args?: Record<string, any>) => {
            const answer = await run(command, args);
            // The files land, then the user switches tabs before the private step.
            if (command === 'provider_upload_files_batch') f.switchServer();
            return answer;
        });
        const items = ['one.txt', 'two.txt'].map(name => ({ name, sourcePath: `${LOCAL}/${name}`, size: 1, modified: null }));
        const runFileBatch = f.targetChecks.runFileBatch as (direction: string, entries: typeof items, dir: string) => Promise<unknown>;
        await runFileBatch('upload', items, REMOTE);
        expect(f.invoke.mock.calls.filter(([command]) => /set_path_privacy$/.test(command))).toEqual([]);
        expect(f.notify.error).toHaveBeenCalledWith('activity.set_private_error', 'one.txt, two.txt');
    });
});

/**
 * CodeRabbit on #1526: a GitHub or GitLab delete without a commit message
 * opens the commit dialog, and its Commit runs the delete again. That second
 * run took a fresh target, the server live after the dialog, so a switch
 * while the dialog was open deleted the file at the same path there. It keeps
 * the target the delete was started for. App's own `deleteRemoteFile` runs
 * here, with the IPC and the dialogs mocked.
 */
describe('a delete that asks for a commit message', () => {
    const run = async () => {
        const source = between(APP, '  const captureGuiTarget =', '\n  const getOverwriteDestination =')
            + between(APP, '  const deleteRemoteFile = (', '\n  const renameFile = (');
        const { code } = await transformWithOxc(source, 'delete.ts');
        const live = { server: 'a.example' };
        // The guard of a direct action: bound to the server live when it is taken.
        const remoteActionGuard = () => {
            const at = live.server;
            return () => { if (live.server !== at) throw new QueuedTargetChangedError('toast.remoteTargetChanged'); };
        };
        const invoke = vi.fn(async (command: string) => command === 'provider_gui_target_epoch' ? 7 : undefined);
        const dialog: { onCommit?: (message: string) => void } = {};
        const notify = { success: vi.fn(), error: vi.fn() };
        const context: Record<string, unknown> = {
            remoteActionGuard, invoke, notify, t: (key: string) => key,
            liveConnectionParams: { protocol: 'github' },
            gitHubRepoInfo: { branch: 'main', writeModeKind: 'direct', workingBranch: null },
            setGitHubCommitDialog: (value: { onCommit: (message: string) => void } | null) => { if (value) dialog.onCommit = value.onCommit; },
            humanLog: { logStart: () => 'log', logSuccess: vi.fn(), logError: vi.fn() },
            usesProviderApi: () => true, aeroVaultOverlaySession: null, confirmBeforeDelete: false,
            setConfirmDialog: vi.fn(), loadRemoteFiles: vi.fn(async () => undefined),
        };
        const deleteRemoteFile = new Function(...Object.keys(context), `${code}; return deleteRemoteFile;`)(...Object.values(context));
        return { deleteRemoteFile, live, invoke, dialog, notify };
    };
    const flush = () => new Promise(resolve => setTimeout(resolve, 0));

    it('deletes nothing on the server switched to while the dialog was open', async () => {
        const r = await run();
        r.deleteRemoteFile('/repo/a.txt', false);
        expect(r.dialog.onCommit).toBeDefined();
        r.live.server = 'b.example';
        r.dialog.onCommit!('sentinel message');
        await flush();
        expect(r.invoke.mock.calls.filter(([name]) => name === 'provider_delete_file')).toHaveLength(0);
        expect(r.notify.error).toHaveBeenCalled();
    });

    it('deletes on the server it was started for when no switch came', async () => {
        const r = await run();
        r.deleteRemoteFile('/repo/a.txt', false);
        r.dialog.onCommit!('sentinel message');
        await flush();
        expect(r.invoke).toHaveBeenCalledWith('provider_delete_file', { path: '/repo/a.txt', commitMessage: 'sentinel message', expectedGuiTarget: 7 });
    });
});
