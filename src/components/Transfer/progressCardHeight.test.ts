// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { emptySpeedProfile, recordProgress } from '../../utils/speedProfile';
import { ProgressCard, type TransferToastState } from './index';

vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../../hooks/useTheme', () => ({
    useTheme: () => ({ theme: 'light', isDark: false }),
    getEffectiveTheme: () => 'light',
}));
const graphProfile = vi.hoisted(() => ({ last: undefined as unknown }));
vi.mock('../SpeedGraph', () => ({
    SpeedGraph: ({ profile }: { profile: unknown }) => {
        graphProfile.last = profile;
        return null;
    },
}));

const summary = (filename: string, total = 100) => ({
    transfer_id: filename,
    filename,
    total,
    transferred: 50,
    percentage: 50,
    speed_bps: 10,
    eta_seconds: 5,
    direction: 'upload',
}) as TransferToastState['summary'];
const profile = [
    { transferred: 10, total: 100, speedBps: 10, at: 1000 },
    { transferred: 50, total: 100, speedBps: 10, at: 2000 },
].reduce(recordProgress, emptySpeedProfile());
const toast = (filename: string): TransferToastState => ({ summary: summary(filename), speedProfile: profile });
// What the card receives when a file starts: no speed data yet.
const starting = (filename: string): TransferToastState => ({ summary: summary(filename), speedProfile: emptySpeedProfile() });

let root: Root;
let host: HTMLDivElement;
let height = 0;
const render = async (transfer: TransferToastState, measured: number) => {
    height = measured;
    await act(async () => root.render(createElement(ProgressCard, { transfer, onCancel: () => {}, onMinimize: () => {} })));
};
const card = () => host.firstElementChild as HTMLElement;

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(() => ({ height }) as DOMRect);
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
    vi.restoreAllMocks();
});

// The card is anchored at the bottom, so a change of height moves its top bar:
// with a fast connection and many small files that was once per file (#347).
describe('ProgressCard during a transfer', () => {
    const toggle = () => host.querySelector<HTMLButtonElement>('.tpb-graph-toggle');
    const setGraphOpen = async (open: boolean) => {
        if ((toggle()!.getAttribute('aria-expanded') === 'true') !== open) await act(async () => toggle()!.click());
        expect(toggle()!.getAttribute('aria-expanded')).toBe(String(open));
    };

    it('does not shrink from one file to the next', async () => {
        await render(toast('a.bin'), 300);
        expect(card().style.minHeight).toBe('300px');
        await render(starting('b.bin'), 240);
        expect(card().style.minHeight).toBe('300px');
    });

    it('keeps the speed graph and its toggle while the next file has no data yet', async () => {
        await render(toast('a.bin'), 300);
        await render(starting('b.bin'), 300);
        expect(toggle()).not.toBeNull();
        expect(graphProfile.last).toBe(profile);
    });

    it('has no graph before the first data', async () => {
        await render(starting('a.bin'), 300);
        expect(toggle()).toBeNull();
    });

    it('drops the graph of a transfer in the other direction', async () => {
        await render(toast('a.bin'), 300);
        const download = { ...summary('b.bin'), direction: 'download' } as TransferToastState['summary'];
        await render({ summary: download, speedProfile: emptySpeedProfile() }, 300);
        expect(toggle()).toBeNull();
    });

    it('does not bring back an earlier graph after a transfer of another kind', async () => {
        await render(toast('a.bin'), 300);
        const download = { ...summary('b.bin'), direction: 'download' } as TransferToastState['summary'];
        await render({ summary: download, speedProfile: emptySpeedProfile() }, 300);
        await render(starting('c.bin'), 300);
        expect(toggle()).toBeNull();
    });

    it('keeps the height of the open graph, measured when its animation ends', async () => {
        vi.useFakeTimers();
        localStorage.clear();
        await render(toast('a.bin'), 300);
        await setGraphOpen(false);
        await setGraphOpen(true);
        height = 420;
        await act(async () => vi.advanceTimersByTime(350));
        expect(card().style.minHeight).toBe('420px');
        vi.useRealTimers();
    });

    it('holds the height again when the animation of a closed graph ends, with no render', async () => {
        vi.useFakeTimers();
        await render(toast('a.bin'), 420);
        await setGraphOpen(true);
        await setGraphOpen(false);
        expect(card().style.minHeight).toBe('');
        height = 300;
        await act(async () => vi.advanceTimersByTime(350));
        expect(card().style.minHeight).toBe('300px');
        vi.useRealTimers();
    });

    it('grows when its content needs more room', async () => {
        await render(toast('a.bin'), 300);
        await render(toast('b.bin'), 360);
        expect(card().style.minHeight).toBe('360px');
    });

    it('starts over for a card opened later', async () => {
        await render(toast('a.bin'), 300);
        await act(async () => root.unmount());
        root = createRoot(host);
        await render(toast('b.bin'), 200);
        expect(card().style.minHeight).toBe('200px');
    });

    it('starts over when a single file follows a folder at once, in the same card', async () => {
        await render({ ...toast('folder'), summary: { ...summary('folder'), total_files: 40 } }, 420);
        expect(card().style.minHeight).toBe('420px');
        await render(toast('a.bin'), 240);
        expect(card().style.minHeight).toBe('240px');
    });

    it('holds its height when the user opens the graph', async () => {
        await render(toast('a.bin'), 300);
        await setGraphOpen(false);
        await render(toast('a.bin'), 300);
        await setGraphOpen(true);
        expect(card().style.minHeight).toBe('300px');
    });

    it('gives the height back when the user closes the graph, once its animation has ended', async () => {
        const now = vi.spyOn(performance, 'now').mockReturnValue(1000);
        await render(toast('a.bin'), 420);
        await setGraphOpen(true);
        expect(card().style.minHeight).toBe('420px');

        await setGraphOpen(false);
        expect(card().style.minHeight).toBe('');

        // Mid-transition the card still measures tall: that must not be kept.
        now.mockReturnValue(1200);
        await render(toast('a.bin'), 420);
        expect(card().style.minHeight).toBe('');

        now.mockReturnValue(1400);
        await render(toast('a.bin'), 300);
        expect(card().style.minHeight).toBe('300px');
    });
});
