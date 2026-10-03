// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { SyncRunFile } from './remoteSyncRunner';
import { syncRunQueueBridge, type SyncRunQueue } from './syncRunQueueBridge';
import APP from '../App.tsx?raw';

const fakeQueue = () => {
    const calls: string[] = [];
    let next = 0;
    const queue: SyncRunQueue = {
        addItem: (filename, path, size, type) => {
            const id = `q${++next}`;
            calls.push(`add ${id} ${filename} ${path} ${size} ${type}`);
            return id;
        },
        startTransfer: (id) => { calls.push(`start ${id}`); },
        completeTransfer: (id) => { calls.push(`complete ${id}`); },
        failTransfer: (id, error) => { calls.push(`fail ${id} ${error}`); },
    };
    return { queue, calls };
};

const file = (
    relativePath: string,
    action: SyncRunFile['action'],
    over: Partial<SyncRunFile> = {},
): SyncRunFile => ({ relativePath, action, size: 2048, mtime: null, ...over });

describe('syncRunQueueBridge', () => {
    it('adds and starts a row named after the file when it starts syncing', () => {
        const { queue, calls } = fakeQueue();
        const onFileStatus = syncRunQueueBridge(
            [file('a/b.txt', 'upload'), file('c.bin', 'download', { size: 7 })],
            queue,
        );
        onFileStatus('a/b.txt', 'syncing');
        onFileStatus('c.bin', 'syncing');
        expect(calls).toEqual([
            'add q1 b.txt a/b.txt 2048 upload',
            'start q1',
            'add q2 c.bin c.bin 7 download',
            'start q2',
        ]);
    });

    it('completes the row on success or skip and fails it with the message on error or a failed verify', () => {
        const { queue, calls } = fakeQueue();
        const paths = ['ok.txt', 'kept.txt', 'bad.txt', 'short.txt'];
        const onFileStatus = syncRunQueueBridge(paths.map((p) => file(p, 'download')), queue);
        for (const path of paths) onFileStatus(path, 'syncing');
        calls.length = 0;
        onFileStatus('ok.txt', 'success');
        onFileStatus('kept.txt', 'skipped');
        onFileStatus('bad.txt', 'error', 'permission denied');
        onFileStatus('short.txt', 'verify_failed', 'short.txt: size mismatch');
        expect(calls).toEqual([
            'complete q1',
            'complete q2',
            'fail q3 permission denied',
            'fail q4 short.txt: size mismatch',
        ]);
    });

    it('keeps one row through a retry and the verify step', () => {
        const { queue, calls } = fakeQueue();
        const onFileStatus = syncRunQueueBridge([file('flaky.txt', 'download')], queue);
        onFileStatus('flaky.txt', 'syncing');
        onFileStatus('flaky.txt', 'retrying');
        onFileStatus('flaky.txt', 'verifying');
        onFileStatus('flaky.txt', 'success');
        expect(calls).toEqual([
            'add q1 flaky.txt flaky.txt 2048 download',
            'start q1',
            'start q1',
            'complete q1',
        ]);
    });

    it('puts a failed attempt back in progress when the runner retries it', () => {
        // The runner reports `retrying` and runs the transfer again with no
        // second `syncing`; the backend's error event for the failed attempt
        // has already turned the row red, and a red row does not take the
        // retry's start and progress events.
        const { queue, calls } = fakeQueue();
        const onFileStatus = syncRunQueueBridge([file('flaky.txt', 'upload')], queue);
        onFileStatus('flaky.txt', 'syncing');
        onFileStatus('flaky.txt', 'retrying');
        onFileStatus('flaky.txt', 'success');
        expect(calls).toEqual([
            'add q1 flaky.txt flaky.txt 2048 upload',
            'start q1',
            'start q1',
            'complete q1',
        ]);
    });

    it('adds nothing for deletes, folders the run creates, or files a cancel skipped before they started', () => {
        const { queue, calls } = fakeQueue();
        const onFileStatus = syncRunQueueBridge(
            [file('old.txt', 'delete-remote'), file('gone', 'delete-local', { isDir: true }), file('later.txt', 'upload')],
            queue,
        );
        onFileStatus('old.txt', 'syncing');
        onFileStatus('old.txt', 'success');
        onFileStatus('gone', 'syncing');
        onFileStatus('gone', 'skipped');
        onFileStatus('new-dir', 'success');
        onFileStatus('later.txt', 'skipped');
        expect(calls).toEqual([]);
    });

    it('names a keep-both copy after its source file, the name the backend progress events carry', () => {
        const { queue, calls } = fakeQueue();
        const onFileStatus = syncRunQueueBridge(
            [file('docs/report (keep).txt', 'upload', { sourcePath: 'docs/report.txt' })],
            queue,
        );
        onFileStatus('docs/report (keep).txt', 'syncing');
        expect(calls[0]).toBe('add q1 report.txt docs/report (keep).txt 2048 upload');
    });
});

describe('AeroSync run wiring (#364)', () => {
    it('feeds the Transfer Queue from both sync launchers', () => {
        // runConnectedRemoteSync and runLocalLocalSync make one runner call
        // each; a call without onFileStatus runs with an empty queue panel.
        const calls = APP.split('await runRemoteSync(')
            .slice(1)
            .map((rest) => rest.slice(0, rest.indexOf(');')));
        expect(calls).toHaveLength(2);
        for (const call of calls) {
            expect(call).toContain('onFileStatus: syncRunQueueBridge(runFiles, transferQueue)');
        }
    });
});
