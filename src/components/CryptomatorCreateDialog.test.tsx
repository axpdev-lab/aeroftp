// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import english from '../i18n/locales/en.json';
import CryptomatorCreateDialog from './CryptomatorCreateDialog';
import { GuardedCloseConfirm } from './GuardedCloseConfirm';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => ({})) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => undefined) }));
vi.mock('../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));
vi.mock('../i18n', () => ({
    useTranslation: () => (key: string) => key.split('.').reduce<unknown>(
        (value, part) => (value as Record<string, unknown>)?.[part], english.translations,
    ) as string,
}));

let root: Root;
let host: HTMLDivElement;

const render = async (el: React.ReactElement) => {
    await act(async () => root.render(el));
};
const keydown = async (target: Element | Window, key: string) => {
    await act(async () => {
        target.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));
    });
};
const typeInto = async (input: HTMLInputElement, value: string) => {
    await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value);
        input.dispatchEvent(new Event('input', { bubbles: true }));
    });
};
const alertButtons = () => [...host.querySelectorAll<HTMLButtonElement>('[role=alertdialog] button')];

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    vi.mocked(invoke).mockClear();
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

it('keeps Enter on the confirm buttons from reaching a host onKeyDown', async () => {
    const onHostKey = vi.fn();
    const onKeep = vi.fn();
    const onConfirm = vi.fn();
    await render(createElement('div', { onKeyDown: onHostKey },
        createElement(GuardedCloseConfirm, { kind: 'dirty', onKeep, onConfirm })));
    for (const button of alertButtons()) {
        await keydown(button, 'Enter');
        expect(onHostKey).not.toHaveBeenCalled();
    }
    // The buttons themselves still answer a plain click.
    await act(async () => alertButtons()[0].click());
    expect(onKeep).toHaveBeenCalledTimes(1);
});

it('Enter on either Cryptomator confirm action never reaches handleCreate', async () => {
    await render(createElement(CryptomatorCreateDialog, { outputDir: '/vaults', onClose: vi.fn() }));
    const inputs = host.querySelectorAll<HTMLInputElement>('input[type=password]');
    await typeInto(inputs[0], 'correct horse');
    await typeInto(inputs[1], 'correct horse');
    // Open the dirty-close confirmation with ESC (the dialog routes it
    // through its own onKeyDown, so the press must land inside it).
    await keydown(inputs[0], 'Escape');
    expect(host.querySelector('[role=alertdialog]')).not.toBeNull();
    for (const button of alertButtons()) {
        await keydown(button, 'Enter');
    }
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'cryptomator_create')).toHaveLength(0);
    // And a click on "keep editing" still just dismisses the confirm.
    await act(async () => alertButtons()[0].click());
    expect(host.querySelector('[role=alertdialog]')).toBeNull();
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'cryptomator_create')).toHaveLength(0);
});
