// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement, useEffect, Suspense, startTransition } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it } from 'vitest';
import { useLatestCallback } from './useLatestCallback';

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

describe('useLatestCallback', () => {
    it('keeps one identity and runs the body of the last render', async () => {
        const seen: Array<(suffix: string) => string> = [];
        const Probe = ({ label }: { label: string }) => {
            seen.push(useLatestCallback((suffix: string) => `${label}${suffix}`));
            return null;
        };
        await mount(createElement(Probe, { label: 'first' }));
        await act(async () => root!.render(createElement(Probe, { label: 'second' })));

        expect(seen.length).toBe(2);
        expect(seen[1]).toBe(seen[0]);
        // The function held since the first render reads the second one.
        expect(seen[0]('!')).toBe('second!');
    });

    it('lets an effect subscribe once and still see the latest state', async () => {
        const subscriptions: Array<() => string> = [];
        const Listener = ({ value }: { value: string }) => {
            const onEvent = useLatestCallback(() => value);
            useEffect(() => {
                subscriptions.push(onEvent);
            }, [onEvent]);
            return null;
        };
        await mount(createElement(Listener, { value: 'a' }));
        await act(async () => root!.render(createElement(Listener, { value: 'b' })));
        await act(async () => root!.render(createElement(Listener, { value: 'c' })));

        expect(subscriptions.length).toBe(1);
        expect(subscriptions[0]()).toBe('c');
    });
    it('does not publish a value from a suspended render that never commits', async () => {
        const pending = new Promise<void>(() => undefined);
        let suspended = false;
        let read!: () => boolean;
        const Probe = ({ active, suspend }: { active: boolean; suspend: boolean }) => {
            read = useLatestCallback(() => active);
            if (suspend) { suspended = true; throw pending; }
            return null;
        };
        const view = (active: boolean, suspend: boolean) => createElement(Suspense, { fallback: null }, createElement(Probe, { active, suspend }));
        await mount(view(false, false));
        expect(read()).toBe(false);
        await act(async () => { startTransition(() => root!.render(view(true, true))); });
        expect(suspended).toBe(true);
        expect(read()).toBe(false);
        await act(async () => root!.render(view(false, false)));
        expect(read()).toBe(false);
    });

});
