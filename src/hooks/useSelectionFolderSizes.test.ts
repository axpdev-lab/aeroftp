// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement, useState } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it } from 'vitest';
import { useSelectionFolderSizes } from './useSelectionFolderSizes';

let calls: string[];
let pending: Map<string, () => void>;
let rerender: () => void;

/** App's shape: a new `calculate` on every render, and renders while it runs. */
const Probe = ({ folders }: { folders: string[] }) => {
    const [, setTick] = useState(0);
    rerender = () => setTick(t => t + 1);
    const calculate = (path: string) => new Promise<void>(resolve => {
        calls.push(path);
        pending.set(path, resolve);
    });
    return createElement('span', null, useSelectionFolderSizes(folders, calculate) ? 'running' : 'idle');
};

let root: Root;
let host: HTMLDivElement;
const render = async (folders: string[]) => {
    await act(async () => root.render(createElement(Probe, { folders })));
};
const finish = async (path: string) => {
    await act(async () => pending.get(path)!());
};
const renderAgain = async () => {
    for (let i = 0; i < 3; i++) await act(async () => rerender());
};

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    calls = [];
    pending = new Map();
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
});

it('counts each folder once, one after the other, through renders', async () => {
    await render(['/x/a', '/x/b']);
    await renderAgain();
    expect(calls).toEqual(['/x/a']);
    expect(host.textContent).toBe('running');

    await finish('/x/a');
    await renderAgain();
    expect(calls).toEqual(['/x/a', '/x/b']);
    expect(host.textContent).toBe('running');

    await finish('/x/b');
    await renderAgain();
    expect(calls).toEqual(['/x/a', '/x/b']);
    expect(host.textContent).toBe('idle');
});

it('starts no further folder once the dialog has closed', async () => {
    await render(['/x/a', '/x/b']);
    await render([]);
    expect(host.textContent).toBe('idle');
    await finish('/x/a');
    expect(calls).toEqual(['/x/a']);
});

it('passes a folder name holding a line break whole', async () => {
    await render(['/x/two\nlines']);
    expect(calls).toEqual(['/x/two\nlines']);
});
