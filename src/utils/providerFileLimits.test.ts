// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { compareEntries } from './compareEndpoints';
import { entriesThatWillNotFit, limitsFromHints, nameTooLong, type ProviderFileLimits } from './providerFileLimits';
import type { TransferOptimizationHints } from '../types';

const MB = 1024 * 1024;
const limits: ProviderFileLimits = { maxFileSize: 100 * MB, maxNameBytes: 16, maxNameChars: null };

const result = compareEntries(
    [
        { name: 'big.iso', isDir: false, size: 287 * MB, mtimeMs: 2000 },
        { name: 'small.txt', isDir: false, size: 10, mtimeMs: 2000 },
        { name: 'a-very-long-file-name.txt', isDir: false, size: 10, mtimeMs: 2000 },
        { name: 'huge-folder', isDir: true, size: 900 * MB, mtimeMs: 2000 },
    ],
    [{ name: 'remote-only-and-huge.bin', isDir: false, size: 900 * MB, mtimeMs: 2000 }],
);

describe('entriesThatWillNotFit', () => {
    it('flags what a mirror to the remote would send and the remote cannot store', () => {
        const found = entriesThatWillNotFit(result, 'local-remote', limits);
        const byName = found.map(f => [f.entry.name, f.reasons]).sort();
        expect(byName).toEqual([
            ['a-very-long-file-name.txt', ['name-too-long']],
            ['big.iso', ['too-large']],
        ]);
    });

    it('checks the other direction when the remote is on the left', () => {
        const found = entriesThatWillNotFit(result, 'remote-local', limits);
        expect(found.map(f => [f.entry.name, f.reasons])).toEqual([
            ['remote-only-and-huge.bin', ['too-large', 'name-too-long']],
        ]);
        expect(found[0].size).toBe(900 * MB);
    });

    it('says nothing without a documented limit or without a remote', () => {
        expect(entriesThatWillNotFit(result, 'local-remote', null)).toEqual([]);
        expect(entriesThatWillNotFit(result, 'local-local', limits)).toEqual([]);
        const noLimits = { max_file_size: null, max_name_bytes: null, max_name_chars: null } as unknown as TransferOptimizationHints;
        expect(limitsFromHints(noLimits)).toBeNull();
    });
});

describe('nameTooLong', () => {
    it('counts UTF-8 bytes and characters separately', () => {
        const name = 'è'.repeat(10); // 10 characters, 20 bytes
        expect(nameTooLong(name, { maxFileSize: null, maxNameBytes: 16, maxNameChars: null })).toBe(true);
        expect(nameTooLong(name, { maxFileSize: null, maxNameBytes: null, maxNameChars: 16 })).toBe(false);
        expect(nameTooLong('😀'.repeat(3), { maxFileSize: null, maxNameBytes: null, maxNameChars: 3 })).toBe(false);
    });
});
