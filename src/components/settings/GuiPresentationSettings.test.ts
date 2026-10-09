// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { act, createElement as h } from 'react';
import { createRoot } from 'react-dom/client';
import { expect, it, vi } from 'vitest';
import { GuiPresentationSettings } from './GuiPresentationSettings';
import { DEFAULT_GUI_PRESENTATION, normalizeGuiPresentation } from '../../gui/presentation';
vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));
it('renders denied human preferences and edits only the requested preset', async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    const host = document.createElement('div'); document.body.append(host); const root = createRoot(host);
    const changed = vi.fn();
    try {
        await act(async () => root.render(h(GuiPresentationSettings, { value: DEFAULT_GUI_PRESENTATION, onChange: changed })));
        const fields = host.querySelectorAll('input'); expect(fields).toHaveLength(4);
        expect(host.querySelector('fieldset')?.dataset.agent).toBe('deny');
        const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!;
        fields[2].focus();
        for (const value of ['1', '17', '175']) await act(async () => { setter.call(fields[2], value); fields[2].dispatchEvent(new Event('input', { bubbles: true })); });
        expect(changed).not.toHaveBeenCalled();
        await act(async () => fields[2].blur());
        expect(changed).toHaveBeenLastCalledWith({ defaultSpeed: 100, presets: [50, 175, 200] });
        changed.mockClear();
        fields[0].focus();
        await act(async () => { setter.call(fields[0], '401'); fields[0].dispatchEvent(new Event('input', { bubbles: true })); });
        await act(async () => fields[0].blur());
        expect(changed).not.toHaveBeenCalled();
    } finally { await act(async () => root.unmount()); host.remove(); }
});
it('normalizes corrupt persisted preferences without carrying arbitrary fields', () => {
    expect(normalizeGuiPresentation({ defaultSpeed: '100', presets: [0, 175, Infinity], password: 'SENTINEL' })).toEqual({ defaultSpeed: 100, presets: [50, 175, 200] });
});
