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
        expect(remote).toContain('throw new Error');
        const diff = between(CODE_BLOCK, 'const handleDiff = useCallback(', '}, [editorFilePath, editorFileIsRemote, showDiff]);');
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
    });
});

describe('a queued transfer', () => {
    it('is bound to its server on every Start and Retry the user queues', () => {
        const registrations = APP.split('retryCallbacksRef.current.set(').slice(1).map(rest => rest.slice(0, 120));
        // The restored-journal retry has its own profile ownership check.
        const restored = registrations.filter(r => r.startsWith('id, () => retryRestoredEntry('));
        const bound = registrations.filter(r => r.startsWith('id, boundToQueuedServer(id, '));
        expect(restored).toHaveLength(1);
        expect(bound).toHaveLength(5);
        expect(registrations).toHaveLength(restored.length + bound.length);
    });

    it.each([
        ['fails without running when another server is live', false, true],
        ['runs when its server is live again', true, false],
    ])('%s', async (_name, same, fails) => {
        const source = between(APP, '  const boundToQueuedServer = useLatestCallback(', '\n  });\n') + '\n  });';
        const run = vi.fn();
        const ctx = {
            useLatestCallback: (fn: unknown) => fn,
            liveConnectionParams: { server: 'a.example', protocol: 'sftp', username: 'u' },
            liveServerIs: vi.fn(() => same),
            transferQueue: { failTransfer: vi.fn() },
            t: (key: string) => key,
        };
        const { code } = await transformWithOxc(source, 'bound.ts');
        const bind = new Function(...Object.keys(ctx), `${code}; return boundToQueuedServer;`)(...Object.values(ctx));
        await bind('item-1', run)();
        expect(ctx.liveServerIs).toHaveBeenCalledWith({ server: 'a.example', protocol: 'sftp', username: 'u' });
        expect(run).toHaveBeenCalledTimes(fails ? 0 : 1);
        if (fails) expect(ctx.transferQueue.failTransfer).toHaveBeenCalledWith('item-1', 'transfer.queuedForAnotherServer');
        else expect(ctx.transferQueue.failTransfer).not.toHaveBeenCalled();
    });
});
