// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
import { BreadcrumbBar, splitPath } from './BreadcrumbBar';
import { TID } from '../utils/testIds';

let host: HTMLDivElement;
let root: Root;
let resize: () => void;
const anchor = '/top/ciphertext';
const onNavigate = vi.fn();
const listSubdirectories = vi.fn(async () => [{ name: 'sub', path: `${anchor}/sub` }]);

beforeEach(() => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  vi.clearAllMocks();
  vi.stubGlobal('ResizeObserver', class {
    constructor(callback: () => void) { resize = callback; }
    observe() {}
    disconnect() {}
  });
  host = document.createElement('div');
  document.body.append(host);
  root = createRoot(host);
});

afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

async function render(currentPath = `${anchor}/sub`, segmentLabels: Record<string, string> | undefined = { [anchor]: 'Archivio è' }) {
  await act(async () => root.render(createElement(BreadcrumbBar, {
    panel: 'remote', currentPath, segmentLabels, onNavigate, listSubdirectories, t: (key: string) => key,
  })));
}

async function click(element: Element) {
  await act(async () => (element as HTMLElement).click());
}

it('shows decrypted anchor labels while segment clicks and tooltips keep canonical paths', async () => {
  await render();
  const button = host.querySelector(`button[title="${anchor}"]`)!;
  expect(button.textContent).toBe('Archivio è');
  expect(host.querySelector('[aria-current="page"]')?.textContent).toBe('sub');
  await click(button);
  expect(onNavigate).toHaveBeenCalledWith(anchor);
});

it('edits and submits the canonical path, even when an anchor has a display label', async () => {
  await render();
  await click(host.querySelector(`[data-testid="${TID.breadcrumbEdit}"]`)!);
  const input = host.querySelector('input')!;
  expect(input.value).toBe(`${anchor}/sub`);
  // The ordinary input accepts an actual path, never the decoded label.
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, anchor);
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });
  await click(host.querySelector(`[data-testid="${TID.breadcrumbConfirm}"]`)!);
  expect(onNavigate).toHaveBeenCalledWith(anchor);
});

it('browses subdirectories through the canonical anchor path', async () => {
  await render(anchor);
  await click(host.querySelector('button[title="breadcrumb.browseChildren"]')!);
  expect(listSubdirectories).toHaveBeenCalledWith(anchor);
  await click(Array.from(host.querySelectorAll('button')).find(button => button.textContent === 'sub')!);
  expect(onNavigate).toHaveBeenCalledWith(`${anchor}/sub`);
});

it('uses labels in sibling menus without changing their destinations', async () => {
  listSubdirectories.mockResolvedValueOnce([{ name: 'ciphertext', path: anchor }]);
  await render(anchor);
  const chevrons = host.querySelectorAll('button[title="breadcrumb.browseSiblings"]');
  await click(chevrons[1]);
  expect(listSubdirectories).toHaveBeenCalledWith('/top');
  const menuButton = Array.from(host.querySelectorAll('button')).find(button => button.textContent === 'Archivio è' && !button.title)!;
  expect(menuButton.className).toContain('font-medium');
  await click(menuButton);
  expect(onNavigate).toHaveBeenCalledWith(anchor);
});

it('uses labels in collapsed ancestors while keeping their navigation paths', async () => {
  await render(`${anchor}/a/b/c/d`);
  const segments = host.querySelector('nav > div')!;
  Object.defineProperties(segments, { scrollWidth: { value: 500 }, clientWidth: { value: 100 } });
  await act(async () => resize());
  await click(host.querySelector('button[title="breadcrumb.showAll"]')!);
  const label = Array.from(host.querySelectorAll('button')).find(button => button.textContent === 'Archivio è')!;
  await click(label);
  expect(onNavigate).toHaveBeenCalledWith(anchor);
});

it('falls back to canonical segment names when display labels are removed', async () => {
  await render();
  expect(host.textContent).toContain('Archivio è');
  await render(`${anchor}/sub`, {});
  expect(host.textContent).not.toContain('Archivio è');
  expect(host.querySelector(`button[title="${anchor}"]`)?.textContent).toBe('ciphertext');
});

it('keeps the sync boundary on canonical paths when an ancestor has a label', async () => {
  await act(async () => root.render(createElement(BreadcrumbBar, {
    currentPath: `${anchor}/sub`, minPath: `${anchor}/sub`,
    segmentLabels: { [anchor]: 'Archivio è' }, onNavigate, t: (key: string) => key,
  })));
  const button = Array.from(host.querySelectorAll('button')).find(b => b.textContent === 'Archivio è')!;
  expect(button.disabled).toBe(true);
  await click(button);
  expect(onNavigate).not.toHaveBeenCalled();
});

it('literal_unix_paths breadcrumbs emit exact navigation and listing targets', () => {
    expect(splitPath(String.raw`/tmp/dir\name/child`)).toEqual([
        { segment: '/', fullPath: '/' }, { segment: 'tmp', fullPath: '/tmp' },
        { segment: String.raw`dir\name`, fullPath: String.raw`/tmp/dir\name` },
        { segment: 'child', fullPath: String.raw`/tmp/dir\name/child` },
    ]);
    expect(splitPath(String.raw`/remote/back\slash`, 'remote').slice(-1)[0]?.fullPath).toBe(String.raw`/remote/back\slash`);
    expect(splitPath(String.raw`C:\opaque`, 'remote').slice(-1)[0]?.fullPath).toBe(String.raw`/C:\opaque`);
});
it('keeps Windows drive and UNC breadcrumb navigation absolute', () => {
    expect(splitPath(String.raw`D:\boot\grub`).map(s => s.fullPath)).toEqual(['D:/', 'D:/boot', 'D:/boot/grub']);
    expect(splitPath(String.raw`\\server\share\dir`).map(s => s.fullPath)).toEqual(['//server/share', '//server/share/dir']);
});
it('keeps adapted UNC breadcrumbs on Windows without changing remote paths', () => {
    vi.stubGlobal('navigator', { platform: 'Win32' });
    expect(splitPath('//server/share/dir').map(s => s.fullPath)).toEqual(['//server/share', '//server/share/dir']);
    expect(splitPath('//server/share/dir', 'remote').map(s => s.fullPath)).toEqual(['/', '/server', '/server/share', '/server/share/dir']);
});
it('literal_unix_paths breadcrumb clicks and child listings use the same identity', async () => {
  const target = String.raw`/tmp/dir\name`;
  await act(async () => root.render(createElement(BreadcrumbBar, {
    currentPath: `${target}/child`, onNavigate, listSubdirectories, t: (key: string) => key,
  })));
  await click(host.querySelector(`button[title="${CSS.escape(target)}"]`)!);
  expect(onNavigate).toHaveBeenCalledWith(target);
  const chevrons = host.querySelectorAll('button[title="breadcrumb.browseSiblings"]');
  await click(chevrons[2]);
  expect(listSubdirectories).toHaveBeenCalledWith(target);
});
