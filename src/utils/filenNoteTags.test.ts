// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import panelSource from '../components/FilenNotesPanel.tsx?raw';
import { changeNoteType, createTagOnNote, deleteTag, renameTag, untagNote } from './filenNoteTags';

/**
 * The Filen notes panel read tags it could not manage, and its type selector
 * never called the type change endpoint, so a type change without a text
 * edit was lost on reload.
 */
describe('Filen note tags and type', () => {
    it('changes the type through filen_notes_change_type', async () => {
        const invoke = vi.fn(async () => undefined);
        await changeNoteType(invoke as never, 'n1', 'md');
        expect(invoke).toHaveBeenCalledWith('filen_notes_change_type', { uuid: 'n1', noteType: 'md' });
    });

    it('creates a tag and puts it on the note', async () => {
        const invoke = vi.fn(async (cmd: string) => (cmd === 'filen_notes_tags_create' ? 't9' : undefined));
        expect(await createTagOnNote(invoke as never, 'n1', '  work ')).toBe('t9');
        expect(invoke).toHaveBeenNthCalledWith(1, 'filen_notes_tags_create', { name: 'work' });
        expect(invoke).toHaveBeenNthCalledWith(2, 'filen_notes_tag_note', { noteUuid: 'n1', tagUuid: 't9' });
        await expect(createTagOnNote(invoke as never, 'n1', '   ')).rejects.toThrow();
    });

    it('removes the new tag when putting it on the note fails, so a retry does not leave a duplicate', async () => {
        const live = new Set<string>();
        let created = 0;
        let attachFailures = 1;
        const invoke = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
            if (cmd === 'filen_notes_tags_create') { const uuid = `t${++created}`; live.add(uuid); return uuid; }
            if (cmd === 'filen_notes_tag_note' && attachFailures-- > 0) throw new Error('tag refused');
            if (cmd === 'filen_notes_tags_delete') live.delete(args!.tagUuid as string);
            return undefined;
        });
        await expect(createTagOnNote(invoke as never, 'n1', 'work')).rejects.toThrow('tag refused');
        expect(await createTagOnNote(invoke as never, 'n1', 'work')).toBe('t2');
        expect([...live]).toEqual(['t2']);
    });

    it('removes, renames and deletes tags with the arguments each command takes', async () => {
        const invoke = vi.fn(async () => undefined);
        await untagNote(invoke as never, 'n1', 't1');
        await renameTag(invoke as never, 't1', ' home ');
        await deleteTag(invoke as never, 't1');
        expect(invoke).toHaveBeenNthCalledWith(1, 'filen_notes_untag_note', { noteUuid: 'n1', tagUuid: 't1' });
        expect(invoke).toHaveBeenNthCalledWith(2, 'filen_notes_tags_rename', { tagUuid: 't1', name: 'home' });
        expect(invoke).toHaveBeenNthCalledWith(3, 'filen_notes_tags_delete', { tagUuid: 't1' });
    });

    it('wires the panel: the type selector calls the endpoint, tags can be managed', () => {
        expect(panelSource).toContain('changeNoteType(invoke, selectedNote.uuid');
        expect(panelSource).toContain('createTagOnNote(invoke');
        expect(panelSource).toContain('untagNote(invoke');
        expect(panelSource).toContain('renameTag(invoke');
        expect(panelSource).toContain('deleteTag(invoke');
    });
});
