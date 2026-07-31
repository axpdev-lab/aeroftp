// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect, beforeEach } from 'vitest';
import {
    clearThumbnailCache,
    getThumbnail,
    keyFor,
    putThumbnail,
    signatureOf,
    thumbnailCacheStats,
} from './thumbnailCache';

describe('thumbnail cache (#347)', () => {
    beforeEach(() => clearThumbnailCache());

    it('refuses to cache anything it cannot tell apart in time', () => {
        // No size and no mtime means a changed file would be served from the old
        // entry forever. A null key makes the caller fetch every time instead,
        // which is the previous behaviour and the safe one.
        expect(signatureOf(null, null)).toBeNull();
        expect(keyFor('local', '/a.png', null)).toBeNull();
        putThumbnail(null, 'data:image/png;base64,AAAA');
        expect(getThumbnail(null)).toBeUndefined();
        expect(thumbnailCacheStats().entries).toBe(0);
    });

    it('treats the same path as a different file once size or mtime moves', () => {
        const before = keyFor('local', '/a.png', signatureOf(1024, '2026-07-30T10:00:00Z'));
        const afterEdit = keyFor('local', '/a.png', signatureOf(2048, '2026-07-31T10:00:00Z'));
        const afterTouch = keyFor('local', '/a.png', signatureOf(1024, '2026-07-31T10:00:00Z'));
        expect(before).not.toBe(afterEdit);
        expect(before).not.toBe(afterTouch);

        putThumbnail(before, 'OLD');
        expect(getThumbnail(afterEdit)).toBeUndefined();
        expect(getThumbnail(before)).toBe('OLD');
    });

    it('keeps two sources that share a path apart', () => {
        const sig = signatureOf(10, '2026-07-31T10:00:00Z');
        putThumbnail(keyFor('local', '/photos/a.png', sig), 'LOCAL');
        putThumbnail(keyFor('remote:session-1', '/photos/a.png', sig), 'REMOTE-1');
        putThumbnail(keyFor('remote:session-2', '/photos/a.png', sig), 'REMOTE-2');
        expect(getThumbnail(keyFor('local', '/photos/a.png', sig))).toBe('LOCAL');
        expect(getThumbnail(keyFor('remote:session-1', '/photos/a.png', sig))).toBe('REMOTE-1');
        expect(getThumbnail(keyFor('remote:session-2', '/photos/a.png', sig))).toBe('REMOTE-2');
    });

    it('survives a view switch and a walk out of the directory and back', () => {
        // The request itself: Icons → List → Icons, and leaving the folder, must
        // not re-download what was already fetched.
        const key = keyFor('remote:s', '/pics/holiday.jpg', signatureOf(4096, 'm'));
        putThumbnail(key, 'PIC');
        for (let i = 0; i < 5; i++) expect(getThumbnail(key)).toBe('PIC');
        expect(thumbnailCacheStats().entries).toBe(1);
    });

    it('stays inside its byte budget, evicting least-recently-used', () => {
        const { maxBytes } = thumbnailCacheStats();
        const chunk = 'x'.repeat(Math.floor(maxBytes / 4));
        const key = (n: number) => keyFor('local', `/big-${n}.png`, signatureOf(n, 'm'));

        for (let n = 0; n < 4; n++) putThumbnail(key(n), chunk);
        expect(thumbnailCacheStats().bytes).toBeLessThanOrEqual(maxBytes);

        // Touch 0 so it is no longer the oldest, then overflow by one.
        expect(getThumbnail(key(0))).toBe(chunk);
        putThumbnail(key(4), chunk);

        expect(thumbnailCacheStats().bytes).toBeLessThanOrEqual(maxBytes);
        expect(getThumbnail(key(0)), 'recently used survives').toBe(chunk);
        expect(getThumbnail(key(1)), 'least recently used is evicted').toBeUndefined();
        expect(getThumbnail(key(4)), 'the new entry is in').toBe(chunk);
    });

    it('declines an entry larger than the whole budget rather than thrashing', () => {
        const { maxBytes } = thumbnailCacheStats();
        putThumbnail(keyFor('local', '/kept.png', signatureOf(1, 'm')), 'KEPT');
        putThumbnail(keyFor('local', '/huge.png', signatureOf(2, 'm')), 'y'.repeat(maxBytes + 1));
        expect(getThumbnail(keyFor('local', '/huge.png', signatureOf(2, 'm')))).toBeUndefined();
        expect(getThumbnail(keyFor('local', '/kept.png', signatureOf(1, 'm'))), 'not evicted for it').toBe('KEPT');
    });

    it('accounts correctly when the same key is written twice', () => {
        const key = keyFor('local', '/a.png', signatureOf(1, 'm'));
        putThumbnail(key, 'aaaa');
        putThumbnail(key, 'bb');
        expect(thumbnailCacheStats()).toMatchObject({ entries: 1, bytes: 2 });
        expect(getThumbnail(key)).toBe('bb');
    });
});
