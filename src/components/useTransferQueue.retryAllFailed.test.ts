// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement, useEffect, useLayoutEffect } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useTransferQueue } from './TransferQueue';

let root: Root | null = null;
let host: HTMLDivElement | null = null;

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});

afterEach(async () => {
    await act(async () => root?.unmount());
    host?.remove();
    root = null;
    host = null;
});

describe('useTransferQueue retryAllFailed (#139)', () => {
    it.each(['single', 'all'] as const)('runs the actual queued executor on %s Retry before React commits', async mode => {
        const { transformWithOxc } = await import('vite'); const { default: app } = await import('../App.tsx?raw');
        const start = app.indexOf('  const boundToQueuedServer =');
        const end = app.indexOf('\n  /** Whether two connections', start);
        const { code } = await transformWithOxc(app.slice(start, end), 'synchronous-retry-executor.ts');
        let queue!: ReturnType<typeof useTransferQueue>;
        const Probe = () => { queue = useTransferQueue(); return null; };
        await act(async () => { root!.render(createElement(Probe)); });
        let id = '';
        await act(async () => { id = queue.addItem('file.txt', '/file.txt', 100, 'upload'); queue.failTransfer(id, 'Timeout'); });
        const beforeRetry = queue;
        const context = { useLatestCallback: (fn: unknown) => fn, syncCancelPendingRef: { current: null },
            syncRunnerPendingRef: { current: null }, stopCountRef: { current: 0 }, queuedTargetProblem: () => null,
            transferQueue: beforeRetry, t: (key: string) => key };
        const bind = new Function(...Object.keys(context), `${code};return boundToQueuedServer;`)(...Object.values(context));
        const executor = vi.fn();
        await act(async () => {
            if (mode === 'single') beforeRetry.retryItem(id); else expect(beforeRetry.retryAllFailed()).toEqual([id]);
            expect(beforeRetry.items.find(item => item.id === id)?.status).toBe('error');
            expect(beforeRetry.getItemNow(id)?.status).toBe('pending');
            const executing = bind({}, id, executor)();
            expect(executor).toHaveBeenCalledOnce();
            await executing;
        });
    });

    // A staged Start waits for a sync run to end; a Stop during that wait
    // settles the row as cancelled instead of leaving it pending.
    it('cancels a staged row stopped while it waited for a sync run', async () => {
        const { transformWithOxc } = await import('vite'); const { default: app } = await import('../App.tsx?raw');
        const start = app.indexOf('  const boundToQueuedServer =');
        const end = app.indexOf('\n  /** Whether two connections', start);
        const { code } = await transformWithOxc(app.slice(start, end), 'stopped-while-waiting.ts');
        let queue!: ReturnType<typeof useTransferQueue>;
        const Probe = () => { queue = useTransferQueue(); return null; };
        await act(async () => { root!.render(createElement(Probe)); });
        let id = '';
        await act(async () => { id = queue.addItem('file.txt', '/file.txt', 100, 'upload', { staged: true }); });
        let endRun!: () => void;
        const syncRun = new Promise<void>(resolve => { endRun = resolve; });
        const stopCountRef = { current: 0 };
        const context = { useLatestCallback: (fn: unknown) => fn, syncCancelPendingRef: { current: null },
            syncRunnerPendingRef: { current: syncRun as Promise<void> | null }, stopCountRef, queuedTargetProblem: () => null,
            transferQueue: queue, t: (key: string) => key };
        const bind = new Function(...Object.keys(context), `${code};return boundToQueuedServer;`)(...Object.values(context));
        const executor = vi.fn();
        await act(async () => {
            queue.startStaged(id);
            const waiting = bind({}, id, executor)();
            stopCountRef.current++;
            context.syncRunnerPendingRef.current = null;
            endRun();
            await waiting;
        });
        expect(executor).not.toHaveBeenCalled();
        const row = queue.getItemNow(id);
        expect(row?.status).not.toBe('pending');
        expect(row?.cancelled).toBeTruthy();
    });

    // Resume All of a restored row while a sync run holds the session, then
    // Stop: the restored retry settles its row as cancelled too.
    it('cancels a restored row whose resume was stopped while a sync run held it', async () => {
        const { transformWithOxc } = await import('vite'); const { default: app } = await import('../App.tsx?raw');
        const start = app.indexOf('  const retryRestoredEntry = useLatestCallback(');
        const end = app.indexOf('\n\n  // Queues the restorable entries of the journal read at startup.', start);
        const { code } = await transformWithOxc(app.slice(start, end), 'restored-stopped-while-waiting.ts');
        let queue!: ReturnType<typeof useTransferQueue>;
        const Probe = () => { queue = useTransferQueue(); return null; };
        await act(async () => { root!.render(createElement(Probe)); });
        let id = '';
        await act(async () => { id = queue.addItem('file.txt', '/file.txt', 100, 'download'); });
        let endRun!: () => void;
        const syncRun = new Promise<void>(resolve => { endRun = resolve; });
        const stopCountRef = { current: 0 };
        const downloadFile = vi.fn();
        const context = { useLatestCallback: (fn: unknown) => fn, syncCancelPendingRef: { current: null },
            syncRunnerPendingRef: { current: syncRun as Promise<void> | null }, stopCountRef,
            transferQueue: queue, t: (key: string) => key, downloadFile, uploadFile: vi.fn() };
        const retry = new Function(...Object.keys(context), `${code};return retryRestoredEntry;`)(...Object.values(context));
        await act(async () => {
            const waiting = retry(id, { direction: 'download', remote_path: '/file.txt', local_path: '/tmp/file.txt' });
            stopCountRef.current++;
            context.syncRunnerPendingRef.current = null;
            endRun();
            await waiting;
        });
        expect(downloadFile).not.toHaveBeenCalled();
        const row = queue.getItemNow(id);
        expect(row?.status).not.toBe('pending');
        expect(row?.cancelled).toBeTruthy();
    });

    it('forgets a removed staged Start synchronously even through an older hook result', async () => {
        let queue!: ReturnType<typeof useTransferQueue>;
        const Probe = () => { queue = useTransferQueue(); return null; };
        await act(async () => { root!.render(createElement(Probe)); });
        let id = '';
        await act(async () => { id = queue.addItem('file.txt', '/file.txt', 100, 'upload', { staged: true }); });
        const staged = queue;
        await act(async () => {
            staged.startStaged(id);
            expect(staged.getItemNow(id)?.status).toBe('pending');
            staged.removeItem(id);
            expect(staged.getItemNow(id)).toBeUndefined();
        });
    });

    it('exposes Retry activity synchronously before React publishes the next render', async () => {
        let queue!: ReturnType<typeof useTransferQueue>;
        const Probe = () => { queue = useTransferQueue(); return null; };
        await act(async () => { root!.render(createElement(Probe)); });
        let id = '';
        await act(async () => { id = queue.addItem('file.txt', '/file.txt', 100, 'upload'); queue.failTransfer(id, 'Timeout'); });
        const beforeRetry = queue;
        expect(beforeRetry.hasActiveTransfers).toBe(false);
        await act(async () => {
            beforeRetry.retryItem(id);
            expect(beforeRetry.hasActiveTransfers).toBe(false);
            expect(beforeRetry.hasActiveTransfersNow()).toBe(true);
            beforeRetry.stopAll();
            expect(beforeRetry.hasActiveTransfersNow()).toBe(false);
        });
    });

    it('returns failed IDs synchronously and transitions rows to pending', async () => {
        let queue!: ReturnType<typeof useTransferQueue>;
        const Probe = () => {
            queue = useTransferQueue();
            return null;
        };

        await act(async () => {
            root!.render(createElement(Probe));
        });

        let id1 = '';
        let id2 = '';
        let id3 = '';

        await act(async () => {
            id1 = queue.addItem('file1.txt', '/file1.txt', 100, 'upload');
            id2 = queue.addItem('file2.txt', '/file2.txt', 200, 'upload');
            id3 = queue.addItem('file3.txt', '/file3.txt', 300, 'upload');
        });

        await act(async () => {
            queue.failTransfer(id1, 'Network timeout');
            queue.completeTransfer(id2);
            queue.failTransfer(id3, 'Server 500');
        });

        expect(queue.items.find(i => i.id === id1)?.status).toBe('error');
        expect(queue.items.find(i => i.id === id2)?.status).toBe('completed');
        expect(queue.items.find(i => i.id === id3)?.status).toBe('error');

        let returnedIds: string[] = [];
        await act(async () => {
            // Snapshot immediately at call time to verify synchronous return and prevent
            // a mutated reference from passing by being populated later inside an updater.
            returnedIds = [...queue.retryAllFailed()];
        });

        // Must return synchronous IDs matching the failed non-cancelled rows
        expect(returnedIds).toEqual([id1, id3]);

        // State update must transition those rows to pending with error cleared
        expect(queue.items.find(i => i.id === id1)?.status).toBe('pending');
        expect(queue.items.find(i => i.id === id1)?.error).toBeUndefined();
        expect(queue.items.find(i => i.id === id2)?.status).toBe('completed');
        expect(queue.items.find(i => i.id === id3)?.status).toBe('pending');
        expect(queue.items.find(i => i.id === id3)?.error).toBeUndefined();
    });

    it('returns empty array synchronously when no rows failed', async () => {
        let queue!: ReturnType<typeof useTransferQueue>;
        const Probe = () => {
            queue = useTransferQueue();
            return null;
        };

        await act(async () => {
            root!.render(createElement(Probe));
        });

        await act(async () => {
            const id = queue.addItem('file.txt', '/file.txt', 100, 'upload');
            queue.completeTransfer(id);
        });

        let returnedIds: string[] = ['sentinel'];
        await act(async () => {
            returnedIds = [...queue.retryAllFailed()];
        });

        expect(returnedIds).toEqual([]);
    });

    it('excludes cancelled items from retryAllFailed', async () => {
        let queue!: ReturnType<typeof useTransferQueue>;
        const Probe = () => {
            queue = useTransferQueue();
            return null;
        };

        await act(async () => {
            root!.render(createElement(Probe));
        });

        let id1 = '';
        let id2 = '';

        await act(async () => {
            id1 = queue.addItem('stopped.txt', '/stopped.txt', 100, 'upload');
            id2 = queue.addItem('failed.txt', '/failed.txt', 200, 'upload');
        });

        await act(async () => {
            queue.cancelTransfer(id1, 'Stopped by user');
            queue.failTransfer(id2, 'Connection reset');
        });

        let returnedIds: string[] = [];
        await act(async () => {
            // Snapshot immediately at call time to verify synchronous return and prevent
            // a mutated reference from passing by being populated later inside an updater.
            returnedIds = [...queue.retryAllFailed()];
        });

        expect(returnedIds).toEqual([id2]);
        expect(queue.items.find(i => i.id === id1)?.status).toBe('error');
        expect(queue.items.find(i => i.id === id1)?.cancelled).toBe(true);
        expect(queue.items.find(i => i.id === id2)?.status).toBe('pending');
    });

    it('returns failed IDs when failTransfer and retryAllFailed are called in the same batch', async () => {
        let queue!: ReturnType<typeof useTransferQueue>;
        const Probe = () => {
            queue = useTransferQueue();
            return null;
        };

        await act(async () => {
            root!.render(createElement(Probe));
        });

        let id1 = '';
        await act(async () => {
            id1 = queue.addItem('file1.txt', '/file1.txt', 100, 'upload');
        });

        let returnedIds: string[] = [];
        await act(async () => {
            // failTransfer and retryAllFailed executed in the same synchronous act tick
            queue.failTransfer(id1, 'Network dropped');
            returnedIds = [...queue.retryAllFailed()];
        });

        expect(returnedIds).toEqual([id1]);
        expect(queue.items.find(i => i.id === id1)?.status).toBe('pending');
    });

    it('excludes removed items when removeItem and retryAllFailed are called in the same batch', async () => {
        let queue!: ReturnType<typeof useTransferQueue>;
        const Probe = () => {
            queue = useTransferQueue();
            return null;
        };

        await act(async () => {
            root!.render(createElement(Probe));
        });

        let id1 = '';
        let id2 = '';
        await act(async () => {
            id1 = queue.addItem('file1.txt', '/file1.txt', 100, 'upload');
            id2 = queue.addItem('file2.txt', '/file2.txt', 200, 'upload');
        });

        await act(async () => {
            queue.failTransfer(id1, 'Error 1');
            queue.failTransfer(id2, 'Error 2');
        });

        let returnedIds: string[] = [];
        await act(async () => {
            // removeItem and retryAllFailed in the same synchronous act tick
            queue.removeItem(id1);
            returnedIds = [...queue.retryAllFailed()];
        });

        expect(returnedIds).toEqual([id2]);
        expect(queue.items.find(i => i.id === id1)).toBeUndefined();
        expect(queue.items.find(i => i.id === id2)?.status).toBe('pending');
    });

    it('preserves updates occurring between commit and passive effects without lost updates', async () => {
        let queue!: ReturnType<typeof useTransferQueue>;
        let armed = false;
        let m1 = false;
        let m2 = false;
        let idA = '';
        let idB = '';

        const Probe = () => {
            queue = useTransferQueue();
            const aTransferring = queue.items.find(i => i.id === idA)?.status === 'transferring';

            useLayoutEffect(() => {
                if (armed && aTransferring && !m1) {
                    m1 = true;
                    queue.failTransfer(idB, 'M1');
                }
            });

            useEffect(() => {
                if (armed && m1 && !m2) {
                    m2 = true;
                    queue.failTransfer(idA, 'M2');
                }
            });

            return null;
        };

        await act(async () => {
            root!.render(createElement(Probe));
        });

        await act(async () => {
            idA = queue.addItem('fileA.txt', '/fileA.txt', 100, 'upload');
            idB = queue.addItem('fileB.txt', '/fileB.txt', 200, 'upload');
        });

        await act(async () => {
            armed = true;
            queue.startTransfer(idA);
        });

        await act(async () => {});

        expect(queue.items.find(i => i.id === idB)?.status).toBe('error');
        expect(queue.items.find(i => i.id === idB)?.error).toBe('M1');
        expect(queue.items.find(i => i.id === idA)?.status).toBe('error');
        expect(queue.items.find(i => i.id === idA)?.error).toBe('M2');
    });
});
