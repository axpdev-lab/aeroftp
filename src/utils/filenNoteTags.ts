// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Filen note tags and note type.
 *
 * The notes panel showed a note's tags but could not add, remove, create,
 * rename or delete one, and its type selector only changed local state: the
 * type reached the server only as a side field of the next content save, so
 * changing it without editing the text was lost. The backend has had the
 * dedicated calls since the notes landed (7cf456b3b); nothing used them.
 */

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

export type FilenNoteType = 'text' | 'md' | 'code' | 'rich' | 'checklist';

/** Change the type through Filen's own endpoint, which re-encrypts the content for it. */
export async function changeNoteType(invoke: Invoke, uuid: string, noteType: FilenNoteType): Promise<void> {
    await invoke('filen_notes_change_type', { uuid, noteType });
}

export async function tagNote(invoke: Invoke, noteUuid: string, tagUuid: string): Promise<void> {
    await invoke('filen_notes_tag_note', { noteUuid, tagUuid });
}

export async function untagNote(invoke: Invoke, noteUuid: string, tagUuid: string): Promise<void> {
    await invoke('filen_notes_untag_note', { noteUuid, tagUuid });
}

/** Create a tag and put it on the note; returns the new tag's uuid. */
export async function createTagOnNote(invoke: Invoke, noteUuid: string, name: string): Promise<string> {
    const clean = name.trim();
    if (!clean) throw new Error('A tag needs a name');
    const tagUuid = await invoke<string>('filen_notes_tags_create', { name: clean });
    try {
        await tagNote(invoke, noteUuid, tagUuid);
    } catch (err) {
        // Left behind, the unattached tag would be duplicated by a retry with the same name.
        await deleteTag(invoke, tagUuid).catch(() => undefined);
        throw err;
    }
    return tagUuid;
}

export async function renameTag(invoke: Invoke, tagUuid: string, name: string): Promise<void> {
    const clean = name.trim();
    if (!clean) throw new Error('A tag needs a name');
    await invoke('filen_notes_tags_rename', { tagUuid, name: clean });
}

export async function deleteTag(invoke: Invoke, tagUuid: string): Promise<void> {
    await invoke('filen_notes_tags_delete', { tagUuid });
}
