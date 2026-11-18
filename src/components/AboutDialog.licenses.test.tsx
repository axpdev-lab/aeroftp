// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({ openThirdPartyLicenses: vi.fn() }));

vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn(async () => '1.0.0') }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => new Promise(() => undefined)) }));
vi.mock('@tauri-apps/plugin-shell', () => ({ open: vi.fn(async () => undefined) }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));
vi.mock('../utils/thirdPartyLicenses', () => ({
    GITHUB_NOTICES_URL: 'https://example.invalid/THIRD-PARTY-NOTICES.md',
    openThirdPartyLicenses: mocks.openThirdPartyLicenses,
}));

import { AboutDialog } from './AboutDialog';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

// An open attempt whose result the test decides.
const deferred = () => {
    let resolve!: (r: { success: boolean; target: string }) => void;
    const promise = new Promise<{ success: boolean; target: string }>((r) => { resolve = r; });
    return { promise, finish: (success: boolean) => resolve({ success, target: 'x' }) };
};

describe('AboutDialog Third-Party Licenses', () => {
    let container: HTMLDivElement;
    let root: Root;
    const render = (isOpen: boolean) => act(() => root.render(<AboutDialog isOpen={isOpen} onClose={() => undefined} />));
    const button = () => [...container.querySelectorAll('button')].find((b) => b.textContent === 'about.thirdPartyLicenses')!;
    const click = () => act(() => { button().click(); });
    const alertShown = () => container.querySelector('[role="alert"]') !== null;

    beforeEach(() => {
        container = document.createElement('div');
        document.body.appendChild(container);
        root = createRoot(container);
        mocks.openThirdPartyLicenses.mockReset();
    });
    afterEach(() => {
        act(() => root.unmount());
        container.remove();
    });

    it('the button runs the opener, and a failed attempt shows the alert', async () => {
        const attempt = deferred();
        mocks.openThirdPartyLicenses.mockReturnValueOnce(attempt.promise);
        await render(true);
        await click();
        expect(mocks.openThirdPartyLicenses).toHaveBeenCalledTimes(1);
        await act(async () => attempt.finish(false));
        expect(alertShown()).toBe(true);
    });

    it('an earlier attempt that fails after a later one opened the notices shows no alert', async () => {
        const first = deferred();
        const second = deferred();
        mocks.openThirdPartyLicenses.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
        await render(true);
        await click();
        await click();
        await act(async () => second.finish(true));
        await act(async () => first.finish(false));
        expect(alertShown()).toBe(false);
    });

    it('closing clears the alert, and an attempt that ends after the close is ignored', async () => {
        const failed = deferred();
        const pending = deferred();
        mocks.openThirdPartyLicenses.mockReturnValueOnce(failed.promise).mockReturnValueOnce(pending.promise);
        await render(true);
        await click();
        await act(async () => failed.finish(false));
        expect(alertShown()).toBe(true);
        await render(false);
        await render(true);
        expect(alertShown()).toBe(false);
        await click();
        await render(false);
        await render(true);
        await act(async () => pending.finish(false));
        expect(alertShown()).toBe(false);
    });
});
