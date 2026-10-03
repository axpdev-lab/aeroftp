// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// @vitest-environment jsdom

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ObjectTierDialog } from './ObjectTierDialog';
import { S3TagsDialog } from './S3TagsDialog';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('../i18n', async () => {
    const { translations } = (await import('../i18n/locales/en.json')).default as { translations: Record<string, unknown> };
    const t = (key: string, params?: Record<string, string | number>) => {
        const value = key.split('.').reduce<unknown>((node, part) => (node as Record<string, unknown> | undefined)?.[part], translations);
        return typeof value === 'string' ? value.replace(/\{(\w+)\}/g, (_, name: string) => String(params?.[name] ?? name)) : key;
    };
    return { useTranslation: () => t };
});

let root: Root;
let host: HTMLDivElement;
const noop = () => undefined;
const click = async (element: Element) => { await act(async () => (element as HTMLElement).click()); };
const button = (label: string) => Array.from(host.querySelectorAll('button')).find((b) => b.textContent?.trim() === label) as HTMLButtonElement | undefined;
const tab = (target: Element, shiftKey = false) => {
    const event = new KeyboardEvent('keydown', { key: 'Tab', shiftKey, bubbles: true, cancelable: true });
    target.dispatchEvent(event);
    return event;
};
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    invoke.mockReset();
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

describe('S3 and Azure object dialogs', () => {
    const dialogs = [
        ['ObjectTierDialog', () => createElement(ObjectTierDialog, { mode: 's3-restore', path: '/a.bin', name: 'a.bin', onClose: noop, onDone: noop })],
        ['S3TagsDialog', () => createElement(S3TagsDialog, { path: '/a.bin', name: 'a.bin', onClose: noop, onSaved: noop })],
    ] as const;

    it.each(dialogs)('%s holds focus inside while open and gives it back on close', async (_, dialog) => {
        invoke.mockResolvedValue({ env: 'prod' });
        const opener = document.createElement('button');
        document.body.append(opener);
        opener.focus();
        const shortcut = vi.fn();
        window.addEventListener('keydown', shortcut);
        try {
            await act(async () => root.render(dialog()));
            const panel = host.querySelector('[role="dialog"]')!;
            expect(panel.contains(document.activeElement)).toBe(true);

            const focusable = Array.from(panel.querySelectorAll<HTMLElement>('button:not([disabled]), input, select'));
            const first = focusable[0];
            const last = focusable[focusable.length - 1];
            last.focus();
            expect(tab(last).defaultPrevented).toBe(true);
            expect(document.activeElement).toBe(first);
            tab(first, true);
            expect(document.activeElement).toBe(last);
            opener.focus();
            tab(opener);
            expect(panel.contains(document.activeElement)).toBe(true);
            // The file manager binds Tab to switching panels on window; it must
            // not see a Tab pressed inside the dialog.
            expect(shortcut).not.toHaveBeenCalled();

            first.focus();
            await act(async () => root.render(null));
            expect(document.activeElement).toBe(opener);
        } finally {
            window.removeEventListener('keydown', shortcut);
            opener.remove();
        }
    });

    it('reports the whole number of restore days that the backend receives', async () => {
        invoke.mockResolvedValue(undefined);
        const onDone = vi.fn();
        await act(async () => root.render(createElement(ObjectTierDialog, { mode: 's3-restore', path: '/a.bin', name: 'a.bin', onClose: noop, onDone })));
        const days = host.querySelector('input[type="number"]') as HTMLInputElement;
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(days, '7.5');
            days.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await click(button('Apply')!);
        expect(invoke).toHaveBeenCalledWith('s3_glacier_restore', { path: '/a.bin', days: 7, tier: 'Standard' });
        expect(onDone).toHaveBeenCalledWith('Restore requested: once ready, the copy stays readable for 7 days');
    });

    it('keeps Save disabled after the tags fail to load and offers a retry', async () => {
        invoke.mockRejectedValueOnce(new Error('network down'));
        await act(async () => root.render(createElement(S3TagsDialog, { path: '/a.bin', name: 'a.bin', onClose: noop, onSaved: noop })));
        expect(host.textContent).toContain('network down');
        // An empty editor here would save as "no tags" and delete the real ones.
        expect(button('Save')!.disabled).toBe(true);
        expect(host.querySelector('input')).toBeNull();

        invoke.mockResolvedValueOnce({ env: 'prod' });
        await click(button('Retry')!);
        expect(host.textContent).not.toContain('network down');
        expect((host.querySelector('input') as HTMLInputElement).value).toBe('env');
        expect(button('Save')!.disabled).toBe(false);
        expect(invoke).not.toHaveBeenCalledWith('s3_delete_object_tags', expect.anything());
    });
});
