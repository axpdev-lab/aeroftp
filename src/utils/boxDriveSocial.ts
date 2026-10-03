// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Comments, collaborators and folder locks on Box and Google Drive.
 *
 * The backend commands shipped in 2.7.4 together with their strings, and the
 * changelog and Help > Providers advertised them, but the context menus only
 * ever reached part of them: Google Drive could add a comment and never list
 * or delete one, Box could lock a folder and never unlock it, and Box
 * comments and collaborators had no screen at all.
 */

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

export type CommentsProvider = 'box' | 'googledrive';

export interface FileComment {
    id: string;
    text: string;
    author: string | null;
    createdAt: string | null;
}

type Raw = Record<string, unknown>;
const str = (v: unknown): string | null => (typeof v === 'string' && v.length > 0 ? v : null);
const obj = (v: unknown): Raw => (v && typeof v === 'object' ? (v as Raw) : {});

/** Box: `{ id, message, created_at, created_by: { name, login } }`; Drive: `{ id, content, createdTime, author: { displayName } }`. */
export function normalizeComment(provider: CommentsProvider, raw: unknown): FileComment | null {
    const r = obj(raw);
    const id = str(r.id);
    if (!id) return null;
    if (provider === 'box') {
        const by = obj(r.created_by);
        return { id, text: str(r.message) ?? '', author: str(by.name) ?? str(by.login), createdAt: str(r.created_at) };
    }
    const author = obj(r.author);
    return { id, text: str(r.content) ?? '', author: str(author.displayName), createdAt: str(r.createdTime) };
}

export async function listComments(invoke: Invoke, provider: CommentsProvider, path: string): Promise<FileComment[]> {
    const cmd = provider === 'box' ? 'box_list_comments' : 'google_drive_list_comments';
    const raw = await invoke<unknown[]>(cmd, { path });
    return (Array.isArray(raw) ? raw : [])
        .map((c) => normalizeComment(provider, c))
        .filter((c): c is FileComment => c !== null);
}

export async function addComment(invoke: Invoke, provider: CommentsProvider, path: string, message: string): Promise<void> {
    const cmd = provider === 'box' ? 'box_add_comment' : 'google_drive_add_comment';
    await invoke(cmd, { path, message });
}

export async function deleteComment(invoke: Invoke, provider: CommentsProvider, path: string, commentId: string): Promise<void> {
    if (provider === 'box') {
        await invoke('box_delete_comment', { commentId });
    } else {
        await invoke('google_drive_delete_comment', { path, commentId });
    }
}

/** Box collaboration roles, in the order Box lists them. */
export const BOX_ROLES = [
    'editor',
    'viewer',
    'previewer',
    'uploader',
    'previewer uploader',
    'viewer uploader',
    'co-owner',
] as const;
export type BoxRole = (typeof BOX_ROLES)[number];

export interface BoxCollaborator {
    id: string;
    who: string;
    role: string;
}

/** `{ id, role, accessible_by: { name, login } }` */
export function normalizeCollaborator(raw: unknown): BoxCollaborator | null {
    const r = obj(raw);
    const id = str(r.id);
    if (!id) return null;
    const by = obj(r.accessible_by);
    return {
        id,
        who: str(by.name) ?? str(by.login) ?? '?',
        role: str(r.role) ?? '',
    };
}

export async function listBoxCollaborators(invoke: Invoke, path: string): Promise<BoxCollaborator[]> {
    const raw = await invoke<unknown[]>('box_list_collaborations', { path });
    return (Array.isArray(raw) ? raw : [])
        .map(normalizeCollaborator)
        .filter((c): c is BoxCollaborator => c !== null);
}

export async function addBoxCollaborator(invoke: Invoke, path: string, email: string, role: BoxRole): Promise<void> {
    await invoke('box_add_collaboration', { path, email: email.trim(), role });
}

export async function removeBoxCollaborator(invoke: Invoke, collabId: string): Promise<void> {
    await invoke('box_remove_collaboration', { collabId });
}

/**
 * Remove every lock on a Box folder. Returns how many were removed, so the
 * caller can tell the user when the folder was not locked at all.
 */
export async function unlockBoxFolder(invoke: Invoke, path: string): Promise<number> {
    const locks = await invoke<unknown[]>('box_list_folder_locks', { path });
    const ids = (Array.isArray(locks) ? locks : [])
        .map((l) => str(obj(l).id))
        .filter((id): id is string => id !== null);
    for (const lockId of ids) {
        await invoke('box_unlock_folder', { lockId });
    }
    return ids.length;
}
