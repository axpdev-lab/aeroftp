// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot } from 'react-dom/client';
import { expect, it, vi } from 'vitest';
import { PropertiesDialog } from './index';

vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));

it.each([
    [true, false, true],
    [true, true, false],
    [false, false, false],
])('shows a partial note only after an incomplete scan stops (%s, %s)', async (partial, scanning, visible) => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    const host = document.createElement('div');
    document.body.append(host);
    const root = createRoot(host);
    try {
        await act(async () => root.render(createElement(PropertiesDialog, {
            file: { name: 'docs', path: '/docs', size: null, is_dir: true, modified: null, isRemote: true },
            onClose: () => {},
            folderSize: { total_bytes: 4, file_count: 1, dir_count: 1, partial },
            folderSizeCalculating: scanning,
        })));
        expect(host.textContent?.includes('properties.sizesPartial')).toBe(visible);
    } finally {
        await act(async () => root.unmount());
        host.remove();
    }
});
