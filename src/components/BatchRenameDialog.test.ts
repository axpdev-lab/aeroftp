// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { batchRenameConflicts, batchRenameExternalCollisions, type BatchRenameFile } from './BatchRenameDialog';

const file = (name: string, folder?: string): BatchRenameFile => ({
    name, path: `/root/${folder ? `${folder}/` : ''}${name}`, isDir: false, folder,
});

/**
 * A flattened search (`*`) can select files of several folders for one Batch
 * Rename. A name is taken per folder: the same new name in two folders is two
 * files, and a sibling of one folder does not block a rename in another.
 */
describe('Batch Rename conflicts, folder by folder', () => {
    it('lets two files of two folders take the same new name', () => {
        const files = [file('a.jpg', 'Summer'), file('b.jpg', 'Winter')];
        const renames = new Map(files.map(f => [f.path, 'photo.jpg']));
        expect(batchRenameConflicts(files, renames).size).toBe(0);
    });

    it('still refuses two files of one folder taking the same new name', () => {
        const files = [file('a.jpg', 'Summer'), file('b.jpg', 'Summer')];
        const renames = new Map(files.map(f => [f.path, 'photo.jpg']));
        expect(batchRenameConflicts(files, renames).size).toBe(1);
        const flat = [file('a.jpg'), file('b.jpg')];
        expect(batchRenameConflicts(flat, new Map(flat.map(f => [f.path, 'photo.jpg']))).size).toBe(1);
    });

    it('flags a sibling of the file\'s own folder, not one of another folder', () => {
        const files = [file('a.jpg', 'Summer')];
        const renames = new Map([[files[0].path, 'b.jpg']]);
        expect(batchRenameExternalCollisions(files, renames, [{ folder: 'Winter', name: 'b.jpg' }, 'b.jpg']).size).toBe(0);
        expect(batchRenameExternalCollisions(files, renames, [{ folder: 'Summer', name: 'b.jpg' }]).size).toBe(1);
    });

    it('reads a bare sibling name as the panel\'s own folder, as before', () => {
        const files = [file('a.jpg')];
        const renames = new Map([[files[0].path, 'b.jpg']]);
        expect(batchRenameExternalCollisions(files, renames, ['b.jpg']).size).toBe(1);
        expect(batchRenameExternalCollisions(files, renames, ['c.jpg']).size).toBe(0);
    });

    it('counts a batch file left unchanged as holding its name in its folder', () => {
        const files = [file('a.jpg', 'Summer'), file('b.jpg', 'Summer'), file('b.jpg', 'Winter')];
        // a.jpg -> b.jpg in Summer, where b.jpg stays: a collision. Winter's b.jpg is elsewhere.
        const renames = new Map([[files[0].path, 'b.jpg']]);
        expect(batchRenameExternalCollisions(files, renames, []).size).toBe(1);
        const onlyWinter = [file('a.jpg', 'Summer'), file('b.jpg', 'Winter')];
        expect(batchRenameExternalCollisions(onlyWinter, new Map([[onlyWinter[0].path, 'b.jpg']]), []).size).toBe(0);
    });
});
