// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { expect, it, vi } from 'vitest';
import { applyPanelSelection, type SelectionPanel } from './panelSelection';

it('rejects an entire stale explicit selection without changing the current one', () => {
    const setSelection = vi.fn();
    expect(applyPanelSelection({ files: [{ name: 'present' }], setSelection }, ['present', 'removed'], 'names')).toBe(false);
    expect(setSelection).not.toHaveBeenCalled();
});

it('uses only names, deduplicates input and does not retain caller-owned collections', () => {
    const setSelection = vi.fn();
    const names = ['same', 'same'];
    applyPanelSelection({ files: [{ name: 'same', password: 'SENTINEL' } as { name: string }], setSelection }, names, 'names');
    names.push('later');
    expect(setSelection.mock.calls[0][0]).toEqual(new Set(['same']));
});

it('selects all or clears only the addressed panel, including a second local panel', () => {
    const local: SelectionPanel = { files: [{ name: 'same' }], setSelection: vi.fn() };
    const local2: SelectionPanel = { files: [{ name: 'same' }, { name: 'second' }], setSelection: vi.fn() };
    const remote: SelectionPanel = { files: [{ name: 'same' }], setSelection: vi.fn() };
    applyPanelSelection(local2, [], 'all');
    expect(local2.setSelection).toHaveBeenCalledExactlyOnceWith(new Set(['same', 'second']));
    applyPanelSelection(local2, [], 'none');
    expect(local2.setSelection).toHaveBeenLastCalledWith(new Set());
    expect(local.setSelection).not.toHaveBeenCalled();
    expect(remote.setSelection).not.toHaveBeenCalled();
});

it('recomputes select-all against the current listing and handles an empty panel', () => {
    const panel: SelectionPanel = { files: [{ name: 'old' }], setSelection: vi.fn() };
    panel.files = [{ name: 'new' }];
    applyPanelSelection(panel, [], 'all');
    expect(panel.setSelection).toHaveBeenLastCalledWith(new Set(['new']));
    panel.files = [];
    applyPanelSelection(panel, [], 'all');
    expect(panel.setSelection).toHaveBeenLastCalledWith(new Set());
});
