// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import overwriteHook from '../hooks/useOverwriteCheck.ts?raw';
import { describe, expect, it, vi } from 'vitest';
import { usesProviderApi, type RemoteFile } from '../types';
import { loadOverwriteDestination } from './overwriteDestination';
import { fileBatchCommand, folderTransferIsComplete, keepsSingleFilePath } from './fileBatchRouting';
import { createFileBatchDispatcher, rearmedOnUserAction } from '../components/transferQueueActions';

// Run the actual App transfer closures with a mocked IPC boundary. This catches
// a caller that forgets to pass the snapshot, lists inside the loop, or writes
// after a failed preflight, without mounting the entire Tauri/Monaco UI.
function closure(name: string, endMarker: string) {
  const start = app.indexOf(`\n  const ${name} =`);
  const end = app.indexOf(endMarker, start + 1);
  if (start < 0 || end < 0) throw new Error(`Missing App closure: ${name}`);
  return app.slice(start, end);
}
const { code } = await transformWithOxc(
  (app.includes('const getOverwriteDestination =') ? closure('getOverwriteDestination', '\n  // Helper: check folder overwrite') : '')
  + closure('downloadFile', '\n  const uploadFile =')
  + closure('uploadFile', '\n  // Keep restore-retry')
  + closure('runFileBatch', '\n  const uploadMultipleFiles ='),
  'overwrite-wiring.ts',
);
const { code: overwriteCode } = await transformWithOxc(
  overwriteHook.slice(overwriteHook.indexOf('  const checkOverwrite ='), overwriteHook.indexOf('  const resetOverwriteSettings =')),
  'overwrite-check.ts',
);

type Direction = 'upload' | 'download';
type Policy = 'skip' | 'overwrite' | 'resume' | 'rename';
const file = (name: string, size = 40): RemoteFile => ({ name, path: `/actual/${name}`, size, modified: null, is_dir: false, permissions: null });
function fixture(options: { direction: Direction; policy?: Policy; destination?: RemoteFile[]; panel?: RemoteFile[]; failListing?: boolean; protocol?: 'webdav' | 'sftp' }) {
  const direction = options.direction;
  const protocol = options.protocol || 'webdav';
  const destination = options.destination ?? [file('a.bin')];
  const panel = options.panel ?? [];
  const invoke = vi.fn(async (command: string, args?: Record<string, any>) => {
    if (['get_local_files', 'provider_list_files'].includes(command)) {
      if (options.failListing) throw new Error('Destination listing refused');
      return command === 'get_local_files' ? destination : { files: destination, current_path: '/panel' };
    }
    if (command === 'provider_download_files_batch' || command === 'provider_upload_files_batch') {
      const entries = args!.params.entries;
      return { completed: entries.length, failed: 0, cancelled: false, succeeded: entries.map((_: unknown, i: number) => i) };
    }
    if (['reset_cancel_flag', 'provider_download_file', 'provider_upload_file'].includes(command)) return undefined;
    throw new Error(`Unexpected IPC: ${command}`);
  });
  // Execute the real hook policy as well as the real App batch planner. This
  // makes duplicate-name regressions exercise suffix selection, not a stub.
  const checkOverwrite = vi.fn(new Function('useCallback', 'localFiles', 'remoteFiles', 'fileExistsAction', 'overwriteApplyToAllRef', 'setOverwriteDialog',
    `${overwriteCode}; return checkOverwrite;`)((fn: unknown) => fn, panel, panel, options.policy || 'skip',
      { current: { enabled: false, action: 'overwrite' } }, () => { throw new Error('Unexpected interactive overwrite'); }));
  const resolveDestination = vi.fn(loadOverwriteDestination);
  const queueItemsRef = { current: [] as { id: string; status: string }[] };
  const transferQueue = {
    addItem: vi.fn(() => {
      const id = `q${queueItemsRef.current.length}`;
      queueItemsRef.current.push({ id, status: 'pending' });
      return id;
    }),
    startTransfer: vi.fn(), completeTransfer: vi.fn(), failTransfer: vi.fn(),
  };
  const notify = { info: vi.fn(), error: vi.fn() };
  const resetOverwriteSettings = vi.fn();
  const context = {
    invoke, checkOverwrite, loadOverwriteDestination: resolveDestination, resetOverwriteSettings,
    sessions: [{ id: 'A', connectionParams: { protocol } }], activeSessionId: 'A', connectionParams: { protocol },
    usesProviderApi, aeroVaultOverlaySession: null, currentRemotePath: '/panel',
    localFiles: direction === 'upload' ? [file('a.bin', 100)] : panel,
    remoteFiles: direction === 'download' ? [file('a.bin', 100)] : panel,
    humanLog: { logStart: () => 'log', logRaw: vi.fn(), log: vi.fn(), logError: vi.fn(), updateEntry: vi.fn() },
    pendingFileLogIds: { current: new Map() }, batchCancelledRef: { current: false }, cancelLevelRef: { current: 0 },
    circuitBreaker: { reset: vi.fn() }, retryCallbacksRef: { current: new Map() }, queueItemsRef, transferQueue,
    recordQueueDescriptor: vi.fn(), registerFileBatchRows: vi.fn(), fileBatchCommand, keepsSingleFilePath,
    folderTransferIsComplete, createFileBatchDispatcher, rearmedOnUserAction,
    joinRemotePath: (dir: string, name: string) => `${dir}/${name}`,
    settings: { autoStartTransfers: true }, fileExistsAction: options.policy || 'skip',
    effectiveMaxConcurrentTransfers: 4, retryCount: 1, timeoutSeconds: 30,
    downloadSegments: 0, sftpDownloadPreset: 'balanced', supportsFtpTransferPresets: false,
    buildSftpDownloadPresetPayload: () => ({}), resolveFtpDownloadSegments: () => 0,
    formatBytes: (size: number) => String(size), t: (key: string) => key, notify,
  };
  const runtime = new Function(...Object.keys(context), `${code}; return { downloadFile, uploadFile, runFileBatch };`)(...Object.values(context));
  const writers = () => invoke.mock.calls.filter(([name]) => /provider_(upload|download)_(file|files_batch)$/.test(name));
  const listings = () => invoke.mock.calls.filter(([name]) => ['get_local_files', 'provider_list_files'].includes(name));
  return { runtime, invoke, checkOverwrite, resolveDestination, notify, transferQueue, listings, writers, resetOverwriteSettings };
}

const items = ['a.bin', 'b.bin'].map(name => ({ name, sourcePath: `/source/${name}`, size: 100, modified: null }));
const single = (f: ReturnType<typeof fixture>, direction: Direction) => direction === 'download'
  ? f.runtime.downloadFile('/source/a.bin', 'a.bin', '/actual', false, 100)
  : f.runtime.uploadFile('/source/a.bin', 'a.bin', false, 100, false, undefined, '/actual');

describe.each(['download', 'upload'] as const)('App actual %s destination', direction => {
  it('skips a single file that exists only in the destination', async () => {
    const f = fixture({ direction });
    expect(await single(f, direction)).toBe(false);
    expect(f.listings()).toHaveLength(1);
    expect(f.writers()).toHaveLength(0);
    expect(f.resolveDestination).toHaveBeenCalledWith(expect.objectContaining({ direction, path: '/actual', isProviderSession: true }), f.invoke);
  });
  it('transfers a single file into an empty destination despite a panel conflict', async () => {
    const f = fixture({ direction, destination: [], panel: [file('a.bin')] });
    expect(await single(f, direction)).toBe(true);
    expect(f.writers()).toHaveLength(1);
    const [, args] = f.writers()[0];
    expect(args![direction === 'download' ? 'localPath' : 'remotePath']).toBe('/actual/a.bin');
  });
  it('loads a batch destination once and reuses it across all checks', async () => {
    const f = fixture({ direction });
    const result = await f.runtime.runFileBatch(direction, items, '/actual');
    expect(result).toMatchObject({ skipped: 1, completed: 1, failed: 0, aborted: false });
    expect(f.listings()).toHaveLength(1);
    expect(f.checkOverwrite).toHaveBeenCalledTimes(2);
    expect(f.checkOverwrite.mock.calls[0][5]).toBe(f.checkOverwrite.mock.calls[1][5]);
    expect(f.writers()).toHaveLength(1);
    expect(f.writers()[0][1]!.params.entries[0].display_name).toBe('b.bin');
  });
  it('keeps an SFTP destination that exists only off-panel on the delta path', async () => {
    const f = fixture({ direction, protocol: 'sftp', policy: 'overwrite' });
    expect(await f.runtime.runFileBatch(direction, items, '/actual')).toMatchObject({ completed: 2 });
    expect(f.writers().map(([name]) => name)).toEqual([`provider_${direction}_files_batch`, `provider_${direction}_file`]);
  });
  it('aborts the batch before queuing or writing when its listing fails', async () => {
    const f = fixture({ direction, failListing: true });
    expect(await f.runtime.runFileBatch(direction, items, '/actual')).toMatchObject({ failed: 2, aborted: true, sent: 0, succeededSources: [] });
    expect(f.writers()).toHaveLength(0);
    expect(f.transferQueue.addItem).not.toHaveBeenCalled();
    expect(f.checkOverwrite).not.toHaveBeenCalled();
    expect(f.notify.error).toHaveBeenCalledWith(`toast.${direction}Failed`, 'Error: Destination listing refused');
    expect(f.resetOverwriteSettings).toHaveBeenCalled();
  });
  it('reports a failed single-file listing without writing', async () => {
    const f = fixture({ direction, failListing: true });
    expect(await single(f, direction)).toBe(false);
    expect(f.writers()).toHaveLength(0);
    expect(f.checkOverwrite).not.toHaveBeenCalled();
    expect(f.notify.error).toHaveBeenCalled();
  });
});

it('preserves resume on an off-panel remote partial', async () => {
  const f = fixture({ direction: 'upload', policy: 'resume' });
  expect(await f.runtime.runFileBatch('upload', items, '/actual')).toMatchObject({ completed: 2 });
  expect(f.writers()[1]).toEqual(['provider_upload_file', expect.objectContaining({ remotePath: '/actual/a.bin', resume: true })]);
});
it('keeps duplicate destinations out of simultaneous batch writes', async () => {
  const f = fixture({ direction: 'upload', destination: [], policy: 'overwrite' });
  await f.runtime.runFileBatch('upload', [items[0], { ...items[0], sourcePath: '/another/a.bin' }], '/actual');
  expect(f.writers().map(([name]) => name)).toEqual(['provider_upload_files_batch', 'provider_upload_file']);
});

it.each(['upload', 'download'] as const)('reserves rename targets for duplicate %s source names', async direction => {
  const f = fixture({ direction, destination: [file('a.bin'), file('a (1).bin')], policy: 'rename' });
  const result = await f.runtime.runFileBatch(direction, [items[0], { ...items[0], sourcePath: '/another/a.bin' }], '/actual');
  expect(result).toMatchObject({ completed: 2, failed: 0 });
  const paths = f.writers().flatMap(([command, args]) => command.endsWith('_files_batch')
    ? args!.params.entries.map((entry: any) => entry[direction === 'upload' ? 'remote_path' : 'local_path'])
    : [args![direction === 'upload' ? 'remotePath' : 'localPath']]);
  expect(paths).toEqual(['/actual/a (2).bin', '/actual/a (3).bin']);
});

it.each(['upload', 'download'] as const)('preserves both same-name %s sources in an initially empty destination', async direction => {
  const f = fixture({ direction, destination: [], policy: 'rename' });
  await f.runtime.runFileBatch(direction, [items[0], { ...items[0], sourcePath: '/another/a.bin' }], '/actual');
  const paths = f.writers().flatMap(([command, args]) => command.endsWith('_files_batch')
    ? args!.params.entries.map((entry: any) => entry[direction === 'upload' ? 'remote_path' : 'local_path'])
    : [args![direction === 'upload' ? 'remotePath' : 'localPath']]);
  expect(paths).toEqual(['/actual/a.bin', '/actual/a (1).bin']);
});
