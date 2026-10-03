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
