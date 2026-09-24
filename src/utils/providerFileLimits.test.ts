// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { compareEntries } from './compareEndpoints';
import { entriesThatWillNotFit, limitsFromHints, nameTooLong, remoteDestinationPath, type ProviderFileLimits } from './providerFileLimits';
import type { TransferOptimizationHints } from '../types';

const MB = 1024 * 1024;
const NO_PATH = { maxPathBytes: null, maxPathChars: null };
const limits: ProviderFileLimits = { maxFileSize: 100 * MB, maxNameBytes: 16, maxNameChars: null, ...NO_PATH };

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
        expect(nameTooLong(name, { maxFileSize: null, maxNameBytes: 16, maxNameChars: null, ...NO_PATH })).toBe(true);
        expect(nameTooLong(name, { maxFileSize: null, maxNameBytes: null, maxNameChars: 16, ...NO_PATH })).toBe(false);
        expect(nameTooLong('😀'.repeat(3), { maxFileSize: null, maxNameBytes: null, maxNameChars: 3, ...NO_PATH })).toBe(false);
    });
});

describe('whole-path limits and the buckets a sync can send', () => {
    // OneDrive counts the whole decoded path (400 characters), S3 the whole
    // key: a short name under a deep remote folder can still be too long.
    it('checks the destination path, remote folder included', () => {
        const deep = compareEntries([{ name: 'a.txt', isDir: false, size: 1, mtimeMs: 2000 }], []);
        const pathLimits: ProviderFileLimits = { maxFileSize: null, maxNameBytes: null, maxNameChars: null, maxPathBytes: null, maxPathChars: 20 };
        expect(entriesThatWillNotFit(deep, 'local-remote', pathLimits, '/short')).toEqual([]);
        const found = entriesThatWillNotFit(deep, 'local-remote', pathLimits, '/a/much/deeper/remote/folder');
        expect(found.map(f => f.reasons)).toEqual([['path-too-long']]);
    });

    it('builds the remote path without a leading slash', () => {
        const entry = { name: 'x.bin', relativePath: 'sub/x.bin', bucket: 'only-left' as const };
        expect(remoteDestinationPath('/Docs/', entry)).toBe('Docs/sub/x.bin');
        expect(remoteDestinationPath('', entry)).toBe('sub/x.bin');
    });

    // Mirror also sends the local copy of entries that are newer on the remote
    // and of conflicts; those uploads can hit the limit too.
    it('includes entries newer on the remote and conflicts', () => {
        const both = compareEntries(
            [{ name: 'old-local.iso', isDir: false, size: 287 * MB, mtimeMs: 1000 }],
            [{ name: 'old-local.iso', isDir: false, size: 10, mtimeMs: 900000 }],
        );
        expect(both.buckets['newer-right'].length + both.buckets.conflict.length).toBe(1);
        const found = entriesThatWillNotFit(both, 'local-remote', limits);
        expect(found.map(f => [f.entry.name, f.reasons])).toEqual([['old-local.iso', ['too-large']]]);
    });
});
