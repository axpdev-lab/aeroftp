// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import { loadOverwriteDestination, type OverwriteDestination } from './overwriteDestination';

const target = (options: Partial<OverwriteDestination> = {}): OverwriteDestination => ({
  direction: 'upload', path: '/actual', isProviderSession: true, visibleRemotePath: '/panel', ...options,
});
const entry = { name: '.a.bin', path: '/actual/.a.bin', size: 40, modified: null, is_dir: false, permissions: null };

it('lists hidden local entries at the actual download path', async () => {
  const invoke = vi.fn().mockResolvedValue([entry]);
  await expect(loadOverwriteDestination(target({ direction: 'download' }), invoke)).resolves.toEqual([entry]);
  expect(invoke.mock.calls).toEqual([['get_local_files', { path: '/actual', showHidden: true }]]);
});
it('reads provider destination metadata without changing cwd or using current_path', async () => {
  const invoke = vi.fn().mockResolvedValue({ files: [entry], current_path: '/panel' });
  await expect(loadOverwriteDestination(target(), invoke)).resolves.toEqual([entry]);
  expect(invoke.mock.calls).toEqual([['provider_list_files', { path: '/actual' }]]);
});
it.each(['download', 'upload'] as const)('propagates %s listing failures', async direction => {
  const error = new Error('permission denied');
  const invoke = vi.fn().mockRejectedValue(error);
  await expect(loadOverwriteDestination(target({ direction }), invoke)).rejects.toBe(error);
});
it.each(['download', 'upload'] as const)('preserves explicitly empty %s snapshots', async direction => {
  const invoke = vi.fn().mockResolvedValue(direction === 'download' ? [] : { files: [], current_path: '/panel' });
  await expect(loadOverwriteDestination(target({ direction }), invoke)).resolves.toEqual([]);
});
it.each(['download', 'upload'] as const)('refuses malformed %s snapshots rather than falling back to a panel', async direction => {
  const invoke = vi.fn().mockResolvedValue(direction === 'download' ? null : { current_path: '/panel' });
  await expect(loadOverwriteDestination(target({ direction }), invoke)).rejects.toThrow('Invalid');
});
describe.each(['vault', 'legacy'])('%s session scope', kind => {
  const scope = kind === 'vault' ? { aeroVaultSessionId: 'vault-A' } : { isProviderSession: false };
  it('refuses an off-panel target without invoking navigation or a listing', async () => {
    const invoke = vi.fn();
    await expect(loadOverwriteDestination(target(scope), invoke)).rejects.toThrow('Cannot inspect another');
    expect(invoke).not.toHaveBeenCalled();
  });
  it('refreshes the current destination without supplying a navigation path', async () => {
    const invoke = vi.fn().mockResolvedValue({ files: [entry], current_path: '/actual' });
    await expect(loadOverwriteDestination(target({ ...scope, visibleRemotePath: '/actual' }), invoke)).resolves.toEqual([entry]);
    expect(invoke.mock.calls).toEqual(kind === 'vault'
      ? [['aerovault_overlay_list', { sessionId: 'vault-A', path: null }]] : [['list_files']]);
  });
  it('rejects a listing of a different current directory', async () => {
    const invoke = vi.fn().mockResolvedValue({ files: [], current_path: '/other' });
    await expect(loadOverwriteDestination(target({ ...scope, visibleRemotePath: '/actual' }), invoke)).rejects.toThrow('Remote destination changed');
  });
});
