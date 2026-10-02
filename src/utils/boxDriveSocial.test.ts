// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import appSource from '../App.tsx?raw';
import {
    addBoxCollaborator,
    deleteComment,
    listBoxCollaborators,
    listComments,
    normalizeComment,
    removeBoxCollaborator,
    unlockBoxFolder,
} from './boxDriveSocial';

const fakeInvoke = (answers: Record<string, unknown>) =>
    vi.fn(async (cmd: string) => answers[cmd]);

/**
 * 2.7.4 shipped these backend commands with their strings and advertised the
 * features, but the menus reached only part of them: Drive could add a
 * comment and never read or delete one, Box could lock a folder and never
 * unlock it, and Box comments and collaborators had no screen.
 */
describe('Box and Google Drive comments', () => {
    it('reads both providers into one shape', () => {
        expect(normalizeComment('box', {
            id: '7', message: 'hi', created_at: '2026-10-01T10:00:00Z', created_by: { name: 'Ana', login: 'ana@x' },
        })).toEqual({ id: '7', text: 'hi', author: 'Ana', createdAt: '2026-10-01T10:00:00Z' });
        expect(normalizeComment('googledrive', {
            id: 'c1', content: 'ok', createdTime: '2026-10-01T10:00:00Z', author: { displayName: 'Bo' },
        })).toEqual({ id: 'c1', text: 'ok', author: 'Bo', createdAt: '2026-10-01T10:00:00Z' });
        expect(normalizeComment('box', { message: 'no id' })).toBeNull();
    });

    it('lists through each provider command', async () => {
        const invoke = fakeInvoke({ box_list_comments: [{ id: '1', message: 'a' }], google_drive_list_comments: [{ id: '2', content: 'b' }] });
        expect((await listComments(invoke as never, 'box', '/f.txt')).map((c) => c.text)).toEqual(['a']);
        expect((await listComments(invoke as never, 'googledrive', '/f.txt')).map((c) => c.text)).toEqual(['b']);
        expect(invoke).toHaveBeenCalledWith('box_list_comments', { path: '/f.txt' });
        expect(invoke).toHaveBeenCalledWith('google_drive_list_comments', { path: '/f.txt' });
    });

    it('deletes by comment id, with the file path where Drive needs it', async () => {
        const invoke = fakeInvoke({});
        await deleteComment(invoke as never, 'box', '/f.txt', '9');
        await deleteComment(invoke as never, 'googledrive', '/f.txt', 'c9');
        expect(invoke).toHaveBeenNthCalledWith(1, 'box_delete_comment', { commentId: '9' });
        expect(invoke).toHaveBeenNthCalledWith(2, 'google_drive_delete_comment', { path: '/f.txt', commentId: 'c9' });
    });
});

describe('Box collaborators', () => {
    it('lists, invites and removes', async () => {
        const invoke = fakeInvoke({
            box_list_collaborations: [{ id: 'k1', role: 'editor', accessible_by: { name: 'Ana', login: 'ana@x' } }],
        });
        expect(await listBoxCollaborators(invoke as never, '/d')).toEqual([{ id: 'k1', who: 'Ana', role: 'editor' }]);
        await addBoxCollaborator(invoke as never, '/d', ' bo@x ', 'viewer');
        await removeBoxCollaborator(invoke as never, 'k1');
        expect(invoke).toHaveBeenCalledWith('box_add_collaboration', { path: '/d', email: 'bo@x', role: 'viewer' });
        expect(invoke).toHaveBeenCalledWith('box_remove_collaboration', { collabId: 'k1' });
    });
});

describe('Box folder unlock', () => {
    it('removes every lock on the folder and says how many', async () => {
        const invoke = fakeInvoke({ box_list_folder_locks: [{ id: 'L1' }, { id: 'L2' }] });
        expect(await unlockBoxFolder(invoke as never, '/d')).toBe(2);
        expect(invoke).toHaveBeenCalledWith('box_list_folder_locks', { path: '/d' });
        expect(invoke).toHaveBeenCalledWith('box_unlock_folder', { lockId: 'L1' });
        expect(invoke).toHaveBeenCalledWith('box_unlock_folder', { lockId: 'L2' });
    });

    it('reports a folder without locks as zero, sending no unlock', async () => {
        const invoke = fakeInvoke({ box_list_folder_locks: [] });
        expect(await unlockBoxFolder(invoke as never, '/d')).toBe(0);
        expect(invoke).toHaveBeenCalledTimes(1);
    });
});

describe('the context menus reach them', () => {
    it('offers Unlock Folder, comments and collaborators for Box, and full comments for Drive', () => {
        expect(appSource).toContain('unlockBoxFolder(invoke, file.path)');
        expect(appSource).toContain("setCommentsTarget({ provider: 'box'");
        expect(appSource).toContain("setCommentsTarget({ provider: 'googledrive'");
        expect(appSource).toContain('<BoxCollaboratorsDialog');
        expect(appSource).not.toContain('GoogleDriveCommentDialog');
    });
});
