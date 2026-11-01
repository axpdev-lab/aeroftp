// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement, useEffect } from 'react';
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
});
