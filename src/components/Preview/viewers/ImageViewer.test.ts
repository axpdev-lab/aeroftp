// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { ImageViewer } from './ImageViewer';
import type { ImageResult, PreviewFileData } from '../types';

const spies = vi.hoisted(() => ({ invoke: vi.fn() }));
const translate = (key: string) => key;
vi.mock('../../../i18n', () => ({ useI18n: () => ({ t: translate }) }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: spies.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../../../utils/imagePreviewBg', () => ({
    useImagePreviewBg: () => ({ value: 'checkerboard', style: {} }),
    IMAGE_PREVIEW_BG_PRESETS: [{ id: 'checkerboard' }], writeImagePreviewBg: vi.fn(),
}));
vi.mock('./ImageEditor', () => ({ default: () => null }));

let root: Root;
let host: HTMLDivElement;
const saved = vi.fn();
const file: PreviewFileData = { name: 'photo.png', path: '/pictures/photo.png', size: 123, isRemote: false, blobUrl: 'blob:photo' };
const result: ImageResult = { path: '/pictures/photo_edited.png', format: 'png', width: 400, height: 300, size: 234 };
const mount = async (current = file) => {
    await act(async () => root.render(createElement(ImageViewer, { file: current, onImageSaved: saved })));
};
const click = async (button: Element | null) => {
    expect(button).not.toBeNull();
    await act(async () => button!.dispatchEvent(new MouseEvent('click', { bubbles: true })));
};
const saveCopy = async () => {
    await click(host.querySelector('[data-image-save]'));
    await click([...host.querySelectorAll('button')].find(el => el.textContent === 'preview.image.edit.saveAsCopy') ?? null);
};
const notice = () => host.querySelector('[data-image-save-notice]');

beforeEach(async () => {
    vi.useFakeTimers();
    spies.invoke.mockReset();
    saved.mockClear();
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
    await mount();
    const image = host.querySelector('img')!;
    Object.defineProperties(image, { naturalWidth: { value: 800 }, naturalHeight: { value: 600 } });
    await act(async () => image.dispatchEvent(new Event('load')));
    await click(host.querySelector('button[title="preview.image.edit.editImage"]'));
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
    vi.useRealTimers();
    vi.restoreAllMocks();
});

it('confirms only a completed native save, not a pending one, and dismisses after 1.5 seconds', async () => {
    let resolveSave!: (result: ImageResult) => void;
    spies.invoke.mockReturnValue(new Promise<ImageResult>(resolve => { resolveSave = resolve; }));
    await saveCopy();
    expect(notice()).toBeNull();
    expect(saved).not.toHaveBeenCalled();
    await act(async () => resolveSave(result));
    expect(host.querySelector('[role="dialog"]')).toBeNull();
    expect(notice()?.textContent).toContain('preview.image.edit.saveSaved');
    expect(saved).toHaveBeenCalledExactlyOnceWith(result);
    await act(async () => vi.advanceTimersByTime(1499));
    expect(notice()).not.toBeNull();
    await act(async () => vi.advanceTimersByTime(1));
    expect(notice()).toBeNull();
});

it('keeps errors in the dialog without a success notice or panel refresh', async () => {
    spies.invoke.mockRejectedValue(new Error('disk full'));
    await saveCopy();
    expect(host.querySelector('[role="dialog"]')?.textContent).toContain('disk full');
    expect(notice()).toBeNull();
    expect(saved).not.toHaveBeenCalled();
});

it('gives a repeated save a fresh 1.5-second confirmation', async () => {
    spies.invoke.mockResolvedValueOnce({ ...result }).mockResolvedValueOnce({ ...result });
    await saveCopy();
    await act(async () => vi.advanceTimersByTime(1000));
    await saveCopy();
    await act(async () => vi.advanceTimersByTime(500));
    expect(notice()).not.toBeNull();
    await act(async () => vi.advanceTimersByTime(1000));
    expect(notice()).toBeNull();
    expect(saved).toHaveBeenCalledTimes(2);
});

it('does not carry the previous image confirmation into the next gallery image', async () => {
    spies.invoke.mockResolvedValue({ ...result });
    await saveCopy();
    expect(notice()).not.toBeNull();
    await mount({ ...file, name: 'next.png', path: '/pictures/next.png', blobUrl: 'blob:next' });
    expect(notice()).toBeNull();
});
