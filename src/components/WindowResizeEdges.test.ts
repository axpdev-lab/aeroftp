// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

const state = { maximized: false, fullscreen: false };
/** When set, isMaximized answers only once the returned resolver is called. */
let holdMaximized: ((release: () => void) => void) | null = null;
let onResized: (() => void) | null = null;
const unlisten = vi.fn();

vi.mock('@tauri-apps/api/window', () => ({
    getCurrentWindow: () => ({
        isMaximized: () => {
            const answer = state.maximized;
            if (!holdMaximized) return Promise.resolve(answer);
            const hold = holdMaximized;
            return new Promise<boolean>((resolve) => hold(() => resolve(answer)));
        },
        isFullscreen: async () => state.fullscreen,
        onResized: async (cb: () => void) => {
            onResized = cb;
            return unlisten;
        },
    }),
    LogicalSize: class {},
    LogicalPosition: class {},
}));

import { useRoundedWindowCorners } from './WindowResizeEdges';

const flag = window as { __AEROFTP_ROUNDED_CORNERS__?: boolean };
const rounded = () => document.documentElement.classList.contains('rounded-window');
const Probe = () => {
    useRoundedWindowCorners();
    return null;
};
const flush = () => act(async () => { await new Promise((r) => setTimeout(r, 0)); });

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    state.maximized = false;
    state.fullscreen = false;
    onResized = null;
    holdMaximized = null;
    unlisten.mockClear();
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
    delete flag.__AEROFTP_ROUNDED_CORNERS__;
});

it('leaves the corners square when the backend did not make the window transparent', async () => {
    await act(async () => root.render(createElement(Probe)));
    await flush();
    expect(rounded()).toBe(false);
    expect(onResized).toBeNull();
});

it('rounds the corners of a normal window and squares them while maximized or fullscreen', async () => {
    flag.__AEROFTP_ROUNDED_CORNERS__ = true;
    await act(async () => root.render(createElement(Probe)));
    await flush();
    expect(rounded()).toBe(true);

    state.maximized = true;
    onResized?.();
    await flush();
    expect(rounded()).toBe(false);

    state.maximized = false;
    state.fullscreen = true;
    onResized?.();
    await flush();
    expect(rounded()).toBe(false);

    state.fullscreen = false;
    onResized?.();
    await flush();
    expect(rounded()).toBe(true);
});

it('removes the class and stops listening when it unmounts', async () => {
    flag.__AEROFTP_ROUNDED_CORNERS__ = true;
    await act(async () => root.render(createElement(Probe)));
    await flush();
    await act(async () => root.unmount());
    expect(rounded()).toBe(false);
    expect(unlisten).toHaveBeenCalledOnce();
    root = createRoot(host);
});

it('applies only the latest answer when two checks resolve out of order', async () => {
    flag.__AEROFTP_ROUNDED_CORNERS__ = true;
    await act(async () => root.render(createElement(Probe)));
    await flush();
    expect(rounded()).toBe(true);

    const held: Array<() => void> = [];
    holdMaximized = (release) => held.push(release);
    onResized?.(); // asks while the window is still normal
    state.maximized = true;
    onResized?.(); // asks again once it is maximized
    held[1](); // the later answer arrives first
    await flush();
    expect(rounded()).toBe(false);
    held[0](); // the earlier "not maximized" arrives last
    await flush();
    expect(rounded()).toBe(false);
});
