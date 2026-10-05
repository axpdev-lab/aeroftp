// @vitest-environment jsdom
// #958: the storage chip is the live indicator of a used-storage scan: the
// running used / total with the file count in words, a spinner and Cancel,
// and no second chip whose "files · bytes" read as a fraction.
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { quotaChipLabel } from '../utils/quotaChipLabel';

vi.mock('../i18n', () => ({
    useTranslation: () => (key: string) => key,
    useI18n: () => ({ language: 'en', t: (key: string) => key }),
}));
vi.mock('./AeroShareStatusButton', () => ({ AeroShareStatusButton: () => null }));

const { StatusBar } = await import('./StatusBar');

let root: Root;
let host: HTMLDivElement;
const render = async (props: Record<string, unknown>) => {
    await act(async () => root.render(createElement(StatusBar, {
        isConnected: true,
        activePanel: 'remote',
        onScanUsed: () => {},
        onCancelUsedScan: () => {},
        ...props,
    } as never)));
};
const chip = () => host.querySelector('[data-storage-chip]') as HTMLElement | null;

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
});

it('labels the figure as used / total with the file count in words', () => {
    const fmt = (n: number) => `${n} B`;
    expect(quotaChipLabel({ used: 40, total: 100, files: 1234 }, 'files', fmt)).toBe(`40 B / 100 B (${(1234).toLocaleString()} files)`);
    expect(quotaChipLabel({ used: 40, total: 0, files: null }, 'files', fmt)).toBe('40 B');
    expect(quotaChipLabel({ used: 40, total: 0, files: 3 }, 'files', fmt)).not.toContain('·');
});

it('shows a running scan in the one chip, against the kept total, with Cancel', async () => {
    await render({
        storageQuota: { used: 40, total: 100, free: 60, files: 3 },
        usedScanStatus: { running: true, files: 3, bytes: 40 },
    });
    expect(host.querySelectorAll('[data-storage-chip]')).toHaveLength(1);
    expect(chip()!.dataset.storageChip).toBe('scanning');
    expect(chip()!.textContent).toContain('/');
    expect(chip()!.textContent).toContain('(3 browser.files)');
    expect(chip()!.textContent).not.toContain('·');
    expect(chip()!.querySelector('.animate-spin')).not.toBeNull();
    expect([...chip()!.querySelectorAll('button')].map((b) => b.title)).toContain('common.cancel');
});

it('shows a scan that has no figure yet as its running count, not "files · bytes"', async () => {
    await render({ storageQuota: null, usedScanStatus: { running: true, files: 7, bytes: 2048 } });
    expect(chip()!.textContent).toContain('(7 browser.files)');
    expect(host.textContent).not.toContain('7 ·');
});

it('shows the figure without a spinner or Cancel once the scan is over', async () => {
    await render({ storageQuota: { used: 40, total: 100, free: 60, files: 3 }, usedScanStatus: null });
    expect(chip()!.dataset.storageChip).toBe('idle');
    expect(chip()!.querySelector('.animate-spin')).toBeNull();
    expect([...chip()!.querySelectorAll('button')].map((b) => b.title)).not.toContain('common.cancel');
});
