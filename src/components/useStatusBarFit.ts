// Decides when the status bar drops its button labels and shows icons only.
//
// It used to be a viewport media query at 1100px, a width guessed for the
// English labels with no optional chip on screen. Above it the labeled right
// cluster took the whole bar and the connection block (server name and the
// HTTPS badge) was drawn underneath the storage chip; a longer locale or one
// more chip moved the real threshold anywhere. The decision is now measured
// on the bar itself, in two steps:
//   1. the launcher labels go first (AeroFile, AeroCloud, AeroSync...): their
//      icons are the buttons people already know and each keeps its title;
//   2. the status labels go last (storage figure, Queue, Log, progress):
//      they carry information, not a name.
//
// Every width is exact in every state, so hiding labels can never make them
// fit again by itself: a hidden label is taken out of the flow (styles.css,
// `data-compact`) but keeps its width, and that width is added back. The
// hysteresis band only absorbs sub-pixel noise.
import { useLayoutEffect, type RefObject } from 'react';

/** Room kept for the current path when the labels compete with it. */
export const STATUS_BAR_PATH_MIN_PX = 80;
/** Room the server name keeps when it truncates. */
export const STATUS_BAR_NAME_MIN_PX = 64;
/** Room kept between the left and the right cluster. */
export const STATUS_BAR_SPACING_PX = 16;
/** Extra room needed before hidden labels come back. */
export const STATUS_BAR_HYSTERESIS_PX = 8;

/** 0: every label shown. 1: launcher labels hidden. 2: every label hidden. */
export type StatusBarCompactLevel = 0 | 1 | 2;
const ATTR_BY_LEVEL = { 1: 'launchers', 2: 'all' } as const;

export interface StatusBarFit {
    /** Content width of the bar. */
    avail: number;
    /** Width the left cluster needs: identity plus the minimum path. */
    leftMin: number;
    /** Width of the right cluster with every label hidden. */
    rightIcons: number;
    /** Width the launcher labels add, gaps included. */
    launchers: number;
    /** Width the status labels add, gaps included. */
    status: number;
}

export function decideCompactLevel(fit: StatusBarFit, current: StatusBarCompactLevel): StatusBarCompactLevel {
    const base = fit.leftMin + fit.rightIcons + STATUS_BAR_SPACING_PX;
    // Coming back to a level with more labels needs the band on top.
    const fits = (need: number, level: StatusBarCompactLevel) =>
        fit.avail >= need + (current > level ? STATUS_BAR_HYSTERESIS_PX : 0);
    if (fits(base + fit.status + fit.launchers, 0)) return 0;
    if (fits(base + fit.status, 1)) return 1;
    return 2;
}

export function readCompactLevel(bar: HTMLElement): StatusBarCompactLevel {
    const attr = bar.getAttribute('data-compact');
    return attr === 'all' ? 2 : attr === 'launchers' ? 1 : 0;
}

const px = (value: string | undefined): number => {
    const n = parseFloat(value ?? '');
    return Number.isFinite(n) ? n : 0;
};
const width = (el: Element): number => el.getBoundingClientRect().width;

// Width the element would take untruncated: the server name shrinks once the
// labels are gone, and the measure must not depend on that.
function naturalWidth(child: Element): { natural: number; name: number } {
    const name = child.querySelector<HTMLElement>('[data-statusbar-name]');
    if (!name) return { natural: width(child), name: 0 };
    const cap = px(getComputedStyle(name).maxWidth) || Infinity;
    const nameNatural = Math.min(name.scrollWidth, cap);
    return { natural: width(child) - width(name) + nameNatural, name: nameNatural };
}

interface IdentityFit {
    identity: number;
    identityMin: number;
    /** Block holding the server name, with its untruncated width. */
    connection?: { el: HTMLElement; natural: number; min: number };
}

export function measureStatusBar(bar: HTMLElement, left: HTMLElement, right: HTMLElement): StatusBarFit & IdentityFit {
    const barStyle = getComputedStyle(bar);
    const avail = bar.clientWidth - px(barStyle.paddingLeft) - px(barStyle.paddingRight);

    const leftGap = px(getComputedStyle(left).columnGap);
    const children = Array.from(left.children);
    let identity = 0;
    let identityCount = 0;
    let nameSlack = 0;
    let hasPath = false;
    let connection: IdentityFit['connection'];
    for (const child of children) {
        if (child.hasAttribute('data-statusbar-path')) { hasPath = true; continue; }
        const { natural, name } = naturalWidth(child);
        const slack = Math.max(0, name - STATUS_BAR_NAME_MIN_PX);
        if (name > 0) connection = { el: child as HTMLElement, natural, min: natural - slack };
        identity += natural;
        nameSlack += slack;
        identityCount += 1;
    }
    identity += leftGap * Math.max(0, identityCount - 1);
    const leftMin = hasPath ? identity + leftGap + STATUS_BAR_PATH_MIN_PX : identity;

    // A label in the flow is part of scrollWidth; a hidden one is not. Each
    // one is counted with the gap it has inside its button.
    const level = readCompactLevel(bar);
    let rightIcons = right.scrollWidth;
    let launchers = 0;
    let status = 0;
    right.querySelectorAll<HTMLElement>('[data-statusbar-label]').forEach((label) => {
        const owner = label.parentElement;
        const w = width(label) + (owner ? px(getComputedStyle(owner).columnGap) : 0);
        const isLauncher = label.getAttribute('data-statusbar-label') === 'launcher';
        const hidden = level === 2 || (level === 1 && isLauncher);
        if (!hidden) rightIcons -= w;
        if (isLauncher) launchers += w;
        else status += w;
    });
    return { avail, leftMin, rightIcons, launchers, status, identity, identityMin: identity - nameSlack, connection };
}

export function fitStatusBar(bar: HTMLElement, left: HTMLElement, right: HTMLElement): void {
    const fit = measureStatusBar(bar, left, right);
    const current = readCompactLevel(bar);
    const level = decideCompactLevel(fit, current);
    if (level !== current) {
        if (level === 0) bar.removeAttribute('data-compact');
        else bar.setAttribute('data-compact', ATTR_BY_LEVEL[level]);
    }
    // Below the compact width the server name truncates to its minimum, then
    // the right cluster gives way: the name and the security badge stay.
    const minWidth = `${Math.ceil(Math.max(0, Math.min(fit.identityMin, fit.avail)))}px`;
    if (left.style.minWidth !== minWidth) left.style.minWidth = minWidth;
    // Inside the left cluster the path gives way before the name: flex alone
    // would shrink both in proportion, so the name block is held at the room
    // the rest of the identity leaves it, between its minimum and its
    // untruncated width. The cluster's own width does not depend on this.
    if (fit.connection) {
        const { el, natural, min } = fit.connection;
        const room = left.clientWidth - (fit.identity - natural);
        const hold = room >= natural ? Math.ceil(natural) : Math.floor(Math.max(min, room));
        const value = `${Math.max(0, hold)}px`;
        if (el.style.minWidth !== value) el.style.minWidth = value;
    }
}

/**
 * Keeps `data-compact` on the bar in step with its width and its content.
 * The bar's size is watched with a ResizeObserver; content changes (a chip
 * appearing, a language switch, AeroShare rendering on its own) with a
 * MutationObserver, which does not see the attributes this hook writes.
 */
export function useStatusBarFit(
    barRef: RefObject<HTMLElement | null>,
    leftRef: RefObject<HTMLElement | null>,
    rightRef: RefObject<HTMLElement | null>,
): void {
    useLayoutEffect(() => {
        const bar = barRef.current;
        const left = leftRef.current;
        const right = rightRef.current;
        if (!bar || !left || !right) return undefined;
        const fit = () => fitStatusBar(bar, left, right);
        fit();
        const resize = typeof ResizeObserver === 'undefined' ? undefined : new ResizeObserver(fit);
        resize?.observe(bar);
        const mutation = typeof MutationObserver === 'undefined' ? undefined : new MutationObserver(fit);
        mutation?.observe(bar, { subtree: true, childList: true, characterData: true, attributeFilter: ['class'] });
        return () => {
            resize?.disconnect();
            mutation?.disconnect();
        };
    }, [barRef, leftRef, rightRef]);
}
