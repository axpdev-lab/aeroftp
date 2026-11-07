// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { EDITOR_APPLY_EVENT, EDITOR_READ_EVENT, type EditorApplyDetail, type EditorReadDetail } from '../../utils/editorBuffer';

const fs = vi.hoisted(() => ({ writeTextFile: vi.fn(), readTextFile: vi.fn() }));
vi.mock('@tauri-apps/plugin-fs', () => fs);
vi.mock('../../utils/pickPath', () => ({ pickSave: vi.fn() }));
vi.mock('../../utils/clipboard', () => ({ copyText: vi.fn() }));
vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));

import { CodeBlockActions } from './CodeBlockActions';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/**
 * A remote file's Apply and Diff reach the editor's buffer, never the local
 * filesystem. With the editor hidden, or showing another file, there is no
 * buffer: the action that was asked for says so, and how to recover.
 */
describe('Apply and Diff on a remote file whose editor is not open', () => {
    let host: HTMLDivElement;
    let root: Root;
    beforeEach(() => {
        host = document.createElement('div');
        document.body.appendChild(host);
        root = createRoot(host);
        act(() => root.render(
            <CodeBlockActions code="new" language="txt" editorFilePath="/srv/a.txt" editorFileName="a.txt" editorFileIsRemote />,
        ));
    });
    afterEach(() => {
        act(() => root.unmount());
        host.remove();
        vi.clearAllMocks();
    });
    const buttons = () => host.querySelectorAll('button');
    const click = async (button: Element) => { await act(async () => { button.dispatchEvent(new MouseEvent('click', { bubbles: true })); }); };
    const alert = () => host.querySelector('[role="alert"]')?.textContent ?? null;

    it('says on Apply that the editor has to show the file', async () => {
        await click(buttons()[1]);
        expect(alert()).toBe('ai.codeBlock.reopenEditor');
        expect(buttons()[1].className).toContain('text-red-400');
        expect(fs.writeTextFile).not.toHaveBeenCalled();
    });

    it('says it on Diff, without marking Apply as failed', async () => {
        await click(buttons()[2]);
        expect(alert()).toBe('ai.codeBlock.reopenEditor');
        expect(buttons()[2].className).toContain('text-red-400');
        expect(buttons()[1].className).not.toContain('text-red-400');
        expect(fs.readTextFile).not.toHaveBeenCalled();
    });

    // The diff of the open file, then the editor hidden before Apply in the diff.
    const diffApply = () => [...host.querySelectorAll('button')].find(b => b.textContent === 'ai.diff.apply');
    const openDiff = async () => {
        const read = (event: Event) => (event as CustomEvent<EditorReadDetail>).detail.reply('old');
        window.addEventListener(EDITOR_READ_EVENT, read);
        try {
            await click(buttons()[2]);
        } finally {
            window.removeEventListener(EDITOR_READ_EVENT, read);
        }
        expect(diffApply()).toBeDefined();
    };

    it('keeps the diff open when its Apply cannot reach the editor', async () => {
        await openDiff();
        await click(diffApply()!);
        expect(diffApply()).toBeDefined();
        expect(alert()).toBe('ai.codeBlock.reopenEditor');
    });

    it('closes the diff once its Apply lands in the editor', async () => {
        await openDiff();
        const accept = (event: Event) => (event as CustomEvent<EditorApplyDetail>).detail.accept();
        window.addEventListener(EDITOR_APPLY_EVENT, accept);
        try {
            await click(diffApply()!);
        } finally {
            window.removeEventListener(EDITOR_APPLY_EVENT, accept);
        }
        expect(diffApply()).toBeUndefined();
    });

    it('says nothing when the editor takes the code', async () => {
        const accept = (event: Event) => (event as CustomEvent<EditorApplyDetail>).detail.accept();
        window.addEventListener(EDITOR_APPLY_EVENT, accept);
        try {
            await click(buttons()[1]);
        } finally {
            window.removeEventListener(EDITOR_APPLY_EVENT, accept);
        }
        expect(alert()).toBeNull();
        expect(fs.writeTextFile).not.toHaveBeenCalled();
    });
});
