// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { CompareScanState } from './CompareScanState';
import { isCompareCancelled } from '../../utils/scanCompleteness';

vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../../hooks/useScanProgress', () => ({
    useScanProgress: () => ({ totals: { files: 781456, dirs: 76894, bytes: 0 }, elapsedMs: 26000 }),
    formatElapsed: () => '0:26',
}));

let root: Root;
let host: HTMLDivElement;
const buttons = () => [...document.querySelectorAll('button')].map((b) => b.textContent?.trim());
const render = async (props: Partial<Parameters<typeof CompareScanState>[0]>) => {
    await act(async () => root.render(createElement(CompareScanState, {
        loading: false,
        leftLabel: '/home/user',
        rightLabel: '/home/user/Pictures',
        ...props,
    })));
};
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

it('offers to start a compare instead of starting one by itself', async () => {
    const onStart = vi.fn();
    await render({ onStart });
    expect(buttons()).toEqual(['aerosync.startCompare']);
    await act(async () => (document.querySelector('button') as HTMLButtonElement).click());
    expect(onStart).toHaveBeenCalledOnce();
});

it('shows a running compare with a Stop button that stops it', async () => {
    const onStop = vi.fn();
    await render({ loading: true, scanProgressId: 'aerosync-1', onStop });
    expect(host.textContent).toContain((781456).toLocaleString());
    expect(buttons()).toEqual(['aerosync.stopCompare']);
    await act(async () => (document.querySelector('button') as HTMLButtonElement).click());
    expect(onStop).toHaveBeenCalledOnce();
});

it('says a stopped compare was stopped and offers to start it again', async () => {
    await render({ stopped: true, onStart: vi.fn() });
    expect(host.textContent).toContain('aerosync.compareStopped');
    expect(buttons()).toEqual(['aerosync.startCompare']);
});

it('recognizes only the exact cancelled error, never another failure', () => {
    expect(isCompareCancelled('COMPARE_CANCELLED')).toBe(true);
    expect(isCompareCancelled(new Error('COMPARE_CANCELLED'))).toBe(true);
    expect(isCompareCancelled('Failed to scan local directory: COMPARE_CANCELLED later')).toBe(false);
    expect(isCompareCancelled('SCAN_INCOMPLETE: compare cancelled by user')).toBe(false);
    expect(isCompareCancelled(null)).toBe(false);
});
