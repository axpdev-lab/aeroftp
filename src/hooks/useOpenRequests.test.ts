// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import type { OpenRequest } from '../types';
import { useOpenRequests } from './useOpenRequests';

const bridge = vi.hoisted(() => {
    const queue: unknown[] = [];
    const listeners = new Map<string, (event: { payload: unknown }) => void>();
    return {
        queue,
        listeners,
        invoke: vi.fn(async (command: string) => (command === 'take_open_requests' ? queue.splice(0) : undefined)),
    };
});
vi.mock('@tauri-apps/api/core', () => ({ invoke: bridge.invoke }));
vi.mock('@tauri-apps/api/event', () => ({
    listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
        bridge.listeners.set(name, handler);
        return () => bridge.listeners.delete(name);
    }),
}));

const request = (name: string): OpenRequest => ({
    kind: 'local', path: `/pictures/${name}`, folder: '/pictures', name, isDir: false,
});
const handled: OpenRequest[] = [];
let handler: (requests: OpenRequest[]) => Promise<void> | void = (requests) => { handled.push(...requests); };
const Probe = ({ ready }: { ready: boolean }) => {
    useOpenRequests(ready, requests => handler(requests));
    return null;
};
let root: Root;
let host: HTMLDivElement;
const render = async (ready: boolean) => {
    await act(async () => root.render(createElement(Probe, { ready })));
};
const emit = async () => {
    await act(async () => bridge.listeners.get('open-request')?.({ payload: null }));
};
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    bridge.queue.length = 0;
    bridge.invoke.mockClear();
    handled.length = 0;
    handler = (requests) => { handled.push(...requests); };
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

it('keeps a file opened during the boot queued until the window is ready', async () => {
    // The cold start: the backend queued the file before the window listened.
    bridge.queue.push(request('photo.png'));
    await render(false);
    await emit();
    expect(bridge.invoke).not.toHaveBeenCalled();
    expect(handled).toEqual([]);
    await render(true);
    expect(handled).toEqual([request('photo.png')]);
});

it('drains again on every event once ready, handling each file once', async () => {
    await render(true);
    bridge.queue.push(request('a.png'), request('b.txt'));
    await emit();
    expect(handled).toEqual([request('a.png'), request('b.txt')]);
    await emit();
    expect(handled).toHaveLength(2);
});

it('takes nothing while the app is locked again', async () => {
    await render(true);
    await render(false);
    bridge.invoke.mockClear();
    bridge.queue.push(request('c.png'));
    await emit();
    expect(bridge.invoke).not.toHaveBeenCalled();
    await render(true);
    expect(handled).toEqual([request('c.png')]);
});

it('runs one drain at a time and takes what arrived during it afterwards', async () => {
    // The first navigation is still in flight when the second file arrives.
    let release!: () => void;
    let active = 0;
    let peak = 0;
    const batches: string[][] = [];
    handler = async (requests) => {
        active++; peak = Math.max(peak, active);
        batches.push(requests.map(r => r.name));
        if (batches.length === 1) await new Promise<void>(resolve => { release = resolve; });
        active--;
    };
    await render(true);
    bridge.queue.push(request('first.png'));
    await emit();
    bridge.queue.push(request('second.png'));
    await emit();
    expect(batches).toEqual([['first.png']]);
    await act(async () => release());
    expect(peak).toBe(1);
    expect(batches).toEqual([['first.png'], ['second.png']]);
});
