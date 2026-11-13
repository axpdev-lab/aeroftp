// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => ({})) }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));

import { BoxTagsDialog } from './BoxTagsDialog';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
});

it('rendering and cancelling leave the caller currentTags array untouched', async () => {
    // Deliberately unsorted: the dirty check must not sort the caller's array.
    const currentTags = ['zeta', 'alpha', 'mid'];
    const onClose = vi.fn();
    await act(async () => {
        root.render(<BoxTagsDialog filePath="/f.txt" currentTags={currentTags} onClose={onClose} onUpdated={() => undefined} />);
    });
    expect(currentTags).toEqual(['zeta', 'alpha', 'mid']);
    const x = [...host.querySelectorAll('button')].find(b => b.querySelector('svg.lucide-x'))!;
    await act(async () => { x.click(); });
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(currentTags).toEqual(['zeta', 'alpha', 'mid']);
});

it('routes the footer Cancel through the dirty guard', async () => {
    const currentTags = ['alpha'];
    const onClose = vi.fn();
    await act(async () => {
        root.render(<BoxTagsDialog filePath="/f.txt" currentTags={currentTags} onClose={onClose} onUpdated={() => undefined} />);
    });
    const input = host.querySelector<HTMLInputElement>('input[placeholder="box.tagsPlaceholder"]')!;
    await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, 'newtag');
        input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    const cancel = [...host.querySelectorAll('button')].find(b => b.textContent?.trim() === 'common.cancel')!;
    await act(async () => { cancel.click(); });
    expect(host.textContent).toContain('modalGuard.dirtyTitle');
    expect(onClose).not.toHaveBeenCalled();
    const discard = [...host.querySelectorAll('button')].find(b => b.textContent?.trim() === 'modalGuard.discardAndClose')!;
    await act(async () => { discard.click(); });
    expect(onClose).toHaveBeenCalledTimes(1);
});
