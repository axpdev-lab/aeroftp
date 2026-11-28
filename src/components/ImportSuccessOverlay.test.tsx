// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ImportCounts } from './ImportSuccessOverlay';

vi.mock('../i18n', () => ({
    useTranslation: () => (key: string, params?: Record<string, unknown>) =>
        params ? `${key} ${JSON.stringify(params)}` : key,
}));

import { ImportSuccessOverlay, IMPORT_SUCCESS_CLOSE_DELAY_MS } from './ImportSuccessOverlay';

let root: Root;
let host: HTMLDivElement;

beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    // clearTimeout must be faked too: a real clearTimeout cannot cancel a fake timer.
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
});

afterEach(() => {
    act(() => root.unmount());
    host.remove();
    vi.useRealTimers();
});

const render = async (counts: ImportCounts, onDone: () => void, delayMs?: number) => {
    await act(async () => {
        root.render(createElement(ImportSuccessOverlay, {
            counts,
            onDone,
            ...(delayMs !== undefined ? { delayMs } : {}),
        }));
    });
};

const summaryText = () => host.querySelector('[data-testid="import-success-summary"]')?.textContent ?? '';

describe('ImportSuccessOverlay', () => {
    it('shows the real counts with the existing i18n keys and closes after the short pause', async () => {
        const onDone = vi.fn();
        await render({ imported: 3, skipped: 1, overwritten: 2 }, onDone);
        expect(host.querySelector('[data-testid="import-success-overlay"]')).not.toBeNull();
        expect(host.querySelector('[data-testid="import-success-check"]')).not.toBeNull();
        expect(summaryText()).toContain('settings.importSuccess {"count":3}');
        expect(summaryText()).toContain('settings.bridgeSkippedCount {"count":1}');
        expect(summaryText()).toContain('settings.overwrittenCount {"count":2}');
        // Not before the pause is over, then exactly once.
        await act(async () => { vi.advanceTimersByTime(IMPORT_SUCCESS_CLOSE_DELAY_MS - 1); });
        expect(onDone).not.toHaveBeenCalled();
        await act(async () => { vi.advanceTimersByTime(1); });
        expect(onDone).toHaveBeenCalledTimes(1);
        await act(async () => { vi.advanceTimersByTime(5000); });
        expect(onDone).toHaveBeenCalledTimes(1);
    });

    it('leaves zero skipped and overwritten counts out of the summary', async () => {
        await render({ imported: 2, skipped: 0, overwritten: 0 }, vi.fn());
        expect(summaryText()).toContain('settings.importSuccess {"count":2}');
        expect(summaryText()).not.toContain('bridgeSkippedCount');
        expect(summaryText()).not.toContain('overwrittenCount');
    });

    it('a click on the overlay ends the pause at once', async () => {
        const onDone = vi.fn();
        await render({ imported: 1, skipped: 0, overwritten: 0 }, onDone);
        await act(async () => {
            host.querySelector<HTMLElement>('[data-testid="import-success-overlay"]')!.click();
        });
        expect(onDone).toHaveBeenCalledTimes(1);
        await act(async () => { vi.advanceTimersByTime(5000); });
        expect(onDone).toHaveBeenCalledTimes(1);
    });

    it('Escape ends the pause at once', async () => {
        const onDone = vi.fn();
        await render({ imported: 1, skipped: 0, overwritten: 0 }, onDone);
        await act(async () => {
            window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
        });
        expect(onDone).toHaveBeenCalledTimes(1);
    });

    it('unmounting before the pause ends clears the timer and never fires', async () => {
        const onDone = vi.fn();
        await render({ imported: 1, skipped: 0, overwritten: 0 }, onDone);
        act(() => root.unmount());
        await act(async () => { vi.advanceTimersByTime(10000); });
        expect(onDone).not.toHaveBeenCalled();
    });

    it('honours a custom delay', async () => {
        const onDone = vi.fn();
        await render({ imported: 1, skipped: 0, overwritten: 0 }, onDone, 200);
        await act(async () => { vi.advanceTimersByTime(199); });
        expect(onDone).not.toHaveBeenCalled();
        await act(async () => { vi.advanceTimersByTime(1); });
        expect(onDone).toHaveBeenCalledTimes(1);
    });
});
