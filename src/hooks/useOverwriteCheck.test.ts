// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { useOverwriteCheck } from './useOverwriteCheck';
import type { LocalFile, RemoteFile } from '../types';

type Options = Parameters<typeof useOverwriteCheck>[0];
type API = ReturnType<typeof useOverwriteCheck>;
const file = (name: string, size = 20): RemoteFile => ({ name, path: `/target/${name}`, size, modified: '2026-10-01T12:00:00Z', is_dir: false, permissions: null });
let root: Root;
let host: HTMLDivElement;
let api: API;
const mount = async (options: Partial<Options> = {}) => {
  const Probe = () => {
    api = useOverwriteCheck({ localFiles: [], remoteFiles: [], ...options });
    return null;
  };
  await act(async () => root.render(createElement(Probe)));
};
beforeEach(() => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  host = document.createElement('div');
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

describe.each([true, false])('actual overwrite destination, sourceIsRemote=%s', sourceIsRemote => {
  const panel = (files: RemoteFile[]) => sourceIsRemote ? { localFiles: files } : { remoteFiles: files };
  it('asks for the off-panel destination and uses its metadata', async () => {
    await mount(panel([]));
    let pending: ReturnType<API['checkOverwrite']>;
    await act(async () => { pending = api.checkOverwrite('a.bin', 100, undefined, sourceIsRemote, 3, [file('a.bin', 40)]); });
    expect(api.overwriteDialog.isOpen).toBe(true);
    expect(api.overwriteDialog.destination).toMatchObject({ name: 'a.bin', size: 40, isRemote: !sourceIsRemote });
    expect(api.overwriteDialog.queueCount).toBe(3);
    await act(async () => api.overwriteDialog.resolve!({ action: 'skip', applyToAll: false }));
    await expect(pending!).resolves.toEqual({ action: 'skip', destinationExists: true });
  });
  it('accepts an empty destination even when the panel contains the name', async () => {
    await mount({ ...panel([file('a.bin')]), fileExistsAction: 'skip' });
    await expect(api.checkOverwrite('a.bin', 100, undefined, sourceIsRemote, 0, [])).resolves.toEqual({ action: 'overwrite', destinationExists: false });
    expect(api.overwriteDialog.isOpen).toBe(false);
  });
  it('renames against all destination names, including directories and hidden names', async () => {
    await mount({ ...panel([file('a (3).bin')]), fileExistsAction: 'rename' });
    const destination: LocalFile[] = [file('a.bin'), file('a (1).bin'), { ...file('a (2).bin'), is_dir: true }];
    await expect(api.checkOverwrite('a.bin', 100, undefined, sourceIsRemote, 0, destination)).resolves.toEqual({ action: 'rename', newName: 'a (3).bin', destinationExists: true });
  });
  it('resumes based on the real partial size', async () => {
    await mount({ ...panel([file('a.bin', 200)]), fileExistsAction: 'resume' });
    await expect(api.checkOverwrite('a.bin', 100, undefined, sourceIsRemote, 0, [file('a.bin', 40)])).resolves.toEqual({ action: 'resume', destinationExists: true });
  });
});

it('keeps panel-only callers working when no snapshot is provided', async () => {
  await mount({ remoteFiles: [file('a.bin')], fileExistsAction: 'skip' });
  await expect(api.checkOverwrite('a.bin', 100, undefined, false)).resolves.toEqual({ action: 'skip', destinationExists: true });
});

it('apply-to-all retains real destination existence for subsequent routing', async () => {
  await mount();
  let pending: ReturnType<API['checkOverwrite']>;
  await act(async () => { pending = api.checkOverwrite('a.bin', 100, undefined, false, 1, [file('a.bin')]); });
  expect(api.overwriteDialog.isOpen).toBe(true);
  await act(async () => api.overwriteDialog.resolve!({ action: 'overwrite', applyToAll: true }));
  await pending!;
  await expect(api.checkOverwrite('b.bin', 100, undefined, false, 0, [])).resolves.toEqual({ action: 'overwrite', destinationExists: false });
});

it('asks before a Windows download replaces a differently cased destination', async () => {
  const existing = { ...file('Foo.txt'), path: 'C:/Downloads/Foo.txt' };
  await mount({ localFiles: [existing] });
  let pending: ReturnType<API['checkOverwrite']>;
  await act(async () => { pending = api.checkOverwrite('foo.txt', 100, undefined, true); });
  expect(api.overwriteDialog.isOpen).toBe(true);
  expect(api.overwriteDialog.destination?.name).toBe('Foo.txt');
  await act(async () => api.overwriteDialog.resolve!({ action: 'cancel', applyToAll: false }));
  await expect(pending!).resolves.toMatchObject({ action: 'cancel', destinationExists: true });
});

it('Windows rename avoids differently cased names, including directories', async () => {
  await mount({ fileExistsAction: 'rename' });
  const destination = [
    { ...file('Foo.txt'), path: 'C:/Downloads/Foo.txt' },
    { ...file('FOO (1).TXT'), path: 'C:/Downloads/FOO (1).TXT', is_dir: true },
  ];
  await expect(api.checkOverwrite('foo.txt', 100, undefined, true, 0, destination)).resolves.toEqual({
    action: 'rename', newName: 'foo (2).txt', destinationExists: true,
  });
});

it.each([true, false])('keeps exact Unix and remote names apart (download=%s)', async sourceIsRemote => {
  await mount({ localFiles: [file('Foo.txt')], remoteFiles: [file('Foo.txt')] });
  await expect(api.checkOverwrite('foo.txt', 100, undefined, sourceIsRemote)).resolves.toMatchObject({
    action: 'overwrite', destinationExists: false,
  });
  expect(api.overwriteDialog.isOpen).toBe(false);
});


it.each(['C:/Downloads/Foo.txt', '/target/foo.txt'])('skips a directory colliding with a download at %s', async path => {
  await mount({ localFiles: [{ ...file(path.includes(':') ? 'Foo.txt' : 'foo.txt'), path, is_dir: true }], fileExistsAction: 'skip' });
  await expect(api.checkOverwrite('foo.txt', 100, undefined, true)).resolves.toEqual({ action: 'skip', destinationExists: true });
});

it.each(['ask', 'overwrite', 'resume'] as const)('reports a directory mismatch before a download under %s', async fileExistsAction => {
  await mount({ localFiles: [{ ...file('Foo.txt'), path: 'C:/Downloads/Foo.txt', is_dir: true }], fileExistsAction });
  await expect(api.checkOverwrite('foo.txt', 100, undefined, true)).rejects.toThrow('Invalid path: Download target is a directory');
});

it('renames a download away from a colliding directory', async () => {
  await mount({ localFiles: [{ ...file('Foo.txt'), path: 'C:/Downloads/Foo.txt', is_dir: true }], fileExistsAction: 'rename' });
  await expect(api.checkOverwrite('foo.txt', 100, undefined, true)).resolves.toMatchObject({ action: 'rename', newName: 'foo (1).txt', destinationExists: true });
});


it('allocates a fresh directory-conflict rename even after apply-to-all', async () => {
  await mount({ localFiles: [{ ...file('first.txt'), path: 'C:/Downloads/first.txt' },
    { ...file('Foo.txt'), path: 'C:/Downloads/Foo.txt', is_dir: true }] });
  let first: ReturnType<API['checkOverwrite']>;
  await act(async () => { first = api.checkOverwrite('first.txt', 100, undefined, true); });
  await act(async () => api.overwriteDialog.resolve!({ action: 'rename', newName: 'first (1).txt', applyToAll: true }));
  await first!;
  await expect(api.checkOverwrite('foo.txt', 100, undefined, true)).resolves.toMatchObject({ action: 'rename', newName: 'foo (1).txt', destinationExists: true });
});
