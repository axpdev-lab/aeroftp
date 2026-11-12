// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement, useEffect, useLayoutEffect } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
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
