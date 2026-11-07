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
import { INBOX, LOCAL, REMOTE, SUB, fixture, local, remote } from '../components/appTransfers.testkit';
import { isSameConnection, savedProfileIdOf } from './liveConnection';

/**
 * Requested 184: an action landed on the wrong target. "Apply" on a code block
 * wrote a remote file's path through the local filesystem (deep scan item 9),
 * and a queued upload started or retried after a tab switch went to the server
 * of the new tab (item 6).
 */

const between = (source: string, start: string, end: string) => {
    const a = source.indexOf(start);
    const b = source.indexOf(end, a + start.length);
    if (a < 0 || b < 0) throw new Error(`Missing block: ${start}`);
    return source.slice(a, b);
};

describe('Apply on a remote file', () => {
    it('goes into the editor buffer, never through the local filesystem', () => {
        const apply = between(CODE_BLOCK, 'const handleApplyToFile = useCallback(', '}, [code, language');
        const remote = between(apply, 'if (editorFilePath && editorFileIsRemote) {', '} else if (editorFilePath) {');
        expect(remote).toContain('applyToEditorBuffer({ path: editorFilePath, isRemote: true }, code)');
        expect(remote).not.toContain('writeTextFile');
        // Without the editor showing the file the action says so (CodeBlockActions.test.tsx).
        expect(remote).toContain("showUnreachable('apply')");
        const diff = between(CODE_BLOCK, 'const handleDiff = useCallback(', '}, [editorFilePath, editorFileIsRemote, showDiff, showUnreachable]);');
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
        const apply = between(CODE_EDITOR, 'const handleApply = (e: Event) => {', 'detail.accept();');
        expect(apply).toContain("editor.executeEdits('aeroagent-apply'");
        expect(apply).toContain('editor.pushUndoStop();');
        expect(apply).not.toContain('setValue(');
    });
});

describe('a queued transfer', () => {
    it('is bound to its server on every Start and Retry the user queues', () => {
        const registrations = APP.split('retryCallbacksRef.current.set(').slice(1).map(rest => rest.slice(0, 120));
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
            const body = APP.slice(APP.indexOf(head));
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
            useLatestCallback: (fn: unknown) => fn,
            isConnected: true,
            remoteConnectPhaseRef: { current: false },
            sessions: [session],
            activeSessionId: 'A',
            liveConnectionParams: session.connectionParams,
            isSameConnection,
            savedProfileIdOf,
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
            useLatestCallback: (fn: unknown) => fn, isConnected: true, remoteConnectPhaseRef: { current: false as false | 'switch' },
            sessions: [session], activeSessionId: 'A', liveConnectionParams: session.connectionParams,
            isSameConnection, savedProfileIdOf, transferQueue: { failTransfer: vi.fn() }, t: (key: string) => key,
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
        for (const [head, end] of [
            ['const runFileBatch = useLatestCallback(', '\n  const uploadMultipleFiles ='],
            ['const uploadMultipleFiles = withBatchLatch(', '\n  // === Bulk Operations ==='],
            ['const downloadMultipleFiles = withBatchLatch(', '\n  // Wire cross-panel drag & drop callback'],
        ]) {
            const body = between(APP, head, end);
            expect(body).toContain('const onQueuedTarget = queuedTargetGuard(queuedTarget);');
            const writes = body.split(/await (?:uploadFile|downloadFile)\(/).slice(1).map(call => call.slice(0, call.indexOf(';')));
            for (const call of writes) expect(call).toMatch(/'throw', undefined, onQueuedTarget\)$/);
        }
        const batch = between(APP, 'const runFileBatch = useLatestCallback(', '\n  const uploadMultipleFiles =');
        expect(batch).toMatch(/onQueuedTarget\(\);\n\s+if \(isProvider\) \{\n\s+const outcome = await invoke/);
        expect(batch).toMatch(/onQueuedTarget\(\);\n\s+if \(isDownload\) \{\n\s+await invoke\('provider_download_file'/);
        expect(batch).toContain('if (queuedTargetProblem(queuedTarget)) return;');
        // uploadFile and downloadFile run it inside their ownership check,
        // which every one of their writes calls first.
        for (const name of ['downloadFile', 'uploadFile']) {
            const fn = between(APP, `const ${name} = useLatestCallback(`, 'const ');
            expect(fn).toContain('onTarget?: () => void');
        }
        // Without one, they bind to the server live at the call.
        expect(APP.match(/const checkTarget = onTarget \?\? queuedTargetGuard\(snapshotQueuedTarget\(\)\);/g)).toHaveLength(2);
        expect(APP.match(/const assertOwnership = \(\) => \{\n\s+checkTarget\(\);/g)).toHaveLength(2);
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

    it('moves them once their server is live again', async () => {
        const f = fixture({ folders: { [REMOTE]: names.map(n => remote(n)) } });
        cutOnA(f);
        f.switchServer();
        await f.clipboardPaste(false, SUB);
        f.switchServer({ server: 'a.example' });
        await f.clipboardPaste(false, SUB);
        expect(f.written()).toEqual(names.map(n => `${SUB}/${n}`));
        expect(f.invoke.mock.calls.filter(([command]) => command === 'provider_delete_file').map(([, args]) => args!.path))
            .toEqual(names.map(n => `${REMOTE}/${n}`));
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
        await f.saveDevToolsFile('new', file);
        expect(saves(f)).toEqual([]);
        expect(f.notify.error).toHaveBeenCalledWith('toast.saveFailed', 'toast.saveOnAnotherServer');
        f.switchServer({ server: 'a.example' });
        await f.saveDevToolsFile('new', file);
        expect(saves(f)).toEqual([['save_remote_file', { path: `${REMOTE}/a.txt`, content: 'new' }]]);
    });

    it('refuses another account on the same server', async () => {
        const f = fixture();
        const file = opened(f);
        f.switchServer({ password: 'key-2' });
        await f.saveDevToolsFile('new', file);
        expect(saves(f)).toEqual([]);
    });

    it('refuses a remote file that does not say which server it came from', async () => {
        const f = fixture();
        await f.saveDevToolsFile('new', { ...opened(f), target: undefined });
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
            const body = between(PREVIEW, opener, '}, [notify]);');
            expect(body).toContain('await readForEditor(');
            expect(body).not.toContain("invoke<string>('preview_remote_file'");
            expect(body).toContain('        target,\n');
        }
        expect(APP).toContain('snapshot: () => snapshotQueuedTarget(),');
        expect(APP).toContain('onSaveFile={saveDevToolsFile}');
    });
});
