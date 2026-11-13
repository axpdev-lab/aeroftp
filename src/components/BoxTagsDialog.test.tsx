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
