// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { expect, it, vi } from 'vitest';
import { isWindowsLocalPath, normalizeLocalPath, editorPathsMatch, localPathBasename, localDropTarget } from './localPath';

it('literal_unix_paths editor events never reload a colliding twin', () => {
    const literal = String.raw`/tmp/back\slash.txt`;
    expect(normalizeLocalPath(literal)).toBe(literal);
    expect(editorPathsMatch(literal, '/tmp/back/slash.txt', false)).toBe(false);
    expect(editorPathsMatch('/tmp/x/same.txt', '/tmp/y/same.txt', false)).toBe(false);
    expect(editorPathsMatch(literal, literal, false)).toBe(true);
    expect(editorPathsMatch(String.raw`D:\dir\file.txt`, 'D:/dir/file.txt', false)).toBe(true);
    expect(editorPathsMatch(String.raw`D:\dir\file.txt`, 'D:/dir/file.txt', true)).toBe(false);
    expect(normalizeLocalPath(String.raw`\\server\share\dir`)).toBe('//server/share/dir');
});

it('literal_unix_paths native drops emit a sibling literal target instead of a truncated name', () => {
    expect(localDropTarget(String.raw`/source/back\slash.txt`, '/dest')).toBe(String.raw`/dest/back\slash.txt`);
    expect(localDropTarget(String.raw`/source/dir\name/`, '/dest/')).toBe(String.raw`/dest/dir\name`);
    expect(localPathBasename(String.raw`/tmp/dir\name`)).toBe(String.raw`dir\name`);
    expect(localDropTarget(String.raw`C:\source\file.txt`, String.raw`D:\dest`)).toBe('D:/dest/file.txt');
});

it('interprets slash-only UNC roots using the native host', () => {
    try {
        vi.stubGlobal('navigator', { platform: 'Linux x86_64' });
        expect(isWindowsLocalPath('//tmp/folder')).toBe(false);
        vi.stubGlobal('navigator', { platform: 'Win32' });
        expect(isWindowsLocalPath('//server/share')).toBe(true);
    } finally { vi.unstubAllGlobals(); }
});
