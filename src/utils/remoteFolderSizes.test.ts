// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { FolderSizeResult } from '../types/aerofile';
import { remoteFolderSizes, type RemoteFolderSizeDeps, type RemoteSizeProgress } from './remoteFolderSizes';

/** The provider and the app around remote counts, each count answered by hand. */
function harness(options: { listenFails?: boolean } = {}) {
    const figures = new Map<string, FolderSizeResult>();
    const calculating = new Set<string>();
    const handlers: ((p: RemoteSizeProgress) => void)[] = [];
    const counts: { path: string; scanId: string; resolve: (r: FolderSizeResult) => void; reject: (e: unknown) => void }[] = [];
    const errors: unknown[] = [];
    const cancels: string[] = [];
    let ids = 0;
    const deps: RemoteFolderSizeDeps = {
        listen: async (handler) => {
            if (options.listenFails) throw new Error('listen failed');
            handlers.push(handler);
            return () => { handlers.splice(handlers.indexOf(handler), 1); };
        },
        count: (path, scanId) => new Promise((resolve, reject) => { counts.push({ path, scanId, resolve, reject }); }),
        cancel: (scanId) => { cancels.push(scanId); },
        newScanId: () => `scan-${++ids}`,
        setFigure: (key, update) => {
            const figure = update(figures.get(key));
            if (figure) figures.set(key, figure);
            else figures.delete(key);
        },
        setCalculating: (key, on) => { if (on) calculating.add(key); else calculating.delete(key); },
        onError: (err) => { errors.push(err); },
    };
    const emit = (p: RemoteSizeProgress) => { [...handlers].forEach(handler => handler(p)); };
    return { sizes: remoteFolderSizes(deps), figures, calculating, counts, emit, errors, cancels };
}

const settle = () => new Promise(resolve => setTimeout(resolve, 0));
const totals = (bytes: number, partial = false) => ({ total_bytes: bytes, file_count: 1, dir_count: 0, partial });

describe('remote folder sizes in Properties', () => {
    it('keeps the figure of a count that ends', async () => {
        const h = harness();
        void h.sizes.count('/a', 'k');
        await settle();
        h.emit({ scan_id: 'scan-1', ...totals(5, true) });
        expect(h.figures.get('k')?.total_bytes).toBe(5);
        h.counts[0].resolve(totals(10));
        await settle();
        expect(h.figures.get('k')).toEqual(totals(10));
        expect(h.calculating.has('k')).toBe(false);
        // Its dialog closes: nothing is left to stop.
        h.sizes.stop();
        expect(h.cancels).toEqual([]);
        expect(h.figures.get('k')).toEqual(totals(10));
    });

    it('stops on the provider the count it ends, by that count\'s id', async () => {
        // The cancel named no count: sent as a dialog closed and arriving after
        // a reopened dialog's count began, it stopped that one.
        const h = harness();
        void h.sizes.count('/a', 'k');
        await settle();
        h.sizes.stop();
        expect(h.cancels).toEqual(['scan-1']);
        void h.sizes.count('/a', 'k');
        await settle();
        expect(h.counts[1].scanId).toBe('scan-2');
        expect(h.cancels, 'a new count is not cancelled').toEqual(['scan-1']);
        h.sizes.stop();
        expect(h.cancels).toEqual(['scan-1', 'scan-2']);
    });

    it('counts a folder afresh when it is opened again after its dialog closed', async () => {
        // The cancelled count returned its partial figure, which was kept: the
        // next open found a figure and showed it instead of counting.
        const h = harness();
        void h.sizes.count('/a', 'k');
        await settle();
        h.emit({ scan_id: 'scan-1', ...totals(5, true) });
        h.sizes.stop();
        expect(h.figures.has('k')).toBe(false);
        expect(h.calculating.has('k')).toBe(false);
        h.counts[0].resolve(totals(5, true));
        await settle();
        // What the app checks before it counts on an open.
        expect(h.figures.has('k') || h.calculating.has('k')).toBe(false);
        void h.sizes.count('/a', 'k');
        await settle();
        expect(h.counts).toHaveLength(2);
        expect(h.calculating.has('k')).toBe(true);
    });

    it('lets no late result of a cancelled count write, even over a new count of the same folder', async () => {
        const h = harness();
        void h.sizes.count('/a', 'k');
        await settle();
        h.sizes.stop();
        void h.sizes.count('/a', 'k');
        await settle();
        // The first count reports and returns after the second started.
        h.emit({ scan_id: 'scan-1', ...totals(999, true) });
        h.counts[0].resolve(totals(999));
        await settle();
        expect(h.figures.has('k')).toBe(false);
        expect(h.calculating.has('k'), 'the old count ended the new one\'s spinner').toBe(true);
        h.emit({ scan_id: 'scan-2', ...totals(7, true) });
        h.counts[1].resolve(totals(10));
        await settle();
        expect(h.figures.get('k')).toEqual(totals(10));
        expect(h.calculating.has('k')).toBe(false);
    });

    it('ends the count of one folder when another starts', async () => {
        const h = harness();
        void h.sizes.count('/a', 'a');
        await settle();
        h.emit({ scan_id: 'scan-1', ...totals(5, true) });
        void h.sizes.count('/b', 'b');
        await settle();
        expect(h.cancels).toEqual(['scan-1']);
        expect(h.figures.has('a')).toBe(false);
        expect(h.calculating.has('a')).toBe(false);
        h.counts[0].resolve(totals(5, true));
        h.counts[1].resolve(totals(3));
        await settle();
        expect(h.figures.has('a')).toBe(false);
        expect(h.figures.get('b')).toEqual(totals(3));
    });

    it('does not start a second count of a folder being counted', async () => {
        const h = harness();
        void h.sizes.count('/a', 'k');
        void h.sizes.count('/a', 'k');
        await settle();
        expect(h.counts).toHaveLength(1);
    });

    it('marks the figure partial and reports an error of the count it keeps', async () => {
        const h = harness();
        void h.sizes.count('/a', 'k');
        await settle();
        h.emit({ scan_id: 'scan-1', ...totals(5) });
        h.counts[0].reject(new Error('gone'));
        await settle();
        expect(h.figures.get('k')).toEqual({ ...totals(5), partial: true });
        expect(h.errors).toHaveLength(1);
        expect(h.calculating.has('k')).toBe(false);
    });

    it('leaves no spinner behind when the progress listener cannot be set up', async () => {
        // The key stayed in the calculating set, and every later count of the
        // folder returned at once.
        const h = harness({ listenFails: true });
        await h.sizes.count('/a', 'k');
        expect(h.calculating.has('k')).toBe(false);
        expect(h.errors).toHaveLength(1);
    });
});
