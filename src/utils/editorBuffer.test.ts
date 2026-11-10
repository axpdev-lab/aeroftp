// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// @vitest-environment jsdom

import { afterEach, describe, expect, it } from 'vitest';
import {
    applyToEditorBuffer, EDITOR_APPLY_EVENT, EDITOR_READ_EVENT, readEditorBuffer,
    type EditorApplyDetail, type EditorReadDetail,
} from './editorBuffer';

const listeners: Array<[string, EventListener]> = [];
const listen = (name: string, listener: EventListener) => {
    window.addEventListener(name, listener);
    listeners.push([name, listener]);
};
afterEach(() => {
    for (const [name, listener] of listeners.splice(0)) window.removeEventListener(name, listener);
});

describe('editorBuffer', () => {
    it('reports that no editor took the content when none has the file open', () => {
        expect(applyToEditorBuffer({ path: '/srv/a.txt', isRemote: true }, 'x')).toBe(false);
        expect(readEditorBuffer({ path: '/srv/a.txt', isRemote: true })).toBeNull();
    });

    it('hands the content to the editor that has the file open, and reads its buffer back', () => {
        let buffer = 'original';
        listen(EDITOR_APPLY_EVENT, (e) => {
            const detail = (e as CustomEvent<EditorApplyDetail>).detail;
            if (detail.path !== '/srv/a.txt' || !detail.isRemote) return;
            buffer = detail.content;
            detail.accept();
        });
        listen(EDITOR_READ_EVENT, (e) => {
            const detail = (e as CustomEvent<EditorReadDetail>).detail;
            if (detail.path === '/srv/a.txt' && detail.isRemote) detail.reply(buffer);
        });
        expect(readEditorBuffer({ path: '/srv/a.txt', isRemote: true })).toBe('original');
        expect(applyToEditorBuffer({ path: '/srv/a.txt', isRemote: true }, 'applied')).toBe(true);
        expect(buffer).toBe('applied');
        // The same path as a local file is another file.
        expect(applyToEditorBuffer({ path: '/srv/a.txt', isRemote: false }, 'local')).toBe(false);
        expect(buffer).toBe('applied');
    });
});
