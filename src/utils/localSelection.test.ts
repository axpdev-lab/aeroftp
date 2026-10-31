// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import type { LocalFile } from '../types';
import { asLocalFileEntry, deepestLocalRowsFirst, dropsIntoItself, focusedLocalPanel, localClipboardEntries, localRowFolder, localRowFolderKey, localRowName, localRowNameClashes, localPanelShows, outermostLocalRows, selectedLocalRows, selectedLocalTrees } from './localSelection';

const row = (name: string, is_dir = true): LocalFile => ({
    name, path: `/home/u/${name}`, size: is_dir ? null : 1, is_dir, modified: null,
});

describe('the rows a local selection names', () => {
    it('finds a nested row of a flattened search, which the listing does not hold', () => {
        const listing = [row('sub'), row('a.txt', false)];
        const shown = [row('sub'), row('sub/inner'), row('a.txt', false)];
        expect(selectedLocalRows(shown, listing, new Set(['sub/inner'])).map(f => f.path))
            .toEqual(['/home/u/sub/inner']);
    });

    it('takes the nested row selected, not an entry of the directory with the same name', () => {
        // The case a delete must get right: "Summer" at the top and
        // "Photos/Summer" below are two folders, and only one is selected.
        const listing = [row('Summer'), row('Photos')];
        const shown = [row('Summer'), row('Photos'), row('Photos/Summer')];
        expect(selectedLocalRows(shown, listing, new Set(['Photos/Summer'])).map(f => f.path))
            .toEqual(['/home/u/Photos/Summer']);
        expect(selectedLocalRows(shown, listing, new Set(['Summer'])).map(f => f.path))
            .toEqual(['/home/u/Summer']);
    });

    it('keeps a selected entry the rows no longer show', () => {
        const listing = [row('docs'), row('music')];
        const shown = [row('docs')];
        expect(selectedLocalRows(shown, listing, new Set(['docs', 'music'])).map(f => f.name))
            .toEqual(['docs', 'music']);
    });

    it('gives each name once, in the order the panel shows it', () => {
        const listing = [row('a'), row('b'), row('c')];
        const shown = [row('c'), row('a'), row('b')];
        expect(selectedLocalRows(shown, listing, new Set(['a', 'c'])).map(f => f.name))
            .toEqual(['c', 'a']);
    });

    it('gives nothing for a name neither holds', () => {
        expect(selectedLocalRows([row('a')], [row('a')], new Set(['gone']))).toEqual([]);
    });
});

describe('the name and folder a local row goes by', () => {
    it('names a nested row by its file name and its folder by the rest', () => {
        const nested = row('Photos 2026/Summer');
        expect(localRowName(nested)).toBe('Summer');
        expect(localRowFolderKey(nested)).toBe('Photos 2026');
        expect(localRowFolder(nested, '/home/u')).toBe('/home/u/Photos 2026');
        expect(localRowName(row('a/b/c.txt', false))).toBe('c.txt');
        expect(localRowFolderKey(row('a/b/c.txt', false))).toBe('a/b');
    });

    it('leaves an entry of the directory as it is', () => {
        const plain = { ...row('notes.txt', false), path: 'C:\\Users\\u\\notes.txt' };
        expect(localRowName(plain)).toBe('notes.txt');
        expect(localRowFolderKey(plain)).toBe('');
        // The panel's directory, verbatim: no separator rewritten.
        expect(localRowFolder(plain, 'C:\\Users\\u')).toBe('C:\\Users\\u');
        expect(asLocalFileEntry(plain)).toBe(plain);
    });

    it('keeps a Unix backslash as part of the name', () => {
        expect(localRowName(row('a\\b', false))).toBe('a\\b');
    });

    it('turns a nested row into an entry of its own folder, path unchanged', () => {
        const nested = row('Photos 2026/Summer');
        expect(asLocalFileEntry(nested)).toEqual({ ...nested, name: 'Summer' });
        expect(asLocalFileEntry(nested).path).toBe('/home/u/Photos 2026/Summer');
    });
});

describe('clipboard entries of a local selection', () => {
    it('names a nested row as the file a paste creates, and keeps its path', () => {
        const listing = [row('Photos')];
        const shown = [row('Photos'), row('Photos/Summer'), row('Photos/a.jpg', false)];
        expect(localClipboardEntries(shown, listing, new Set(['Photos/Summer', 'Photos/a.jpg']))).toEqual([
            { name: 'Summer', path: '/home/u/Photos/Summer', is_dir: true },
            { name: 'a.jpg', path: '/home/u/Photos/a.jpg', is_dir: false },
        ]);
    });

    it('takes a selected folder with what is below it, and that entry not again', () => {
        // Select All in a flattened search: a copy is the folder, once.
        const listing = [row('Photos')];
        const shown = [row('Photos'), row('Photos/Photos'), row('Photos/a.jpg', false)];
        expect(localClipboardEntries(shown, listing, new Set(shown.map(f => f.name)))).toEqual([
            { name: 'Photos', path: '/home/u/Photos', is_dir: true },
        ]);
    });
});

describe('selected rows that one destination cannot hold together', () => {
    it('names a file name two folders share, whatever its case', () => {
        expect(localRowNameClashes([row('A/notes.txt', false), row('B/notes.txt', false)])).toEqual(['notes.txt']);
        expect(localRowNameClashes([row('A/Notes.txt', false), row('notes.txt', false)])).toEqual(['Notes.txt']);
    });

    it('lets distinct names, and the entries of one folder, through', () => {
        expect(localRowNameClashes([row('A/a.txt', false), row('B/b.txt', false)])).toEqual([]);
        // A case-sensitive folder can hold both; they were side by side already.
        expect(localRowNameClashes([row('A/Notes.txt', false), row('A/notes.txt', false)])).toEqual([]);
        expect(localRowNameClashes([row('notes.txt', false), row('todo.txt', false)])).toEqual([]);
    });
});

describe('rows inside another selected row', () => {
    it('leaves out what a selected folder already holds', () => {
        const rows = [row('Photos'), row('Photos/a.jpg', false), row('Photos/Summer'), row('Photos 2026/b.jpg', false)];
        expect(outermostLocalRows(rows).map(f => f.name)).toEqual(['Photos', 'Photos 2026/b.jpg']);
    });

    it('compares Windows paths by their folders, whichever separator', () => {
        const dir = { name: 'Photos', path: 'C:\\Users\\u\\Photos', is_dir: true };
        const inner = { name: 'Photos/a.jpg', path: 'C:/Users/u/Photos/a.jpg', is_dir: false };
        const beside = { name: 'Photos2.jpg', path: 'C:\\Users\\u\\Photos2.jpg', is_dir: false };
        expect(outermostLocalRows([dir, inner, beside])).toEqual([dir, beside]);
    });

    it('gives an action on the whole selection each selected tree once', () => {
        const listing = [row('Photos'), row('notes.txt', false)];
        const shown = [row('Photos'), row('Photos/Summer'), row('Photos/Summer/a.jpg', false), row('Other/b.jpg', false), row('notes.txt', false)];
        const all = new Set(shown.map(f => f.name));
        expect(selectedLocalTrees(shown, listing, all).map(f => f.name)).toEqual(['Photos', 'Other/b.jpg', 'notes.txt']);
        // What a rename or a tag takes: every row.
        expect(selectedLocalRows(shown, listing, all)).toHaveLength(5);
        // A nested row on its own is its own tree.
        expect(selectedLocalTrees(shown, listing, new Set(['Photos/Summer/a.jpg'])).map(f => f.path))
            .toEqual(['/home/u/Photos/Summer/a.jpg']);
    });

    it('renames the deepest first, so a folder is renamed after what it holds', () => {
        const rows = [row('Photos'), row('Photos/Summer/a.jpg', false), row('Photos/Summer')];
        expect(deepestLocalRowsFirst(rows).map(f => f.name)).toEqual(['Photos/Summer/a.jpg', 'Photos/Summer', 'Photos']);
    });
});

describe('the local panel an action acts on', () => {
    const state = (over: Partial<Parameters<typeof focusedLocalPanel>[0]>) => ({
        isConnected: false, showRemotePanel: true, showDualLocalPanel: true, activeLocalPanelId: 'local2' as const, ...over,
    });

    it('is the focused panel while two local panels are shown', () => {
        expect(focusedLocalPanel(state({}))).toBe('local2');
        expect(focusedLocalPanel(state({ isConnected: true, showRemotePanel: false }))).toBe('local2');
        expect(focusedLocalPanel(state({ activeLocalPanelId: 'local' }))).toBe('local');
    });

    it('is never panel 2 once it is hidden, whatever the focus says', () => {
        // A server connected with the remote panel shown, or dual mode off.
        expect(focusedLocalPanel(state({ isConnected: true, showRemotePanel: true }))).toBe('local');
        expect(focusedLocalPanel(state({ showDualLocalPanel: false }))).toBe('local');
    });
});

describe('the panels that show a folder written to', () => {
    it('is the panel whose folder it is, with or without a trailing slash', () => {
        expect(localPanelShows('/home/u/Photos', '/home/u/Photos', false)).toBe(true);
        expect(localPanelShows('/home/u/Photos/', '/home/u/Photos', false)).toBe(true);
        expect(localPanelShows('/home/u/Other', '/home/u/Photos', false)).toBe(false);
    });

    it('is also a flattened panel above it, whose rows list the folder', () => {
        expect(localPanelShows('/home/u/Photos/Summer', '/home/u', true)).toBe(true);
        expect(localPanelShows('/home/u/Photos/Summer', '/home/u', false)).toBe(false);
        expect(localPanelShows('/etc', '/', true)).toBe(true);
    });

    it('is not a panel on a sibling that shares the name\'s start', () => {
        expect(localPanelShows('/home/u/Photos 2026/x', '/home/u/Photos', true)).toBe(false);
        expect(localPanelShows('/home/u', '/home/u/Photos', true)).toBe(false);
    });

    it('reads Windows paths with either separator', () => {
        expect(localPanelShows('C:\\Users\\u\\Photos', 'C:/Users/u', true)).toBe(true);
        expect(localPanelShows('C:\\Users\\u', 'C:/Users/u/', false)).toBe(true);
    });
});

describe('a drop that would put a dragged folder into itself', () => {
    it('is a drop on a dragged folder or on a folder below one', () => {
        expect(dropsIntoItself('/home/u/Photos', ['/home/u/Photos'])).toBe(true);
        expect(dropsIntoItself('/home/u/Photos/Summer', ['/home/u/notes.txt', '/home/u/Photos'])).toBe(true);
        expect(dropsIntoItself('/home/u/Photos/Summer/', ['/home/u/Photos/'])).toBe(true);
    });

    it('is not a drop beside it, above it, or on a folder that only shares its name\'s start', () => {
        expect(dropsIntoItself('/home/u/Other', ['/home/u/Photos'])).toBe(false);
        expect(dropsIntoItself('/home/u', ['/home/u/Photos'])).toBe(false);
        expect(dropsIntoItself('/home/u/Photos 2026', ['/home/u/Photos'])).toBe(false);
    });

    it('reads Windows paths with either separator', () => {
        expect(dropsIntoItself('C:\\Users\\u\\Photos\\Summer', ['C:/Users/u/Photos'])).toBe(true);
        expect(dropsIntoItself('C:/Users/u/Photos 2026', ['C:\\Users\\u\\Photos'])).toBe(false);
    });
});
