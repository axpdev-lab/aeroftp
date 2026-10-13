// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { act, createElement as h } from 'react';
import { createRoot } from 'react-dom/client';
import { expect, it, vi } from 'vitest';
import { GuiControllerBanner } from './GuiControllerBanner';
import { DEFAULT_GUI_PRESENTATION } from '../gui/presentation';
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
it('refuses synthetic speed and Pause input outside DEV while keeping Stop available', async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    vi.stubEnv('DEV', false);
    const host = document.createElement('div'); document.body.append(host); const root = createRoot(host);
    const onSpeed = vi.fn(), onPause = vi.fn(), onStop = vi.fn();
    try {
        await act(async () => root.render(h(GuiControllerBanner, { lease: {
            owner: { id: 'aeroagent:test', kind: 'aeroagent', label: 'AeroAgent' }, intent: null,
            control: { speed_percent: 100, speed_source: 'default', paused: false, phase: 'ready' },
        }, preferences: DEFAULT_GUI_PRESENTATION, onSpeed, onPause, onStop })));
        const controls = host.querySelectorAll<HTMLButtonElement>('button[data-gui-controller-control]');
        await act(async () => { for (const button of controls) button.click(); });
        const badge = host.querySelector('[data-gui-controller-badge]');
        expect(badge?.className).not.toContain('fixed');
        expect(badge?.className).toContain('border-t');
        expect(badge?.getAttribute('role')).toBe('region');
        const agent = host.querySelector('[data-gui-controller-agent]');
        expect(agent?.className).toContain('bg-amber-400');
        const stop = host.querySelector('[data-gui-controller-stop]');
        expect(stop?.previousElementSibling?.className).toContain('w-px');
        expect(stop?.previousElementSibling?.previousElementSibling?.tagName).toBe('BUTTON');
        expect(onSpeed).not.toHaveBeenCalled(); expect(onPause).not.toHaveBeenCalled();
        expect(host.querySelector('input')?.dataset.agent).toBe('deny');
        await act(async () => host.querySelector<HTMLButtonElement>('[data-gui-controller-stop]')!.click());
        expect(onStop).toHaveBeenCalledTimes(1);
    } finally { await act(async () => root.unmount()); host.remove(); vi.unstubAllEnvs(); }
});
