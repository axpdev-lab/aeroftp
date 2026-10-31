// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';

const i18n = vi.hoisted(() => {
    const t = (key: string) => key;
    const context = { language: 'en' };
    return { t, context };
});
vi.mock('../i18n', () => ({
    useTranslation: () => i18n.t,
    useI18n: () => i18n.context,
}));

import { useToast } from '../components/Toast';
import { ActivityLogProvider } from './useActivityLog';
import { useHumanizedLog } from './useHumanizedLog';

let root: Root | null = null;
afterEach(async () => {
    await act(async () => root?.unmount());
    root = null;
});

const mount = async (element: ReturnType<typeof createElement>) => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    root = createRoot(document.createElement('div'));
    await act(async () => root!.render(element));
};

/**
 * App builds `notify` from `useToast()` and the activity log, and about 50 of
 * its callbacks and effects depend on `notify`. `useToast()` returned a new
 * object on every render, so `notify` was rebuilt on every render of App: those
 * effects ran again each time (event listeners subscribed again) and the
 * callbacks' missing dependencies went unseen. The objects these hooks return
 * keep their identity until what they hold changes.
 */
describe('hook results that callbacks depend on keep their identity', () => {
    it('useToast: one object while the toasts stay, and the same functions when they change', async () => {
        const seen: Array<ReturnType<typeof useToast>> = [];
        const Probe = (_: { render: number }) => {
            seen.push(useToast());
            return null;
        };
        await mount(createElement(Probe, { render: 1 }));
        await act(async () => root!.render(createElement(Probe, { render: 2 })));
        expect(seen[1]).toBe(seen[0]);

        await act(async () => { seen[1].success('saved'); });
        const after = seen[seen.length - 1];
        expect(after.toasts.map(t => t.title)).toEqual(['saved']);
        for (const name of ['success', 'error', 'info', 'warning', 'addToast', 'addToastWithAction', 'removeToast'] as const) {
            expect(after[name], name).toBe(seen[0][name]);
        }
    });

    it('useHumanizedLog: one object while entries are added to the log', async () => {
        const seen: Array<ReturnType<typeof useHumanizedLog>> = [];
        const Probe = () => {
            seen.push(useHumanizedLog());
            return null;
        };
        await mount(createElement(ActivityLogProvider, null, createElement(Probe)));
        await act(async () => { seen[0].logRaw('activity.disconnect_start', 'DISCONNECT', {}, 'error'); });
        await act(async () => { seen[0].logStart('UPLOAD', { filename: 'a.txt', isRemote: true }); });

        // Each entry renders the log's consumers again.
        expect(seen.length).toBeGreaterThan(2);
        for (const humanLog of seen) expect(humanLog).toBe(seen[0]);
    });
});
