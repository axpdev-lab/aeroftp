// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later

import * as React from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { CompareTabContent } from './CompareTabContent';
import { compareEntries } from '../../utils/compareEndpoints';

const mocks = vi.hoisted(() => ({ pickSave: vi.fn(), writeTextFile: vi.fn() }));
vi.mock('@tauri-apps/plugin-fs', () => ({ writeTextFile: mocks.writeTextFile }));
vi.mock('../../utils/pickPath', () => ({ pickSave: mocks.pickSave, pickFile: vi.fn() }));
vi.mock('../../i18n', () => ({
    useTranslation: () => (key: string, args?: Record<string, unknown>) => (args ? `${key} ${Object.values(args).join(' ')}` : key),
}));

const result = compareEntries([{ name: 'only-here', isDir: true, size: 0 }, { name: 'a.txt', isDir: false, size: 3 }], []);
let root: Root;
let host: HTMLDivElement;
const button = (key: string) => Array.from(host.querySelectorAll('button')).find(b => b.textContent?.includes(key))!;

beforeEach(async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div'); document.body.append(host);
    root = createRoot(host);
    mocks.pickSave.mockReset().mockResolvedValue('/tmp/out.json');
    mocks.writeTextFile.mockReset().mockResolvedValue(undefined);
    await React.act(async () => root.render(React.createElement(CompareTabContent, {
        result, leftLabel: 'l', rightLabel: 'r', canMirrorLeftToRight: true, canMirrorRightToLeft: true,
        onApplyMirrorLeftToRight: vi.fn(), onApplyMirrorRightToLeft: vi.fn(),
    } as React.ComponentProps<typeof CompareTabContent>)));
});
afterEach(async () => {
    await React.act(async () => root.unmount());
    host.remove();
});

describe('the Compare tab', () => {
    it('says a write error of the export', async () => {
        mocks.writeTextFile.mockRejectedValue(new Error('disk full'));
        await React.act(async () => button('syncPanel.exportJSON').click());
        const alert = host.querySelector('[role=alert]')?.textContent;
        expect(alert).toContain('aerosync.exportFailed');
        expect(alert).toContain('disk full');
    });

    it('writes each format to the path chosen, with its own filter and content', async () => {
        await React.act(async () => button('syncPanel.exportCSV').click());
        expect(mocks.pickSave.mock.calls[0][0].filters).toEqual([{ name: 'CSV', extensions: ['csv'] }]);
        const [csvPath, csv] = mocks.writeTextFile.mock.calls[0];
        expect(csvPath).toBe('/tmp/out.json');
        expect(csv).toContain('a.txt');
        expect(() => JSON.parse(csv)).toThrow();

        await React.act(async () => button('syncPanel.exportJSON').click());
        expect(mocks.pickSave.mock.calls[1][0].filters).toEqual([{ name: 'JSON', extensions: ['json'] }]);
        expect(JSON.stringify(JSON.parse(mocks.writeTextFile.mock.calls[1][1]))).toContain('a.txt');
    });

    it('opens one save dialog however many times the export is clicked', async () => {
        let answer = (_path: string | null) => {};
        mocks.pickSave.mockImplementation(() => new Promise((resolve) => { answer = resolve; }));
        await React.act(async () => {
            button('syncPanel.exportJSON').click();
            button('syncPanel.exportCSV').click();
        });
        expect(mocks.pickSave).toHaveBeenCalledTimes(1);
        await React.act(async () => answer(null));
        expect(mocks.writeTextFile).not.toHaveBeenCalled();
    });

    it('shows no size on the side a folder is missing from', () => {
        const row = Array.from(host.querySelectorAll('tr')).find(r => r.textContent?.includes('only-here'))!;
        const cells = Array.from(row.querySelectorAll('td')).map(c => c.textContent);
        expect(cells[1]).toBe('aerosync.compareTable.folder');
        expect(cells[2]).toBe('-');
    });
});
