// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { CopyableFormFields } from './CopyableFormFields';

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), t: (key: string) => key }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('../../i18n', () => ({ useTranslation: () => mocks.t }));

let container: HTMLDivElement;
let root: Root;
beforeEach(() => {
    vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
    mocks.invoke.mockReset().mockResolvedValue(undefined);
    container = document.createElement('div');
    document.body.append(container);
    root = createRoot(container);
});
afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
    vi.unstubAllGlobals();
});

async function render(type = 'text', value = 'account', enabled = true, disabled = false) {
    await act(async () => root.render(React.createElement(CopyableFormFields, {
        enabled, className: 'relative',
        children: React.createElement('input', { type, value, readOnly: true, disabled }),
    })));
    return container.querySelector('input')!;
}
const copyButton = () => container.querySelector<HTMLButtonElement>('button[aria-label="common.copy"]');
async function hover(input: HTMLInputElement) {
    await act(async () => input.dispatchEvent(new MouseEvent('pointerover', { bubbles: true })));
}

describe('copying QuickConnect fields', () => {
    it('appears on hover and copies the latest whole value through the native clipboard', async () => {
        const input = await render();
        expect(copyButton()).toBeNull();
        await hover(input);
        expect(copyButton()).not.toBeNull();
        await render('text', 'long-access-key-with-no-selection');
        await act(async () => copyButton()!.click());
        expect(mocks.invoke).toHaveBeenCalledWith('copy_to_clipboard', { text: 'long-access-key-with-no-selection' });
        expect(copyButton()!.title).toBe('common.copied');
        expect(copyButton()!.outerHTML).not.toContain('long-access-key');
    });

    it('only offers a password after reveal and removes the button as soon as it is hidden again', async () => {
        const input = await render('password', 'secret-fixture');
        await hover(input);
        expect(copyButton()).toBeNull();
        await render('text', 'secret-fixture');
        await hover(input);
        expect(copyButton()).not.toBeNull();
        await act(async () => copyButton()!.click());
        expect(mocks.invoke).toHaveBeenCalledWith('copy_to_clipboard', { text: 'secret-fixture' });
        await render('password', 'secret-fixture');
        expect(copyButton()).toBeNull();
    });

    it.each(['password', 'number', 'checkbox', 'file'])('does not offer copying for %s inputs', async type => {
        const input = await render(type, type === 'file' ? '' : '1');
        await hover(input);
        expect(copyButton()).toBeNull();
    });

    it('removes the affordance on empty values or inactive tabs', async () => {
        const input = await render();
        await hover(input);
        await render('text', '');
        expect(copyButton()).toBeNull();
        await render();
        await hover(input);
        await render('text', 'account', false);
        expect(copyButton()).toBeNull();
        expect(mocks.invoke).not.toHaveBeenCalled();
    });

    it('offers the same action to keyboard users and dismisses it on leaving the field', async () => {
        const input = await render();
        await act(async () => input.focus());
        expect(copyButton()).not.toBeNull();
        await act(async () => input.blur());
        expect(copyButton()).toBeNull();
    });

    it('does not show copied feedback when clipboard access fails', async () => {
        mocks.invoke.mockRejectedValue(new Error('Clipboard unavailable'));
        const input = await render();
        await hover(input);
        await act(async () => copyButton()!.click());
        expect(copyButton()!.title).toBe('common.copy');
    });
});
