// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
import { act, createElement, useRef, useState } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import english from '../i18n/locales/en.json';
import { GuardedCloseConfirm } from './GuardedCloseConfirm';
import { SharePermissionsDialog } from './SharePermissionsDialog';
import { useModalFocusTrap } from '../hooks/useModalFocusTrap';
import { useGuardedClose } from '../hooks/useGuardedClose';

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => undefined) }));
vi.mock('../i18n', () => ({
    useTranslation: () => (key: string) => key.split('.').reduce<unknown>(
        (value, part) => (value as Record<string, unknown>)?.[part], english.translations,
    ) as string,
}));
vi.mock('../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    mocks.invoke.mockReset().mockResolvedValue([]);
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
});

afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
    vi.clearAllMocks();
});

const render = async (el: React.ReactElement) => {
    await act(async () => root.render(el));
};
const flush = async () => {
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });
};
const key = (k: string, shiftKey = false) => act(async () => {
    document.body.dispatchEvent(new KeyboardEvent('keydown', { key: k, shiftKey, bubbles: true, cancelable: true }));
});
const confirmButtons = () => [...host.querySelectorAll<HTMLButtonElement>('[role="alertdialog"] button')];

// A host modal with a focus trap and a couple of controls, like the real
// dialogs that render GuardedCloseConfirm inside their panel.
function Host({ dirty, onClose }: { dirty: boolean; onClose: () => void }) {
    const panelRef = useRef<HTMLDivElement>(null);
    useModalFocusTrap(panelRef);
    const guarded = useGuardedClose({ guard: dirty ? 'dirty' : null, onClose });
    return (
        <div ref={panelRef}>
            <button data-testid="host-x" onClick={guarded.requestClose}>x</button>
            <input data-testid="host-input" />
            {guarded.confirmOpen && guarded.confirmKind && (
                <GuardedCloseConfirm
                    kind={guarded.confirmKind}
                    canAbort={guarded.canAbort}
                    onKeep={guarded.cancelConfirm}
                    onConfirm={guarded.confirmAndClose}
                />
            )}
        </div>
    );
}

const openConfirm = async (onClose: () => void) => {
    await render(createElement(Host, { dirty: true, onClose }));
    const x = host.querySelector<HTMLElement>('[data-testid="host-x"]')!;
    x.focus();
    await act(async () => { x.click(); });
    await flush();
    expect(host.querySelector('[role="alertdialog"]')).not.toBeNull();
    return x;
};

describe('GuardedCloseConfirm keyboard ownership (Discussion #1527 review pass 2)', () => {
    it('moves focus to the safe action when it opens', async () => {
        await openConfirm(vi.fn());
        const keep = confirmButtons()[0];
        expect(document.activeElement).toBe(keep);
        expect(keep.textContent).toContain('Keep editing');
    });

    it('cycles Tab and Shift+Tab only between its two buttons, never reaching the host', async () => {
        await openConfirm(vi.fn());
        const [keep, discard] = confirmButtons();
        const hostInput = host.querySelector<HTMLElement>('[data-testid="host-input"]')!;
        for (let i = 0; i < 4; i++) {
            await key('Tab');
            expect([keep, discard]).toContain(document.activeElement);
            expect(document.activeElement).not.toBe(hostInput);
        }
        expect(document.activeElement).toBe(keep);
        await key('Tab', true);
        expect(document.activeElement).toBe(discard);
        await key('Tab', true);
        expect(document.activeElement).toBe(keep);
    });

    it('returns focus to the element that had it after Keep', async () => {
        const onClose = vi.fn();
        const x = await openConfirm(onClose);
        await act(async () => { confirmButtons()[0].click(); });
        await flush();
        expect(host.querySelector('[role="alertdialog"]')).toBeNull();
        expect(document.activeElement).toBe(x);
        expect(onClose).not.toHaveBeenCalled();
    });

    it('SharePermissionsDialog: Tab and Enter on the confirm never reach the email draft action', async () => {
        const onClose = vi.fn();
        mocks.invoke.mockImplementation(async (cmd: string) => (cmd === 'provider_list_permissions' ? [] : null));
        await render(createElement(SharePermissionsDialog, { filePath: '/f.txt', fileName: 'f.txt', onClose }));
        await flush();
        const email = host.querySelector<HTMLInputElement>('input[type="email"]')!;
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(email, 'friend@example.com');
            email.dispatchEvent(new Event('input', { bubbles: true }));
        });
        const x = [...host.querySelectorAll('button')].find(b => b.querySelector('svg.lucide-x'))!;
        await act(async () => { x.click(); });
        await flush();
        expect(host.querySelector('[role="alertdialog"]')).not.toBeNull();
        // Several Tabs must stay inside the confirm; a real Enter keydown on
        // the focused confirm control must not reach the host's add action.
        for (let i = 0; i < 3; i++) await key('Tab');
        const focused = document.activeElement as HTMLElement;
        expect(confirmButtons()).toContain(focused);
        await act(async () => {
            focused.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
        });
        await flush();
        expect(mocks.invoke).not.toHaveBeenCalledWith('provider_add_permission', expect.anything());
        // jsdom does not synthesize click from Enter; click Keep to close.
        await act(async () => { confirmButtons()[0].click(); });
        await flush();
        expect(host.querySelector('[role="alertdialog"]')).toBeNull();
        expect(mocks.invoke).not.toHaveBeenCalledWith('provider_add_permission', expect.anything());
        expect(email.value).toBe('friend@example.com');
        expect(onClose).not.toHaveBeenCalled();
    });
});
