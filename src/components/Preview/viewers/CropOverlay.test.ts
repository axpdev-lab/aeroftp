// @vitest-environment jsdom
import { act, createElement, createRef } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { CropOverlay, cropCoversWholeImage } from './CropOverlay';
import type { CropRect } from '../types';

vi.mock('../../../i18n', () => ({ useI18n: () => ({}) }));

// The viewer is 600 x 400; the image is shown at 400 x 300, centred, so it
// starts 100 px from the left and 50 px from the top of the overlay. The
// file itself is 800 x 600.
const OVERLAY = { left: 0, top: 0, width: 600, height: 400 };
const IMAGE = { left: 100, top: 50, width: 400, height: 300 };
const rect = (b: typeof OVERLAY) =>
    ({ ...b, x: b.left, y: b.top, right: b.left + b.width, bottom: b.top + b.height, toJSON: () => b }) as DOMRect;

let root: Root;
let host: HTMLDivElement;
let img: HTMLImageElement;
let crops: CropRect[];

beforeEach(async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div');
    document.body.append(host);
    img = document.createElement('img');
    Object.defineProperty(img, 'naturalWidth', { value: 800 });
    Object.defineProperty(img, 'naturalHeight', { value: 600 });
    img.getBoundingClientRect = () => rect(IMAGE);
    vi.spyOn(HTMLDivElement.prototype, 'getBoundingClientRect').mockImplementation(() => rect(OVERLAY));
    crops = [];
    const imageRef = createRef<HTMLImageElement>() as { current: HTMLImageElement | null };
    imageRef.current = img;
    root = createRoot(host);
    await act(async () => root.render(createElement(CropOverlay, {
        imageRef,
        aspectRatio: null,
        onCropChange: (c: CropRect) => crops.push(c),
        onCancel: () => {},
    })));
    // A second render places the frame once the overlay element exists.
    await act(async () => root.render(createElement(CropOverlay, {
        imageRef,
        aspectRatio: null,
        onCropChange: (c: CropRect) => crops.push(c),
        onCancel: () => {},
    })));
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
    vi.restoreAllMocks();
});

const px = (el: Element | null, prop: 'left' | 'top' | 'width' | 'height') => parseFloat((el as HTMLElement).style[prop]);
const frame = () => host.querySelector('[data-crop-frame]');
const border = () => host.querySelector('[data-crop-border]');
const overlay = () => host.firstElementChild as HTMLElement;
const drag = async (from: [number, number], to: [number, number]) => {
    await act(async () => {
        overlay().dispatchEvent(new MouseEvent('mousedown', { bubbles: true, clientX: from[0], clientY: from[1] }));
        document.dispatchEvent(new MouseEvent('mousemove', { clientX: to[0], clientY: to[1] }));
        document.dispatchEvent(new MouseEvent('mouseup', {}));
    });
};

it('starts with the whole image selected, its handles on the image edges', () => {
    expect(crops[crops.length - 1]).toEqual({ x: 0, y: 0, width: 800, height: 600 });
    expect([px(border(), 'left'), px(border(), 'top'), px(border(), 'width'), px(border(), 'height')]).toEqual([0, 0, 400, 300]);
});

it('draws the selection over the image, not over the margin around it', () => {
    expect([px(frame(), 'left'), px(frame(), 'top')]).toEqual([IMAGE.left, IMAGE.top]);
});

it('reaches the bottom-right part of the image, and draws the selection where the pointer is', async () => {
    // Drag the top-left handle from the image's corner to its centre.
    await drag([IMAGE.left, IMAGE.top], [IMAGE.left + 200, IMAGE.top + 150]);
    expect(crops[crops.length - 1]).toEqual({ x: 400, y: 300, width: 400, height: 300 });
    // On screen the border's corner is where the pointer let go.
    expect(px(frame(), 'left') + px(border(), 'left')).toBe(IMAGE.left + 200);
    expect(px(frame(), 'top') + px(border(), 'top')).toBe(IMAGE.top + 150);
});

it('treats a selection of the whole image as no crop, and anything smaller as a crop', () => {
    expect(cropCoversWholeImage({ x: 0, y: 0, width: 800, height: 600 }, 800, 600)).toBe(true);
    expect(cropCoversWholeImage({ x: 0, y: 0, width: 799, height: 600 }, 800, 600)).toBe(false);
    expect(cropCoversWholeImage({ x: 1, y: 0, width: 800, height: 600 }, 800, 600)).toBe(false);
});
