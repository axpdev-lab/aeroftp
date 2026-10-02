// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { BoxCollaboratorsDialog } from './BoxCollaboratorsDialog';

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

const NOBODY = 'Not shared with anyone yet';
const ana = { id: 'k1', role: 'editor', accessible_by: { name: 'Ana', login: 'ana@x' } };

let root: Root;
let host: HTMLDivElement;
const render = async () => {
    await act(async () => root.render(createElement(BoxCollaboratorsDialog, { path: '/d', name: 'd', onClose: () => {} })));
};
const input = async (element: HTMLInputElement, value: string) => {
    await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(element, value);
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

describe('BoxCollaboratorsDialog load failures', () => {
    it('shows a failed first load as an error, not as an empty share list', async () => {
        invoke.mockRejectedValue('List collaborations failed: 403');
        await render();
        expect(host.textContent).toContain('List collaborations failed: 403');
        expect(host.textContent).not.toContain(NOBODY);
    });

    it('keeps the list on screen when the reload after an invite fails', async () => {
        let lists = 0;
        invoke.mockImplementation(async (command: string) => {
            if (command !== 'box_list_collaborations') return undefined;
            if (++lists > 1) throw 'List collaborations failed: 500';
            return [ana];
        });
        await render();
        await input(host.querySelector('input[type=email]')!, 'bo@x');
        await act(async () => Array.from(host.querySelectorAll('button')).find((b) => b.textContent === 'Add Collaborator')!.click());
        expect(host.textContent).toContain('Ana');
        expect(host.textContent).toContain('List collaborations failed: 500');
        expect(host.textContent).not.toContain(NOBODY);
    });
});

describe('BoxCollaboratorsDialog focus', () => {
    it('moves focus in on open and gives it back on close', async () => {
        invoke.mockResolvedValue([]);
        const opener = document.createElement('button');
        document.body.append(opener); opener.focus();
        await render();
        expect(document.activeElement).toBe(host.querySelector('input[type=email]'));
        await act(async () => root.render(null));
        expect(document.activeElement).toBe(opener);
        opener.remove();
    });

    it('wraps Tab and Shift+Tab inside the dialog and keeps Tab from the app shortcuts', async () => {
        invoke.mockResolvedValue([ana]);
        const appShortcut = vi.fn();
        window.addEventListener('keydown', appShortcut);
        await render();
        const focusable = host.querySelectorAll<HTMLElement>('button:not([disabled]), input, select');
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
