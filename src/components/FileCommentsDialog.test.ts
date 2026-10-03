// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FileCommentsDialog } from './FileCommentsDialog';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('../i18n', async () => {
    const { translations } = (await import('../i18n/locales/en.json')).default as { translations: Record<string, unknown> };
    const t = (key: string) => {
        const value = key.split('.').reduce<unknown>((node, part) => (node as Record<string, unknown> | undefined)?.[part], translations);
        return typeof value === 'string' ? value : key;
    };
    return { useTranslation: () => t };
});

const NO_COMMENTS = 'No comments yet';

let root: Root;
let host: HTMLDivElement;
const render = async () => {
    await act(async () => root.render(createElement(FileCommentsDialog, { provider: 'box', filePath: '/f.txt', fileName: 'f.txt', onClose: () => {} })));
};
const type = async (element: HTMLTextAreaElement, value: string) => {
    await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(element, value);
        element.dispatchEvent(new Event('input', { bubbles: true }));
    });
};
const tab = (shiftKey = false) => {
    const event = new KeyboardEvent('keydown', { key: 'Tab', shiftKey, bubbles: true, cancelable: true });
    (document.activeElement ?? document.body).dispatchEvent(event);
    return event;
};

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    invoke.mockReset();
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

describe('FileCommentsDialog load failures', () => {
    it('shows a failed first load as an error, not as a file without comments', async () => {
        invoke.mockRejectedValue('List comments failed: 403');
        await render();
        expect(host.textContent).toContain('List comments failed: 403');
        expect(host.textContent).not.toContain(NO_COMMENTS);
    });

    it('keeps the comments on screen when the reload after posting fails', async () => {
        let lists = 0;
        invoke.mockImplementation(async (command: string) => {
            if (command !== 'box_list_comments') return undefined;
            if (++lists > 1) throw 'List comments failed: 500';
            return [{ id: '1', message: 'first note' }];
        });
        await render();
        await type(host.querySelector('textarea')!, 'second note');
        await act(async () => Array.from(host.querySelectorAll('button')).find((b) => b.textContent === 'Add Comment')!.click());
        expect(host.textContent).toContain('first note');
        expect(host.textContent).toContain('List comments failed: 500');
        expect(host.textContent).not.toContain(NO_COMMENTS);
    });
});

describe('FileCommentsDialog focus', () => {
    it('starts on the message box and gives focus back on close', async () => {
        invoke.mockResolvedValue([]);
        const opener = document.createElement('button');
        document.body.append(opener); opener.focus();
        await render();
        expect(document.activeElement).toBe(host.querySelector('textarea'));
        await act(async () => root.render(null));
        expect(document.activeElement).toBe(opener);
        opener.remove();
    });

    it('wraps Tab and Shift+Tab inside the dialog and keeps Tab from the app shortcuts', async () => {
        invoke.mockResolvedValue([{ id: '1', message: 'first note' }]);
        const appShortcut = vi.fn();
        window.addEventListener('keydown', appShortcut);
        await render();
        const focusable = host.querySelectorAll<HTMLElement>('button:not([disabled]), textarea');
        const first = focusable[0];
        const last = focusable[focusable.length - 1];
        last.focus();
        expect(tab().defaultPrevented).toBe(true);
        expect(document.activeElement).toBe(first);
        tab(true);
        expect(document.activeElement).toBe(last);
        expect(appShortcut).not.toHaveBeenCalled();
        window.removeEventListener('keydown', appShortcut);
    });
});
