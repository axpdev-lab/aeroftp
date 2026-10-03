// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// @vitest-environment jsdom
//
// CyberToolsPanel is the Security Tools body (tab bar and the three tools)
// without the window around it, so AeroTools can show it as a column next to
// the editor and the terminal (#347, Ehud): the app stays usable, and the GUI
// window can be moved or snapped, while the tools are open.
//
// The part that changes meaning outside a window is the OS file drop. Tauri
// delivers it to the whole webview with a position and no target element. The
// window covers the app, so every drop was for Hash Forge; a column does not,
// so a drop on AeroFile next to it must stay an AeroFile drop, and a drop on
// the column must not also be imported into the local folder.

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const invoke = vi.hoisted(() => vi.fn(async () => undefined));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
type DropHandler = (event: { payload: Record<string, unknown> }) => void;
const dropHandlers = vi.hoisted(() => new Set<(event: { payload: Record<string, unknown> }) => void>());
vi.mock('@tauri-apps/api/webview', () => ({
    getCurrentWebview: () => ({
        onDragDropEvent: async (handler: DropHandler) => {
            dropHandlers.add(handler);
            return () => { dropHandlers.delete(handler); };
        },
    }),
}));
vi.mock('../utils/pickPath', () => ({ pickFile: vi.fn() }));
vi.mock('../i18n', async () => {
    const { translations } = (await import('../i18n/locales/en.json')).default as { translations: Record<string, unknown> };
    const t = (key: string) => {
        const value = key.split('.').reduce<unknown>((node, part) => (node as Record<string, unknown> | undefined)?.[part], translations);
        return typeof value === 'string' ? value : key;
    };
    return { useTranslation: () => t, translate: t };
});

import { CyberToolsPanel } from './CyberToolsPanel';
import { nativeDropOwnerAt } from '../utils/nativeDropOwner';

let root: Root;
let host: HTMLDivElement;
let elementAtPoint: Element | null = null;
const pointsAsked: Array<[number, number]> = [];

const render = async (nativeDropScope: 'window' | 'panel') => {
    await act(async () => root.render(createElement('div', { className: 'flex flex-col h-96' },
        createElement('div', { id: 'aerofile' }, 'local panel'),
        createElement(CyberToolsPanel, { nativeDropScope }),
    )));
    await act(async () => { await Promise.resolve(); });
};

const nativeDrag = async (type: 'enter' | 'over' | 'drop' | 'leave', target: Element | null, paths: string[] = []) => {
    elementAtPoint = target;
    await act(async () => {
        dropHandlers.forEach(h => h({ payload: { type, paths, position: { x: 200, y: 120 } } }));
    });
};

const panelRoot = () => host.querySelector('[data-native-drop-owner]') as HTMLElement;
const hashFilePath = () => host.querySelector<HTMLInputElement>('input[readonly]')?.value ?? null;
const tabButton = (label: string) => Array.from(host.querySelectorAll('button')).find(b => b.textContent === label) as HTMLButtonElement;

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    invoke.mockClear();
    dropHandlers.clear();
    pointsAsked.length = 0;
    elementAtPoint = null;
    // jsdom has no layout, so it has no elementFromPoint either.
    document.elementFromPoint = (x: number, y: number) => { pointsAsked.push([x, y]); return elementAtPoint; };
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
    vi.unstubAllGlobals();
});

describe('CyberToolsPanel', () => {
    it('renders the three tools without an overlay or fixed positioning, filling its container', async () => {
        await render('panel');
        const panel = panelRoot();
        expect(panel).toBeTruthy();
        expect(host.querySelector('.fixed'), 'no fixed overlay: the panel lives in the layout').toBeNull();
        expect(host.querySelector('[class*="inset-0"].fixed, [class*="bg-black"]')).toBeNull();
        // Fills a flex-column parent and scrolls its own content.
        expect(panel.className).toContain('flex-1');
        expect(panel.className).toContain('min-h-0');
        expect(panel.querySelector('.overflow-y-auto')).toBeTruthy();
        for (const label of ['Hash Forge', 'CryptoLab', 'Password Forge']) {
            expect(tabButton(label), label).toBeTruthy();
        }
    });

    it('switches tabs', async () => {
        await render('panel');
        expect(host.textContent).toContain('MD5');
        await act(async () => tabButton('CryptoLab').click());
        expect(host.textContent).toContain('AES-256-GCM');
        expect(host.textContent).not.toContain('MD5');
        await act(async () => tabButton('Password Forge').click());
        expect(host.textContent).not.toContain('AES-256-GCM');
        await act(async () => tabButton('Hash Forge').click());
        expect(host.textContent).toContain('MD5');
    });

    it('claims the keyboard and can take focus, so a click inside keeps keys inside', async () => {
        await render('panel');
        expect(panelRoot().hasAttribute('data-keyboard-island')).toBe(true);
        // The attribute, not the `tabIndex` property: a plain div already
        // reports -1 there without being focusable.
        expect(panelRoot().getAttribute('tabindex'), 'a click inside must focus the panel').toBe('-1');
    });
});

describe('OS file drops when the panel sits next to other surfaces', () => {
    it('takes a drop that lands on the panel', async () => {
        await render('panel');
        await nativeDrag('drop', panelRoot().querySelector('p'), ['/home/u/release.tar.gz']);
        expect(hashFilePath()).toBe('/home/u/release.tar.gz');
    });

    it('ignores a drop that lands on AeroFile next to it', async () => {
        await render('panel');
        await nativeDrag('drop', host.querySelector('#aerofile'), ['/home/u/photo.jpg']);
        expect(hashFilePath(), 'Hash Forge took a drop aimed at AeroFile').toBeNull();
    });

    it('highlights only while the drag is over the panel', async () => {
        await render('panel');
        const hint = () => host.querySelector('.border-dashed');
        await nativeDrag('over', host.querySelector('#aerofile'));
        expect(hint(), 'drop hint shown while dragging over AeroFile').toBeNull();
        await nativeDrag('over', panelRoot().querySelector('p'));
        expect(hint()).toBeTruthy();
        await nativeDrag('over', host.querySelector('#aerofile'));
        expect(hint()).toBeNull();
    });

    it('in the window, still takes every drop (it covers the app)', async () => {
        await render('window');
        await nativeDrag('drop', null, ['/home/u/any.bin']);
        expect(hashFilePath()).toBe('/home/u/any.bin');
    });
});

describe('nativeDropOwnerAt', () => {
    it('hit-tests the drop position as CSS pixels, unscaled, also on a HiDPI screen', async () => {
        // Typed PhysicalPosition, but GTK (Linux) and AppKit (macOS) report
        // logical coordinates and Tauri passes them through unscaled; Windows,
        // the device-pixel backend, never fires this event in AeroFTP.
        vi.stubGlobal('devicePixelRatio', 2);
        await render('panel');
        elementAtPoint = panelRoot().querySelector('p');
        expect(nativeDropOwnerAt({ x: 200, y: 120 })).toBe(panelRoot());
        expect(pointsAsked[pointsAsked.length - 1], 'a scaled hit test misses on HiDPI').toEqual([200, 120]);
    });

    it('finds no owner over a surface that does not claim drops', async () => {
        await render('panel');
        elementAtPoint = host.querySelector('#aerofile');
        expect(nativeDropOwnerAt({ x: 10, y: 10 })).toBeNull();
        elementAtPoint = null;
        expect(nativeDropOwnerAt({ x: 10, y: 10 })).toBeNull();
    });
});
