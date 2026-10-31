// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import overwriteHook from '../hooks/useOverwriteCheck.ts?raw';
import { describe, expect, it, vi } from 'vitest';
import { usesProviderApi, type RemoteFile } from '../types';
import { loadOverwriteDestination } from './overwriteDestination';
import { fileBatchCommand, folderTransferIsComplete, keepsSingleFilePath, splitForFileBatch } from './fileBatchRouting';
import { createFileBatchDispatcher, rearmedOnUserAction } from '../components/transferQueueActions';
import { isSafeRemoteDownloadName, remoteDownloadTarget } from './localPath';

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
// The paste: what it hands the batch of a copy to the other side.
const { code: pasteCode } = await transformWithOxc(
  closure('clipboardPaste', '\n  // openDevToolsPreview, openUniversalPreview'),
  'paste-wiring.ts',
);
const { code: overwriteCode } = await transformWithOxc(
  overwriteHook.slice(overwriteHook.indexOf('  const checkOverwrite ='), overwriteHook.indexOf('  const resetOverwriteSettings =')),
  'overwrite-check.ts',
);

type Direction = 'upload' | 'download';
type Policy = 'skip' | 'overwrite' | 'resume' | 'rename';
const file = (name: string, size = 40): RemoteFile => ({ name, path: `/actual/${name}`, size, modified: null, is_dir: false, permissions: null });
function fixture(options: { direction: Direction; policy?: Policy; destination?: RemoteFile[]; panel?: RemoteFile[]; nested?: RemoteFile[]; panel2?: RemoteFile[]; failListing?: boolean; protocol?: 'webdav' | 'sftp' | 'imagekit' }) {
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
    if (command === 'provider_download_folder') return { cancelled: false, failed: 0, skipped: 0 };
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
    isSafeRemoteDownloadName, remoteDownloadTarget,
    sessions: [{ id: 'A', connectionParams: { protocol } }], activeSessionId: 'A', connectionParams: { protocol },
    liveConnectionParams: { protocol },
    usesProviderApi, aeroVaultOverlaySession: null, currentRemotePath: '/panel',
    localFiles: direction === 'upload' ? [file('a.bin', 100)] : panel,
    // The rows shown, which a nested row of a flattened search is found in.
    sortedLocalFiles: options.nested ?? [],
    localFiles2: options.panel2 ?? [], sortedLocalFiles2: options.panel2 ?? [],
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
  const writers = () => invoke.mock.calls.filter(([name]) => /provider_(upload|download)_(file|files_batch|folder)$/.test(name));
  const listings = () => invoke.mock.calls.filter(([name]) => ['get_local_files', 'provider_list_files'].includes(name));
  return { runtime, invoke, checkOverwrite, resolveDestination, notify, transferQueue, listings, writers, resetOverwriteSettings };
}

const items = ['a.bin', 'b.bin'].map(name => ({ name, sourcePath: `/source/${name}`, size: 100, modified: null }));
const single = (f: ReturnType<typeof fixture>, direction: Direction) => direction === 'download'
  ? f.runtime.downloadFile('/source/a.bin', 'a.bin', '/actual', false, 100)
  : f.runtime.uploadFile('/source/a.bin', 'a.bin', false, 100, false, undefined, '/actual');

it('forwards a nested row\'s own size and modified date to checkOverwrite', async () => {
  // A nested row of a flattened search, found by its path: the panel's own
  // `a.bin` has another size and no date, so a lookup by name would show.
  const nested = { ...file('a.bin', 250), path: '/source/a.bin', modified: '2026-01-01T00:00:00Z' };
  const f = fixture({ direction: 'upload', destination: [], nested: [nested] });
  await f.runtime.uploadFile('/source/a.bin', 'a.bin', false, 0, false, undefined, '/actual');
  expect(f.checkOverwrite.mock.calls[0][1]).toBe(250);
  expect(f.checkOverwrite.mock.calls[0][2]).toEqual(new Date('2026-01-01T00:00:00Z'));
});

describe('a file of panel 2 brings its own size and modified date to the overwrite check', () => {
  // Panel 1 does not list it, and panel 1's `a.bin` is another file.
  const panel2 = ['a.bin', 'b.bin'].map((name, i) => ({ ...file(name, 300 + i), path: `/panel2/${name}`, modified: '2026-02-01T00:00:00Z' }));

  it('uploaded on its own', async () => {
    const f = fixture({ direction: 'upload', destination: [], panel2 });
    await f.runtime.uploadFile('/panel2/a.bin', 'a.bin', false, 0, false, undefined, '/actual');
    expect(f.checkOverwrite.mock.calls[0][1]).toBe(300);
    expect(f.checkOverwrite.mock.calls[0][2]).toEqual(new Date('2026-02-01T00:00:00Z'));
  });

  it('copied and pasted on the server, as one batch', async () => {
    const runFileBatch = vi.fn(async (_direction: string, entries: { sourcePath: string }[]) => (
      { succeededSources: entries.map(e => e.sourcePath), aborted: false }));
    const context = {
      fileClipboardRef: { current: { files: panel2.map(({ name, path, is_dir }) => ({ name, path, is_dir })), sourceDir: '/panel2', isRemote: false, operation: 'copy' } },
      splitForFileBatch, fileBatchSessionFlags: () => ({ aeroVaultOverlay: false, gitHostUpload: false }),
      resetOverwriteSettings: vi.fn(), runFileBatch, batchCancelledRef: { current: false },
      remoteFiles: [], localFiles: [file('a.bin', 1)], sortedLocalFiles: [], localFiles2: panel2, sortedLocalFiles2: panel2,
      loadRemoteFiles: vi.fn(), notify: { error: vi.fn(), warning: vi.fn(), info: vi.fn() }, t: (key: string) => key,
    };
    const { clipboardPaste } = new Function(...Object.keys(context), `${pasteCode}; return { clipboardPaste };`)(...Object.values(context));
    await clipboardPaste(true, '/remote');
    expect(runFileBatch).toHaveBeenCalledTimes(1);
    expect(runFileBatch.mock.calls[0][1]).toEqual(panel2.map(r => ({ name: r.name, sourcePath: r.path, size: r.size, modified: r.modified })));
  });
});

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

it('carries ImageKit skip policy to native single and batch uploads', async () => {
  const f = fixture({ direction: 'upload', protocol: 'imagekit', policy: 'skip', destination: [] });
  await single(f, 'upload');
  expect(f.writers()[0]).toEqual(['provider_upload_file', expect.objectContaining({ noClobber: true })]);
  const batch = fixture({ direction: 'upload', protocol: 'imagekit', policy: 'skip', destination: [] });
  await batch.runtime.runFileBatch('upload', items, '/actual');
  expect(batch.writers()[0]).toEqual(['provider_upload_files_batch', expect.objectContaining({ params: expect.objectContaining({ no_clobber: true }) })]);
});

describe('App remote download leaf safety', () => {
  const literalUnixName = String.raw`back\slash.txt`;

  it.each([false, true])('preserves a literal POSIX backslash in single/folder download (isDir=%s)', async isDir => {
    const f = fixture({ direction: 'download', destination: [] });
    expect(await f.runtime.downloadFile(`/source/${literalUnixName}`, literalUnixName, '/actual', isDir, 100)).toBe(true);
    const [command, args] = f.writers()[0];
    expect(command).toBe(isDir ? 'provider_download_folder' : 'provider_download_file');
    expect(args!.localPath).toBe(`/actual/${literalUnixName}`);
  });

  it.each([false, true])('rejects traversal in single/folder download before prompt or writer (isDir=%s)', async isDir => {
    const f = fixture({ direction: 'download', destination: [] });
    expect(await f.runtime.downloadFile('/source/../outside', '../outside', '/actual', isDir, 100)).toBe(false);
    expect(f.writers()).toHaveLength(0);
    expect(f.checkOverwrite).not.toHaveBeenCalled();
  });

  it('preserves a literal POSIX backslash in the real batch planner', async () => {
    const f = fixture({ direction: 'download', destination: [] });
    const result = await f.runtime.runFileBatch('download', [{ name: literalUnixName, sourcePath: `/source/${literalUnixName}`, size: 100, modified: null }], '/actual');
    expect(result).toMatchObject({ completed: 1, failed: 0, aborted: false });
    expect(f.writers()[0][1]!.params.entries[0].local_path).toBe(`/actual/${literalUnixName}`);
  });

  it('rejects batch traversal before listing, prompt or writer', async () => {
    const f = fixture({ direction: 'download', destination: [] });
    const result = await f.runtime.runFileBatch('download', [{ name: '../outside', sourcePath: '/source/../outside', size: 100, modified: null }], '/actual');
    expect(result).toMatchObject({ failed: 1, aborted: true, sent: 0 });
    expect(f.listings()).toHaveLength(0);
    expect(f.checkOverwrite).not.toHaveBeenCalled();
    expect(f.writers()).toHaveLength(0);
  });

  it('rejects backslash as a separator on Windows single and batch targets', async () => {
    const target = String.raw`C:\actual`;
    const singleFixture = fixture({ direction: 'download', destination: [] });
    expect(await singleFixture.runtime.downloadFile(`/source/${literalUnixName}`, literalUnixName, target, false, 100)).toBe(false);
    expect(singleFixture.writers()).toHaveLength(0);

    const batchFixture = fixture({ direction: 'download', destination: [] });
    const result = await batchFixture.runtime.runFileBatch('download', [{ name: literalUnixName, sourcePath: `/source/${literalUnixName}`, size: 100, modified: null }], target);
    expect(result).toMatchObject({ failed: 1, aborted: true, sent: 0 });
    expect(batchFixture.writers()).toHaveLength(0);
  });
});
