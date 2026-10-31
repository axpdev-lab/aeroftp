// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type React from 'react';

const tauri = vi.hoisted(() => ({ invoke: vi.fn(async (_cmd: string, _args?: unknown) => undefined) }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: tauri.invoke }));

import { useDragAndDrop, type PanelKey } from './useDragAndDrop';

type Drag = ReturnType<typeof useDragAndDrop>;
type Row = { name: string; path: string; is_dir: boolean };

let root: Root | null = null;
let drag: Drag | null = null;

beforeEach(() => tauri.invoke.mockClear());
afterEach(async () => {
    await act(async () => root?.unmount());
    root = null;
    drag = null;
});

const event = () => ({
    preventDefault() {},
    stopPropagation() {},
    ctrlKey: false,
    dataTransfer: { effectAllowed: '', dropEffect: '', setData() {}, setDragImage() {} },
}) as unknown as React.DragEvent;

/** Windows panels: panel 1 spells its folder with `\`, panel 2 with `/`. */
const mount = async (currentLocalPath: string, currentLocalPath2: string) => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    const Probe = () => {
        drag = useDragAndDrop({
            notify: { success: () => null, error: () => null },
            humanLog: { logStart: () => 'id', logSuccess: () => 'id', logError: () => 'id' },
            currentRemotePath: '/',
            currentLocalPath,
            currentLocalPath2,
            loadRemoteFiles: async () => undefined,
            loadLocalFiles: async () => true,
            loadLocalFiles2: async () => true,
        });
        return null;
    };
    root = createRoot(document.createElement('div'));
    await act(async () => root!.render(createElement(Probe)));
};

const dragRow = async (row: Row, panel: PanelKey, shown: Row[]) => {
    await act(async () => drag!.handleDragStart(event(), row, panel, new Set([row.name]), shown));
};

const moves = () => tauri.invoke.mock.calls.filter(([cmd]) => cmd === 'rename_local_file' || cmd === 'copy_local_file');

/**
 * A drop that puts an entry where it already is does nothing. The destination
 * is joined with `/` (or with the separator of the target folder), and a
 * Windows row path has `\`: compared as strings, the two name one file
 * differently, and the move ran on the file itself and failed with an error.
 */
describe('a drop of an entry where it already is does nothing, whichever separator names it', () => {
    it('a nested row of a flattened search dropped on the folder it is in (same panel)', async () => {
        await mount('C:\\a', 'C:\\b');
        const row = { name: 'sub/b.txt', path: 'C:\\a\\sub\\b.txt', is_dir: false };
        await dragRow(row, 'local', [row]);
        await act(async () => drag!.handleDrop(event(), 'C:\\a\\sub', 'local'));
        expect(moves()).toEqual([]);
    });

    it('a row dropped on the other panel showing its folder written with `/`', async () => {
        await mount('C:\\a', 'C:/a');
        const row = { name: 'b.txt', path: 'C:\\a\\b.txt', is_dir: false };
        await dragRow(row, 'local', [row]);
        await act(async () => drag!.handlePanelDrop(event(), 'local2'));
        expect(moves()).toEqual([]);
    });

    it('still moves a row to another folder', async () => {
        await mount('C:\\a', 'C:\\b');
        const row = { name: 'sub/b.txt', path: 'C:\\a\\sub\\b.txt', is_dir: false };
        await dragRow(row, 'local', [row]);
        await act(async () => drag!.handleDrop(event(), 'C:\\a\\other', 'local'));
        expect(moves()).toEqual([
            ['rename_local_file', { from: 'C:\\a\\sub\\b.txt', to: 'C:\\a\\other/b.txt', overwrite: false }],
        ]);
    });
});
