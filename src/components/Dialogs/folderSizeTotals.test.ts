// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { MultiFilePropertiesDialog, PropertiesDialog } from './index';
import type { FileProperties } from './index';
import type { FolderSizeResult } from '../../types/aerofile';

vi.mock('../../i18n', () => ({
    useTranslation: () => (key: string, params?: Record<string, unknown>) =>
        params ? `${key}${JSON.stringify(params)}` : key,
}));

let cleanup: (() => Promise<void>) | null = null;
afterEach(async () => {
    await cleanup?.();
    cleanup = null;
});

const render = async (element: ReturnType<typeof createElement>): Promise<HTMLElement> => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    const host = document.createElement('div');
    document.body.append(host);
    const root = createRoot(host);
    await act(async () => root.render(element));
    cleanup = async () => {
        await act(async () => root.unmount());
        host.remove();
    };
    return host;
};

const folder: FileProperties = { name: 'docs', path: '/docs', size: null, is_dir: true, modified: null, isRemote: true };
const size = (total_bytes: number, partial = false): FolderSizeResult => ({ total_bytes, file_count: 1, dir_count: 0, partial });

/**
 * A folder's size, once shown, used to stay as it was for the rest of the
 * session: the dialog offered "Calculate Size" only while there was no figure,
 * and a figure cached from an earlier scan, or from another server's folder at
 * the same path, could not be refreshed from anywhere.
 */
describe('a shown folder size can be calculated again', () => {
    it('offers the calculation next to the figure once the scan has stopped', async () => {
        const onCalculateFolderSize = vi.fn();
        const host = await render(createElement(PropertiesDialog, {
            file: folder, onClose: () => {}, folderSize: size(4), folderSizeCalculating: false, onCalculateFolderSize,
        }));
        const again = host.querySelector<HTMLButtonElement>('button[aria-label="properties.calculateSize"]');
        expect(again, 'a control to calculate the size again').not.toBeNull();
        await act(async () => again!.click());
        expect(onCalculateFolderSize).toHaveBeenCalledTimes(1);
    });

    it('does not offer it while the scan is running', async () => {
        const host = await render(createElement(PropertiesDialog, {
            file: folder, onClose: () => {}, folderSize: size(4), folderSizeCalculating: true, onCalculateFolderSize: () => {},
        }));
        expect(host.querySelector('button[aria-label="properties.calculateSize"]')).toBeNull();
    });
});

/**
 * Properties of several items summed the files and left every folder out of
 * the total, without saying so when files were also selected: "Total size:
 * 10 B" for a selection holding a 5 GB folder.
 */
describe('the total of a selection counts its folders', () => {
    const files: FileProperties[] = [
        { name: 'a.txt', path: '/x/a.txt', size: 10, is_dir: false, modified: null, isRemote: false },
        { name: 'docs', path: '/x/docs', size: null, is_dir: true, modified: null, isRemote: false },
    ];
    const multi = (folderSizes: Map<string, FolderSizeResult>, folderSizeCalculating = false) =>
        render(createElement(MultiFilePropertiesDialog, {
            selection: { files, isRemote: false },
            onClose: () => {},
            folderSizes,
            folderSizeCalculating,
        }));

    it('adds the size of each folder to the total', async () => {
        const host = await multi(new Map([['/x/docs', size(100)]]));
        expect(host.textContent).toContain('properties.exactBytes{"bytes":"110"}');
        expect(host.textContent).not.toContain('properties.sizesPartial');
    });

    it('says the total is incomplete while a folder has no size', async () => {
        const host = await multi(new Map());
        expect(host.textContent).toContain('properties.sizesPartial');
    });

    it('says the total is incomplete when a folder scan was partial', async () => {
        const host = await multi(new Map([['/x/docs', size(100, true)]]));
        expect(host.textContent).toContain('properties.sizesPartial');
    });

    it('shows the running total while the folders are being calculated', async () => {
        const host = await multi(new Map([['/x/docs', size(60)]]), true);
        expect(host.textContent).toContain('properties.exactBytes{"bytes":"70"}');
        expect(host.textContent).toContain('properties.calculating');
        expect(host.textContent).not.toContain('properties.sizesPartial');
    });

    it('gives a total for a selection of folders only, once they are sized', async () => {
        const host = await render(createElement(MultiFilePropertiesDialog, {
            selection: { files: files.slice(1).concat({ ...files[1], name: 'more', path: '/x/more' }), isRemote: false },
            onClose: () => {},
            folderSizes: new Map([['/x/docs', size(100)], ['/x/more', size(23)]]),
        }));
        expect(host.textContent).toContain('properties.exactBytes{"bytes":"123"}');
        expect(host.textContent).not.toContain('properties.foldersOnly');
    });
});
