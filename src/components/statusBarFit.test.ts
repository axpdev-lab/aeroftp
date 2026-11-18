// @vitest-environment jsdom
// The quota chip was drawn over the server name and the HTTPS badge at
// window widths just above 1100px: the right cluster kept its labels until a
// fixed media query, took the whole bar, and the left cluster neither clipped
// nor kept room for its identity. jsdom has no layout, so widths are stubbed
// per element and the bar is driven through its observers.
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

vi.mock('../i18n', () => ({
    useTranslation: () => (key: string) => key,
    useI18n: () => ({ language: 'en', t: (key: string) => key }),
}));
vi.mock('./AeroShareStatusButton', () => ({ AeroShareStatusButton: () => null }));

const { StatusBar } = await import('./StatusBar');
const {
    decideCompactLevel,
    STATUS_BAR_HYSTERESIS_PX,
    STATUS_BAR_NAME_MIN_PX,
    STATUS_BAR_PATH_MIN_PX,
    STATUS_BAR_SPACING_PX,
} = await import('./useStatusBarFit');

// Stubbed geometry: every left item 100px; in the connection block a dot and
// an icon of 20px each and the server name, 60px on screen and 150px
// untruncated; every label 50px with no gap; the right cluster 300px of
// icons plus the labels that are in the flow.
const ITEM = 100;
const NAME = 60;
const NAME_FULL = 150;
const DOT = 20;
let nameFull = NAME_FULL;
const LABEL_W = 50;
let LABEL = LABEL_W;
const ICONS = 300;
let barWidth = 0;
let leftWidth = 0;
// Fake observers keep what they watch, so a test can fire only the ones that
// watch the bar (a window resize) or only the content ones (a font change).
let observers: Array<{ cb: () => void; targets: Set<Element> }> = [];
let frames: Array<() => void> = [];
const fire = (watching: (target: Element) => boolean) => observers
    .filter((o) => [...o.targets].some(watching))
    .forEach((o) => o.cb());
const flushFrames = () => { const due = frames; frames = []; due.forEach((cb) => cb()); };

let root: Root;
let host: HTMLDivElement;
const bar = () => host.querySelector('[role="status"]') as HTMLElement;
const left = () => bar()?.children[0] as HTMLElement | undefined;
const right = () => bar()?.children[1] as HTMLElement | undefined;
const labels = (tier?: string) => right()!.querySelectorAll(tier ? `[data-statusbar-label="${tier}"]` : '[data-statusbar-label]').length;
const compact = () => bar().getAttribute('data-compact');
// The connection block counts from its contents, its name untruncated.
const identity = () => (left()!.children.length - 2) * ITEM + 2 * DOT + nameFull;
// What the bar needs with the path at its minimum and each label tier shown.
const needAll = () => identity() + STATUS_BAR_PATH_MIN_PX + ICONS + labels() * LABEL + STATUS_BAR_SPACING_PX;
const needStatus = () => needAll() - labels('launcher') * LABEL;
const resizeTo = (width: number) => act(() => {
    barWidth = width;
    fire((target) => target === bar());
});

const render = async (props: Record<string, unknown> = {}) => {
    await act(async () => root.render(createElement(StatusBar, {
        isConnected: true,
        activePanel: 'remote',
        serverInfo: 'Google Drive',
        connectionSecurity: 'secure',
        secureProtocol: 'HTTPS',
        storageQuota: { used: 15_600, total: 17_000, free: 1_400 },
        onScanUsed: () => {},
        onToggleTransferQueue: () => {},
        onToggleActivityLog: () => {},
        onToggleAeroFile: () => {},
        onToggleCloud: () => {},
        onToggleSync: () => {},
        onToggleAeroAgent: () => {},
        ...props,
    } as never)));
};

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    observers = [];
    frames = [];
    LABEL = LABEL_W;
    nameFull = NAME_FULL;
    vi.stubGlobal('ResizeObserver', class {
        private entry: { cb: () => void; targets: Set<Element> };
        constructor(cb: () => void) { this.entry = { cb, targets: new Set() }; observers.push(this.entry); }
        observe(target: Element) { this.entry.targets.add(target); }
        disconnect() { this.entry.targets.clear(); }
    });
    vi.stubGlobal('requestAnimationFrame', (cb: () => void) => frames.push(cb));
    vi.stubGlobal('cancelAnimationFrame', () => {});
    vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockImplementation(function (this: HTMLElement) {
        if (this.getAttribute('role') === 'status') return barWidth;
        return this === left() ? leftWidth : 0;
    });
    vi.spyOn(Element.prototype, 'getBoundingClientRect').mockImplementation(function (this: Element) {
        const width = this.hasAttribute('data-statusbar-label') ? LABEL
            : this.hasAttribute('data-statusbar-name') ? NAME
                // The connection block's box honours the min-width held on it.
                : this === left()?.children[0] ? Math.max(2 * DOT + NAME, parseFloat((this as HTMLElement).style.minWidth) || 0)
                    : this.parentElement === left() ? ITEM
                        : this.parentElement === left()?.children[0] ? DOT : 0;
        return { width, height: 0, x: 0, y: 0, top: 0, left: 0, right: width, bottom: 0, toJSON: () => ({}) } as DOMRect;
    });
    vi.spyOn(HTMLElement.prototype, 'scrollWidth', 'get').mockImplementation(function (this: HTMLElement) {
        if (this.hasAttribute('data-statusbar-name')) return nameFull;
        if (this !== right()) return 0;
        const inFlow = compact() === 'all' ? 0 : compact() === 'launchers' ? labels('status') : labels();
        return ICONS + inFlow * LABEL;
    });
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
});

it('clips the left cluster and keeps the identity from shrinking under the right one', async () => {
    barWidth = 3000;
    await render();
    expect(left()!.className).toContain('overflow-hidden');
    const [connection, badge, separator, path] = Array.from(left()!.children) as HTMLElement[];
    expect(connection.textContent).toContain('Google Drive');
    expect(badge.textContent).toContain('HTTPS');
    for (const el of [badge, separator]) expect(el.className).toContain('shrink-0');
    // The name truncates (and keeps its full text as a title) instead of
    // spilling out of the cluster.
    expect(connection.className).toContain('min-w-0');
    const name = connection.querySelector('[data-statusbar-name]') as HTMLElement;
    expect(name.className).toContain('truncate');
    expect(name.title).toBe('Google Drive');
    expect(path.hasAttribute('data-statusbar-path')).toBe(true);
    // The storage figure is a status label: it is the last to go.
    expect(right()!.querySelector('[data-storage-chip] [data-statusbar-label="status"]')).not.toBeNull();
    expect(labels('launcher')).toBeGreaterThan(0);
});

it('drops the launcher labels first and the status labels last, by measure', async () => {
    barWidth = 3000;
    await render();
    expect(compact()).toBeNull();
    await resizeTo(needAll() - 1);
    expect(compact()).toBe('launchers');
    await resizeTo(needStatus() - 1);
    expect(compact()).toBe('all');
    // Below the compact width the identity keeps its room, the name down to
    // its minimum.
    expect(left()!.style.minWidth).toBe(`${identity() - (NAME_FULL - STATUS_BAR_NAME_MIN_PX)}px`);
    await resizeTo(needStatus() + STATUS_BAR_HYSTERESIS_PX);
    expect(compact()).toBe('launchers');
    await resizeTo(needAll() + STATUS_BAR_HYSTERESIS_PX);
    expect(compact()).toBeNull();
});

it('holds one state at each threshold across repeated resize events', async () => {
    barWidth = 3000;
    await render();
    for (const [need, below, above] of [[needAll(), 'launchers', null], [needStatus(), 'all', 'launchers']] as const) {
        await resizeTo(need - 1);
        for (let i = 0; i < 5; i += 1) {
            await resizeTo(need);
            expect(compact()).toBe(below);
        }
        for (let i = 0; i < 5; i += 1) {
            await resizeTo(need + STATUS_BAR_HYSTERESIS_PX);
            expect(compact()).toBe(above);
        }
        await resizeTo(need - 1);
    }
});

it('lets the path give way before the server name truncates', async () => {
    barWidth = 3000;
    await render();
    const connection = left()!.children[0] as HTMLElement;
    const natural = 2 * DOT + NAME_FULL;
    const others = identity() - natural;
    const hold = async (room: number) => {
        leftWidth = others + room;
        await resizeTo(barWidth);
        return connection.style.minWidth;
    };
    // Room for the whole name: it is held untruncated and the path shrinks.
    expect(await hold(natural + 40)).toBe(`${natural}px`);
    // Less room: the name takes all of it, never less than its minimum.
    expect(await hold(natural - 30)).toBe(`${natural - 30}px`);
    expect(await hold(10)).toBe(`${natural - (NAME_FULL - STATUS_BAR_NAME_MIN_PX)}px`);
});

it('gives the room back when the server name gets shorter', async () => {
    barWidth = 3000;
    await render();
    const connection = left()!.children[0] as HTMLElement;
    leftWidth = 3000;
    await resizeTo(needAll() - 1);
    expect(compact()).toBe('launchers');
    expect(connection.style.minWidth).toBe(`${2 * DOT + NAME_FULL}px`);
    // Same mounted block, shorter name: its held minimum must not count.
    nameFull = 30;
    await render({ serverInfo: 'Box' });
    await act(async () => { await Promise.resolve(); });
    expect(connection.style.minWidth).toBe(`${2 * DOT + 30}px`);
    expect(compact()).toBeNull();
});

it('re-measures when the text gets wider with no resize and no DOM change', async () => {
    barWidth = 3000;
    await render();
    await resizeTo(needAll() + STATUS_BAR_HYSTERESIS_PX);
    expect(compact()).toBeNull();
    // Another font family: every label wider, the bar and its DOM unchanged.
    LABEL = LABEL_W + 10;
    await act(async () => {
        fire((target) => target === right());
        flushFrames();
    });
    expect(compact()).toBe('launchers');
    // Back to the narrower font: only the hidden labels change width, the
    // cluster around them does not, and the labels still come back.
    LABEL = LABEL_W;
    await act(async () => {
        fire((target) => target.hasAttribute('data-statusbar-label') && target.getAttribute('data-statusbar-label') === 'launcher');
        flushFrames();
    });
    expect(compact()).toBeNull();
});

it('re-measures when content changes without a resize', async () => {
    barWidth = 3000;
    await render({ storageQuota: undefined, onScanUsed: undefined });
    await resizeTo(needAll());
    expect(compact()).toBeNull();
    // The storage chip arriving makes the labeled cluster wider than the bar.
    await render();
    await act(async () => { await Promise.resolve(); });
    expect(compact()).not.toBeNull();
});

it('decides with hysteresis: leaving a level needs the band, entering does not', () => {
    const fit = { avail: 0, leftMin: 300, rightIcons: 300, launchers: 200, status: 100 };
    const base = fit.leftMin + fit.rightIcons + STATUS_BAR_SPACING_PX;
    const all = base + fit.status + fit.launchers;
    expect(decideCompactLevel({ ...fit, avail: all }, 0)).toBe(0);
    expect(decideCompactLevel({ ...fit, avail: all - 1 }, 0)).toBe(1);
    expect(decideCompactLevel({ ...fit, avail: all }, 1)).toBe(1);
    expect(decideCompactLevel({ ...fit, avail: all + STATUS_BAR_HYSTERESIS_PX }, 1)).toBe(0);
    expect(decideCompactLevel({ ...fit, avail: base + fit.status - 1 }, 0)).toBe(2);
    expect(decideCompactLevel({ ...fit, avail: base + fit.status }, 2)).toBe(2);
    expect(decideCompactLevel({ ...fit, avail: base + fit.status + STATUS_BAR_HYSTERESIS_PX }, 2)).toBe(1);
});
