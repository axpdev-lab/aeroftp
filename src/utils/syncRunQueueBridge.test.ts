// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { SyncRunFile } from './remoteSyncRunner';
import type { SyncJournal } from '../types';
import { syncRunQueueBridge, type SyncRunQueue } from './syncRunQueueBridge';
import APP from '../App.tsx?raw';

const fakeQueue = () => {
    const calls: string[] = [];
    let next = 0;
    const queue: SyncRunQueue = {
        addItems: (items) => {
            const ids = items.map(() => `q${++next}`);
            calls.push(`plan ${ids.join(',')} ${items.map(item => `${item.filename} ${item.path} ${item.size} ${item.type} ${item.options?.plannedSync}`).join('|')}`);
            return ids;
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
    it('registers the whole planned transfer denominator before the first file starts', () => {
        const { queue, calls } = fakeQueue();
        const onFileStatus = syncRunQueueBridge(
            [file('a/b.txt', 'upload'), file('c.bin', 'download', { size: 7 })],
            queue,
        );
        expect(calls).toEqual([
            'plan q1,q2 b.txt a/b.txt 2048 upload true|c.bin c.bin 7 download true',
        ]);
        onFileStatus('a/b.txt', 'syncing');
        onFileStatus('c.bin', 'syncing');
        expect(calls.slice(1)).toEqual(['start q1', 'start q2']);
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
            'plan q1 flaky.txt flaky.txt 2048 download true',
            'start q1',
            'start q1',
            'complete q1',
        ]);
    });

    it('puts a failed attempt back in progress when the runner retries it', () => {
        const { queue, calls } = fakeQueue();
        const onFileStatus = syncRunQueueBridge([file('flaky.txt', 'upload')], queue);
        onFileStatus('flaky.txt', 'syncing');
        onFileStatus('flaky.txt', 'retrying');
        onFileStatus('flaky.txt', 'success');
        expect(calls).toEqual([
            'plan q1 flaky.txt flaky.txt 2048 upload true',
            'start q1',
            'start q1',
            'complete q1',
        ]);
    });

    it('plans only real transfers and settles a file skipped before it started', () => {
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
        expect(calls).toEqual([
            'plan q1 later.txt later.txt 2048 upload true',
            'complete q1',
        ]);
    });

    it('names a keep-both copy after its source file, the name the backend progress events carry', () => {
        const { queue, calls } = fakeQueue();
        syncRunQueueBridge(
            [file('docs/report (keep).txt', 'upload', { sourcePath: 'docs/report.txt' })],
            queue,
        );
        expect(calls[0]).toBe('plan q1 report.txt docs/report (keep).txt 2048 upload true');
    });
    it('settles entries a resumed journal already completed without a new event', () => {
        const { queue, calls } = fakeQueue();
        const resumeJournal = {
            entries: [
                { relative_path: 'done.txt', action: 'upload', status: 'completed' },
                { relative_path: 'later.txt', action: 'upload', status: 'pending' },
            ],
        } as SyncJournal;
        syncRunQueueBridge([file('done.txt', 'upload'), file('later.txt', 'upload')], queue, resumeJournal);
        expect(calls).toEqual([
            'plan q1,q2 done.txt done.txt 2048 upload true|later.txt later.txt 2048 upload true',
            'complete q1',
        ]);
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
            expect(call).toContain('onFileStatus: syncRunQueueBridge(runFiles, transferQueue, opts.resumeJournal)');
        }
    });
});
