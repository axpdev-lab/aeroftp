// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { expect, it, vi } from 'vitest';
const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
import { splitPathsByType } from './pathStaging';

it('literal_unix_paths picker preserves emitted file and directory paths', async () => {
    invoke.mockResolvedValue([
        { name: String.raw`dir\name`, path: String.raw`/tmp/dir\name`, is_dir: true },
        { name: String.raw`back\slash.txt`, path: String.raw`/tmp/back\slash.txt`, is_dir: false },
    ]);
    const result = await splitPathsByType([String.raw`/tmp/dir\name/`, String.raw`/tmp/back\slash.txt`]);
    expect(invoke).toHaveBeenCalledExactlyOnceWith('get_local_files', { path: '/tmp', showHidden: true });
    expect(result).toEqual({ files: [String.raw`/tmp/back\slash.txt`], dirs: [String.raw`/tmp/dir\name/`] });
});
it('keeps Windows picker separators and drive roots absolute', async () => {
    invoke.mockClear();
    invoke.mockResolvedValue([{ name: 'folder', path: 'D:/folder', is_dir: true }]);
    expect(await splitPathsByType([String.raw`D:\folder`])).toEqual({ files: [], dirs: ['D:/folder'] });
    expect(invoke).toHaveBeenCalledExactlyOnceWith('get_local_files', { path: 'D:/', showHidden: true });
});

it('stages a selected UNC share root as a directory without reading its children', async () => {
    invoke.mockClear();
    expect(await splitPathsByType([String.raw`\\server\share`])).toEqual({
        files: [],
        dirs: ['//server/share'],
    });
    expect(invoke).not.toHaveBeenCalled();
});

it('lists a UNC child from the share itself', async () => {
    invoke.mockClear();
    invoke.mockResolvedValue([{ name: 'folder', path: '//server/share/folder', is_dir: true }]);
    expect(await splitPathsByType([String.raw`\\server\share\folder`])).toEqual({
        files: [],
        dirs: ['//server/share/folder'],
    });
    expect(invoke).toHaveBeenCalledExactlyOnceWith('get_local_files', {
        path: '//server/share',
        showHidden: true,
    });
});

it('stages a selected Windows drive root as a directory', async () => {
    invoke.mockClear();
    expect(await splitPathsByType(['D:\\'])).toEqual({ files: [], dirs: ['D:/'] });
    expect(invoke).not.toHaveBeenCalled();
});
