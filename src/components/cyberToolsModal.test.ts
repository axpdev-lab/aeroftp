// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// @vitest-environment jsdom
//
// Security Tools has two homes since #347 (Ehud): a panel inside AeroTools,
// next to the editor and the terminal, and the floating window that the
// Cyber-theme titlebar button still opens. This file guards the window: it is
// the same overlay shell as before, it hosts the shared panel body, and the
// titlebar is the only thing in App that still opens it.
//
// It also guards the keyboard. The app-wide shortcut handler skips inputs, but
// a focused button is not an input, so before the panel claimed its keys a Tab
// on a Hash Forge pill switched the file panels behind the window (and was
// swallowed, so focus never moved) and a Delete asked to delete the selected
// files.

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import TITLEBAR from './CustomTitlebar.tsx?raw';
import MODAL from './CyberToolsModal.tsx?raw';
import APP from '../App.tsx?raw';

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

import { CyberToolsModal } from './CyberToolsModal';
import { useKeyboardShortcuts } from '../hooks/useKeyboardShortcuts';

let root: Root;
let host: HTMLDivElement;
const onClose = vi.fn();
const shortcut = { Tab: vi.fn(), Delete: vi.fn(), Escape: vi.fn() };

/** The app-wide shortcut hook, mounted the way App mounts it. */
const AppShortcuts = () => {
    useKeyboardShortcuts(shortcut);
    return null;
};

const render = async () => {
    await act(async () => root.render(createElement('div', null,
        createElement(AppShortcuts),
        createElement('button', { id: 'outside' }, 'file panel'),
        createElement(CyberToolsModal, { onClose }),
    )));
    // The Hash Forge drop listener registers after an awaited promise.
    await act(async () => { await Promise.resolve(); });
};

const press = (target: Element, key: string): KeyboardEvent => {
    const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true });
    (target as HTMLElement).focus();
    target.dispatchEvent(event);
    return event;
};

const buttonByText = (text: string): HTMLButtonElement => {
    const found = Array.from(host.querySelectorAll('button')).find(b => b.textContent?.includes(text));
    expect(found, `button "${text}"`).toBeTruthy();
    return found as HTMLButtonElement;
};

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    invoke.mockClear();
    onClose.mockClear();
    Object.values(shortcut).forEach(fn => fn.mockClear());
    dropHandlers.clear();
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
});

describe('Security Tools window (titlebar easter egg)', () => {
    it('is the fixed overlay shell around the shared panel body', async () => {
        await render();
        const overlay = host.querySelector('.fixed.inset-0');
        expect(overlay, 'the window keeps its full-screen overlay').toBeTruthy();
        const panel = overlay!.querySelector('[data-native-drop-owner]');
        expect(panel, 'the window hosts CyberToolsPanel, which marks its root').toBeTruthy();
        expect(panel!.textContent).toContain('Hash Forge');
        expect(panel!.textContent).toContain('CryptoLab');
        expect(panel!.textContent).toContain('Password Forge');
        expect(MODAL).toMatch(/<CyberToolsPanel\s+nativeDropScope="window"\s*\/>/);
    });

    it('still closes on Escape', async () => {
        await render();
        press(document.body, 'Escape');
        expect(onClose).toHaveBeenCalled();
    });

    it('still takes an OS drop wherever it lands, because it covers the window', async () => {
        // Guard, not a regression test: this is the behaviour from before the
        // panel existed, and it must survive the move. jsdom has no
        // elementFromPoint, so a window-scoped listener that started
        // hit-testing would throw here instead of taking the drop.
        await render();
        expect(dropHandlers.size).toBe(1);
        await act(async () => {
            dropHandlers.forEach(h => h({ payload: { type: 'drop', paths: ['/home/u/file.iso'], position: { x: 1, y: 1 } } }));
        });
        const path = host.querySelector<HTMLInputElement>('input[readonly]');
        expect(path?.value).toBe('/home/u/file.iso');
    });
});

describe('keys pressed inside Security Tools stay inside it', () => {
    it('does not hand Tab or Delete on a focused button to the file-manager shortcuts', async () => {
        await render();
        const pill = buttonByText('SHA-256');
        const tab = press(pill, 'Tab');
        expect(shortcut.Tab, 'Tab on a Hash Forge pill switched the file panels').not.toHaveBeenCalled();
        expect(tab.defaultPrevented, 'Tab must move focus natively').toBe(false);
        press(pill, 'Delete');
        expect(shortcut.Delete, 'Delete on a Hash Forge pill reached the file delete').not.toHaveBeenCalled();
    });

    it('lets Escape through, as inputs do', async () => {
        await render();
        press(buttonByText('SHA-256'), 'Escape');
        expect(shortcut.Escape).toHaveBeenCalled();
    });

    it('leaves the shortcuts working outside the panel (positive control)', async () => {
        await render();
        press(host.querySelector('#outside')!, 'Tab');
        expect(shortcut.Tab).toHaveBeenCalledTimes(1);
    });
});

describe('who opens the window', () => {
    it('the Cyber-theme titlebar button opens it', () => {
        const button = TITLEBAR.slice(TITLEBAR.indexOf("{appTheme === 'cyber' && ("));
        expect(button.slice(0, 300)).toContain('onClick={onShowCyberTools}');
        expect(APP).toMatch(/onShowCyberTools=\{\(\) => setShowCyberTools\(true\)\}/);
        expect(APP).toContain('{showCyberTools && <CyberToolsModal onClose={() => setShowCyberTools(false)} />}');
    });

    it('nothing else in App opens it: AeroTools shows the panel instead', () => {
        // Before #347 the AeroTools launcher was wired to the same opener, so
        // App carried two `onShowCyberTools=` props.
        expect(APP.match(/onShowCyberTools=/g)).toHaveLength(1);
    });
});
