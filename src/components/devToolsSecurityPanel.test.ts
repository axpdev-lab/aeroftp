// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// @vitest-environment jsdom
//
// AeroTools > Security Tools is a panel, like Editor, Terminal and AI Agent
// (#347, Ehud): the launcher toggles a column inside AeroTools instead of
// opening a window over the app, so the app stays usable while the tools are
// open and the GUI window can still be moved, maximised or snapped.

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import APP from '../App.tsx?raw';

const invoke = vi.hoisted(() => vi.fn(async () => undefined));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('@tauri-apps/api/webview', () => ({
    getCurrentWebview: () => ({ onDragDropEvent: async () => () => {} }),
}));
vi.mock('@tauri-apps/plugin-fs', () => ({ readTextFile: vi.fn() }));
vi.mock('../utils/pickPath', () => ({ pickFile: vi.fn() }));
vi.mock('./DevTools/CodeEditor', () => ({ CodeEditor: () => createElement('div', { 'data-column': 'editor' }) }));
vi.mock('./DevTools/SSHTerminal', () => ({ SSHTerminal: () => createElement('div', { 'data-column': 'terminal' }) }));
vi.mock('./DevTools/AIChat', () => ({ AIChat: () => createElement('div', { 'data-column': 'chat' }) }));
vi.mock('../i18n', async () => {
    const { translations } = (await import('../i18n/locales/en.json')).default as { translations: Record<string, unknown> };
    const t = (key: string) => {
        const value = key.split('.').reduce<unknown>((node, part) => (node as Record<string, unknown> | undefined)?.[part], translations);
        return typeof value === 'string' ? value : key;
    };
    return { useTranslation: () => t, translate: t };
});

import { DevToolsV2 } from './DevTools/DevToolsV2';

type Props = React.ComponentProps<typeof DevToolsV2>;

let root: Root;
let host: HTMLDivElement;
const modalOpener = vi.fn();

/** Before #347 the launcher called this to open the window. It is still
 *  handed in, untyped, so the test can see that nothing calls it. */
const render = async (overrides: Partial<Props> = {}) => {
    const props = { isOpen: true, previewFile: null, onClose: () => {}, onShowCyberTools: modalOpener, ...overrides };
    await act(async () => root.render(createElement(DevToolsV2, props as unknown as Props)));
    await act(async () => { await Promise.resolve(); });
};

const launcher = (label: string) => {
    const found = Array.from(host.querySelectorAll('button')).find(b => b.textContent === label);
    expect(found, `launcher "${label}"`).toBeTruthy();
    return found as HTMLButtonElement;
};
const click = async (el: HTMLElement) => { await act(async () => el.click()); };
/** Active launchers are filled (`text-white`); inactive ones only hover to it. */
const isActive = (el: HTMLElement) => el.classList.contains('text-white');
const securityColumn = () => host.querySelector('[data-native-drop-owner]')?.closest('[style]') as HTMLElement | null;

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    modalOpener.mockClear();
    vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} });
    // The narrowest AeroTools there is: the main window's minimum width.
    vi.stubGlobal('innerWidth', 1024);
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
    vi.unstubAllGlobals();
});

describe('AeroTools Security Tools panel', () => {
    it('toggles a Security Tools column from the launcher and opens no window', async () => {
        await render();
        expect(host.querySelector('[data-native-drop-owner]')).toBeNull();

        await click(launcher('Security Tools'));
        expect(modalOpener, 'the launcher must not open the Security Tools window').not.toHaveBeenCalled();
        const panel = host.querySelector('[data-native-drop-owner]');
        expect(panel, 'the Security Tools panel is shown inside AeroTools').toBeTruthy();
        expect(panel!.textContent).toContain('Hash Forge');
        expect(host.querySelector('.fixed'), 'no overlay inside AeroTools').toBeNull();
        expect(isActive(launcher('Security Tools')), 'the launcher shows the active style').toBe(true);

        await click(launcher('Security Tools'));
        expect(host.querySelector('[data-native-drop-owner]')).toBeNull();
        expect(isActive(launcher('Security Tools'))).toBe(false);
        expect(modalOpener).not.toHaveBeenCalled();
    });

    it('fits all four panels side by side at the minimum window width', async () => {
        await render();
        await click(launcher('Terminal'));
        await click(launcher('AI Agent'));
        await click(launcher('Security Tools'));

        const shown = ['editor', 'terminal', 'chat'].filter(c => {
            const column = host.querySelector(`[data-column="${c}"]`)?.closest('[style]') as HTMLElement | null;
            return column && !column.classList.contains('hidden');
        });
        expect(shown).toEqual(['editor', 'terminal', 'chat']);
        const security = securityColumn();
        expect(security, 'Security Tools is the fourth column').toBeTruthy();
        expect(security!.style.width).toBe('25%');
        // Every visible column but the last is followed by a resize handle.
        expect(host.querySelectorAll('.cursor-col-resize')).toHaveLength(3);
    });

    it('starts fresh each time AeroTools is reopened, like the window did', async () => {
        // The tools hold secrets (a BLAKE3 key, a Crypto Lab password) and a
        // staged temp copy of a dropped file; closing the window dropped them.
        // AeroTools only hides itself when closed, so the panel must unmount.
        await render();
        await click(launcher('Security Tools'));
        expect(host.querySelector('[data-native-drop-owner]')).toBeTruthy();
        await render({ isOpen: false });
        expect(host.querySelector('[data-native-drop-owner]')).toBeNull();
        await render({ isOpen: true });
        expect(host.querySelector('[data-native-drop-owner]'), 'the toggle survives a close').toBeTruthy();
    });

    it('steps aside for a solo Editor, Terminal or Agent request', async () => {
        await render();
        await click(launcher('Security Tools'));
        expect(host.querySelector('[data-native-drop-owner]')).toBeTruthy();
        await act(async () => { window.dispatchEvent(new CustomEvent('devtools-panel-solo', { detail: 'terminal' })); });
        expect(host.querySelector('[data-native-drop-owner]')).toBeNull();
        expect(isActive(launcher('Security Tools'))).toBe(false);
    });
});

describe('App wiring', () => {
    it('does not import a drop that lands on the Security Tools panel into the local folder', () => {
        const start = APP.indexOf('webview.onDragDropEvent(async (event) => {');
        expect(start).toBeGreaterThan(-1);
        const handler = APP.slice(start, APP.indexOf("invoke('copy_local_file'", start));
        expect(handler).toContain('if (nativeDropOwnerAt(event.payload.position)) return;');
    });
});
