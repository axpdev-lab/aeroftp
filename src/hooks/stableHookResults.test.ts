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
import { ActivityLogProvider, useActivityLog } from './useActivityLog';
import { useHumanizedLog } from './useHumanizedLog';

const keyAsText = i18n.t;
let root: Root | null = null;
afterEach(async () => {
    await act(async () => root?.unmount());
    root = null;
    i18n.t = keyAsText;
    i18n.context = { language: 'en' };
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

    it('useHumanizedLog: a new object when the language or its texts change, and it writes them', async () => {
        const seen: Array<ReturnType<typeof useHumanizedLog>> = [];
        let messages: string[] = [];
        const Probe = () => {
            seen.push(useHumanizedLog());
            messages = useActivityLog().entries.map(entry => entry.message);
            return null;
        };
        const tree = () => createElement(ActivityLogProvider, null, createElement(Probe));
        await mount(tree());
        const english = seen[seen.length - 1];

        // The language alone: logNavigate words a step into a subfolder by it
        // (in English "entered", elsewhere the plain line).
        i18n.context = { language: 'de' };
        await act(async () => root!.render(tree()));
        const german = seen[seen.length - 1];
        expect(german).not.toBe(english);
        await act(async () => { german.logNavigate('/a', true); });
        await act(async () => { german.logNavigate('/a/b', true); });
        expect(messages).toHaveLength(2);
        expect(new Set(messages)).toEqual(new Set(['activity.navigate_success']));

        // The texts alone.
        i18n.t = (key: string) => `de:${key}`;
        await act(async () => root!.render(tree()));
        const translated = seen[seen.length - 1];
        expect(translated).not.toBe(german);
        await act(async () => { translated.logRaw('activity.disconnect_start', 'DISCONNECT', {}, 'error'); });
        expect(messages).toContain('de:activity.disconnect_start');
    });
});
