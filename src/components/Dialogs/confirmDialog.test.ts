// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { ConfirmDialog } from './index';

vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));

let root: Root;
let host: HTMLDivElement;
const labels = () => [...document.querySelectorAll('button')].map(b => b.textContent?.trim());
const render = async (informational?: boolean) => {
    await act(async () => root.render(createElement(ConfirmDialog, {
        message: 'Nothing was imported.',
        onConfirm: () => {},
        onCancel: () => {},
        confirmLabel: 'OK',
        informational,
    })));
};
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

it('offers Cancel next to the choice it confirms', async () => {
    await render();
    expect(labels()).toEqual(['common.cancel', 'OK']);
});

// The Flatpak import results that need no restart only inform: a Cancel next
// to OK offered a choice that did not exist (left from 4.2.1, #961).
it('shows only the acknowledgement for an informational message', async () => {
    await render(true);
    expect(labels()).toEqual(['OK']);
});
