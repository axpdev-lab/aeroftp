// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** The file the editor has open, as the events below name it. */
export interface EditorBufferTarget {
    path: string;
    isRemote: boolean;
}

export const EDITOR_APPLY_EVENT = 'editor-apply';
export const EDITOR_READ_EVENT = 'editor-read';

/** Detail of {@link EDITOR_APPLY_EVENT}: `accept` is called by the editor that took the content. */
export interface EditorApplyDetail extends EditorBufferTarget {
    content: string;
    accept: () => void;
}

/** Detail of {@link EDITOR_READ_EVENT}: `reply` gets the buffer of the editor that has the file open. */
export interface EditorReadDetail extends EditorBufferTarget {
    reply: (content: string) => void;
}

/** Puts `content` into the open editor's buffer for `target`, left unsaved: the
 *  editor's own save then writes it where the file lives, on its server for a
 *  remote file. Returns whether an editor with that file open took it. Event
 *  listeners run synchronously, so the answer is known on return. */
export function applyToEditorBuffer(target: EditorBufferTarget, content: string): boolean {
    let accepted = false;
    const detail: EditorApplyDetail = { ...target, content, accept: () => { accepted = true; } };
    window.dispatchEvent(new CustomEvent(EDITOR_APPLY_EVENT, { detail }));
    return accepted;
}

/** The current buffer of the open editor for `target`, or null when no editor has it open. */
export function readEditorBuffer(target: EditorBufferTarget): string | null {
    let content: string | null = null;
    const detail: EditorReadDetail = { ...target, reply: (value) => { content = value; } };
    window.dispatchEvent(new CustomEvent(EDITOR_READ_EVENT, { detail }));
    return content;
}
