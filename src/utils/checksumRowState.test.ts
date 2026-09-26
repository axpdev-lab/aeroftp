// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { checksumRowState } from './checksumRowState';

describe('checksumRowState', () => {
    it('offers every standard algorithm for a local file', () => {
        expect(checksumRowState('sha512', false, null)).toBe('calculate');
    });

    it('names the backend instead of offering what it cannot produce', () => {
        expect(checksumRowState('md5', false, new Set(['sha1', 'sha256']))).toBe('not-on-backend');
        expect(checksumRowState('sha256', false, new Set(['sha1', 'sha256']))).toBe('calculate');
    });

    // FTP computes each digest on request, so a CRC32 the server advertises is
    // only ever returned when asked for by name: a row that could not ask
    // left it unreachable. The same held for a backend whose only digest is
    // its own (Dropbox, Koofr, GitHub), where no standard row could be clicked.
    it('offers a backend digest the capability lists', () => {
        expect(checksumRowState('crc32', true, new Set(['md5', 'crc32']))).toBe('calculate');
        expect(checksumRowState('dropbox', true, new Set(['dropbox']))).toBe('calculate');
    });

    it('keeps a backend digest the capability does not list as server-only', () => {
        expect(checksumRowState('crc32', true, new Set(['md5']))).toBe('server-only');
        expect(checksumRowState('quickxor', true, null)).toBe('server-only');
    });
});
