// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { GitHubReleaseBrowser } from './GitHubReleaseBrowser';

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('../utils/pickPath', () => ({ pickFile: vi.fn(async () => null), pickSave: vi.fn(async () => null) }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));
vi.mock('../hooks/useHumanizedLog', () => ({
    useHumanizedLog: () => ({ logRaw: () => 'log-1', updateEntry: () => undefined }),
}));

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    mocks.invoke.mockReset().mockImplementation(async (cmd: string) => {
        if (cmd === 'github_list_releases') return { releases: [], count: 0 };
        return null;
    });
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
});

afterEach(() => {
    act(() => root.unmount());
    host.remove();
    vi.clearAllMocks();
});

const flush = () => act(async () => { await new Promise(resolve => setTimeout(resolve, 0)); });
const render = async (node: React.ReactNode) => {
    await act(async () => root.render(<>{node}</>));
    await flush();
};
const buttons = () => [...host.querySelectorAll('button')];
const press = (el: HTMLElement) => act(async () => { el.click(); });
const click = (label: string) => {
    const found = buttons().find(b => b.textContent?.trim() === label || b.getAttribute('title') === label);
    if (!found) throw new Error(`no button "${label}"`);
    return press(found);
};
const typeInto = (el: HTMLTextAreaElement | HTMLInputElement, value: string) => act(async () => {
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value')!.set!.call(el, value);
    el.dispatchEvent(new Event('input', { bubbles: true }));
});
const confirmOpen = () => host.querySelector('[role="alertdialog"]') !== null;
const backButton = () => {
    const found = buttons().find(b => b.innerHTML.includes('lucide-arrow-left'));
    if (!found) throw new Error('no Back button');
    return found;
};
const descriptionInput = () => host.querySelector('textarea') as HTMLTextAreaElement;

const openCreateForm = async (onClose: () => void) => {
    await render(<GitHubReleaseBrowser isOpen onClose={onClose} />);
    await click('github.createRelease');
    expect(descriptionInput()).not.toBeNull();
};

describe('GitHubReleaseBrowser Back guard (Discussion #1527 review)', () => {
    it('asks before Back drops a typed release draft, and Keep retains it', async () => {
        const onClose = vi.fn();
        await openCreateForm(onClose);
        await typeInto(descriptionInput(), 'sentinel release notes');
        await press(backButton());
        expect(confirmOpen()).toBe(true);
        expect(onClose).not.toHaveBeenCalled();
        expect(descriptionInput()).not.toBeNull();
        await click('modalGuard.keepEditing');
        expect(confirmOpen()).toBe(false);
        expect(descriptionInput().value).toBe('sentinel release notes');
    });

    it('Back with a dirty draft confirms, then returns to the list; a new release starts empty', async () => {
        const onClose = vi.fn();
        await openCreateForm(onClose);
        await typeInto(descriptionInput(), 'sentinel release notes');
        await press(backButton());
        expect(confirmOpen()).toBe(true);
        await click('modalGuard.discardAndClose');
        expect(confirmOpen()).toBe(false);
        expect(descriptionInput()).toBeNull();
        await click('github.createRelease');
        expect(descriptionInput().value).toBe('');
    });

    it('Back with an empty form returns to the list without asking', async () => {
        const onClose = vi.fn();
        await openCreateForm(onClose);
        await press(backButton());
        expect(confirmOpen()).toBe(false);
        expect(descriptionInput()).toBeNull();
    });

    it('keeps Back inert while a release create is in flight', async () => {
        let resolveCreate!: () => void;
        mocks.invoke.mockImplementation(async (cmd: string) => {
            if (cmd === 'github_list_releases') return { releases: [], count: 0 };
            if (cmd === 'github_create_release') return new Promise<null>(res => { resolveCreate = () => res(null); });
            return null;
        });
        const onClose = vi.fn();
        await openCreateForm(onClose);
        await typeInto(host.querySelector('input')!, 'v9.9.9');
        await typeInto(descriptionInput(), 'sentinel release notes');
        await click('github.createRelease');
        await press(backButton());
        expect(confirmOpen()).toBe(false);
        expect(descriptionInput()).not.toBeNull();
        expect(onClose).not.toHaveBeenCalled();
        await act(async () => { resolveCreate(); });
        await flush();
    });
});
