// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import type { RemoteFile } from '../types';
import { splitForFileBatch } from './fileBatchRouting';
import { file, fixture, pasteCode, type Direction } from './testFixtures/appTransferWiring';

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
      resetOverwriteSettings: vi.fn(), runFileBatch, batchCancelledRef: { current: false }, stopCountRef: { current: 0 },
      remoteFiles: [], localFiles: [file('a.bin', 1)], sortedLocalFiles: [], localFiles2: panel2, sortedLocalFiles2: panel2,
      loadRemoteFiles: vi.fn(), notify: { error: vi.fn(), warning: vi.fn(), info: vi.fn() }, t: (key: string) => key,
      // One server for the whole paste (the clipboard's server check is in wrongTarget.wiring.test.ts).
      snapshotQueuedTarget: () => ({ params: {}, profileId: null, overlay: null }),
      queuedTargetProblem: () => null, queuedTargetGuard: () => () => undefined,
      captureGuiTarget: async () => 7,
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


it('serializes differently cased Windows download claims in selection order', async () => {
  const f = fixture({ direction: 'download', destination: [], policy: 'overwrite' });
  await f.runtime.runFileBatch('download', [
    { ...items[0], name: 'foo.txt', sourcePath: '/one/foo.txt' },
    { ...items[0], name: 'FOO.txt', sourcePath: '/two/FOO.txt' },
  ], 'C:/Downloads');
  expect(f.writers().map(([name]) => name)).toEqual(['provider_download_files_batch', 'provider_download_file']);
  expect(f.writers()[0][1]!.params.entries).toHaveLength(1);
  expect(f.writers()[1][1]!.remotePath).toBe('/two/FOO.txt');
});

it.each(['download', 'upload'] as const)('refuses a restored %s when the profile changes during destination listing', async direction => {
  let release!: () => void;
  let entered!: () => void;
  let owns = true;
  const listing = new Promise<void>(resolve => { release = resolve; });
  const reached = new Promise<void>(resolve => { entered = resolve; });
  const f = fixture({ direction, destination: [], policy: 'overwrite', ownsProfile: () => owns,
    beforeListing: async () => { entered(); await listing; } });
  const owner = { profileId: 'profile-A', generation: 7 };
  const pending = direction === 'download'
    ? f.runtime.downloadFile('/remote/a.bin', 'a.bin', '/actual', false, 100, false, 'throw', owner)
    : f.runtime.uploadFile('/local/a.bin', 'a.bin', false, 100, false, undefined, '/actual', 'throw', owner);
  const refused = expect(pending).rejects.toThrow('CONNECTION_CHANGED');
  await reached;
  owns = false;
  release();
  await refused;
  expect(f.writers()).toHaveLength(0);
});

it('passes the captured generation to the backend after preflight', async () => {
  const f = fixture({ direction: 'download', destination: [], ownsProfile: () => true });
  await f.runtime.downloadFile('/remote/a.bin', 'a.bin', '/actual', false, 100, false, 'throw', { profileId: 'profile-A', generation: 7 });
  expect(f.writers()[0][1]).toMatchObject({ expectedGeneration: 7, expectedProfileId: 'profile-A' });
});


it('requires the backend overlay as well as the frontend profile before an encrypted retry', async () => {
  const f = fixture({ direction: 'download', destination: [], ownsProfile: () => true });
  await f.runtime.downloadFile('/remote/a.bin', 'a.bin', '/actual', false, 100, false, 'throw', { profileId: 'profile-A', generation: 7, requireOverlay: true });
  expect(f.writers()[0][1]).toMatchObject({ expectedGeneration: 7, expectedProfileId: 'profile-A', requireCryptOverlay: true });
});

it.each(['opendrive', 'fourshared'] as const)('keeps cut originals after %s privacy rejects a changed target', async protocol => {
  const f = fixture({ direction: 'upload', protocol, destination: [], policy: 'overwrite', privacyError: 'CONNECTION_CHANGED: target changed' });
  const result = await f.runtime.runFileBatch('upload', items, '/actual');
  expect(f.invoke).toHaveBeenCalledWith(protocol === 'opendrive' ? 'opendrive_set_path_privacy' : 'fourshared_set_path_privacy', expect.objectContaining({ isPublic: false, expectedGuiTarget: expect.any(Number) }));
  expect(result.succeededSources).toEqual([]);
  expect(result.aborted).toBe(true);
  expect(result.failed).toBe(2);
  expect(f.transferQueue.completeTransfer).not.toHaveBeenCalled();
  // App itself settles both rows as failed: neither file was made private.
  const rows = f.transferQueue.addItem.mock.results.map(added => added.value);
  expect(rows).toHaveLength(2);
  expect(f.transferQueue.failTransfer.mock.calls).toEqual(rows.map(id => [id, 'activity.set_private_error']));
});

it.each(['opendrive', 'fourshared'] as const)('fails only the row whose %s privacy write was rejected', async protocol => {
  const f = fixture({ direction: 'upload', protocol, destination: [], policy: 'overwrite' });
  const backend = f.invoke.getMockImplementation()!;
  f.invoke.mockImplementation(async (command, args) => {
    if (!command.endsWith('_set_path_privacy')) return backend(command, args);
    if (String(args?.path).endsWith('/b.bin')) throw new Error('privacy refused');
    return undefined;
  });
  const result = await f.runtime.runFileBatch('upload', items, '/actual');
  const [rowA, rowB] = f.transferQueue.addItem.mock.results.map(added => added.value);
  expect(f.invoke.mock.calls.filter(([command]) => command.endsWith('_set_path_privacy')).map(([, args]) => args!.path)).toEqual(['/actual/a.bin', '/actual/b.bin']);
  expect(f.transferQueue.failTransfer.mock.calls).toEqual([[rowB, 'activity.set_private_error']]);
  expect(f.transferQueue.completeTransfer.mock.calls).toEqual([[rowA]]);
  expect(result.failed).toBe(1);
  expect(result.succeededSources).toEqual(['/source/a.bin']);
});

it.each(['upload', 'download'] as const)('pins a fresh epoch for staged %s Start and subsequent Retry', async direction => {
  let epoch = 7;
  const f = fixture({ direction, destination: [], policy: 'overwrite', staged: true, epoch: () => epoch });
  expect(await f.runtime.runFileBatch(direction, items, '/actual')).toMatchObject({ staged: true });
  expect(f.writers()).toHaveLength(0);
  epoch = 9;
  await f.retryCallbacks.get('q0')!();
  await vi.waitFor(() => expect(f.writers()).toHaveLength(1));
  expect(f.writers()[0][1]?.params.expected_gui_target).toBe(9);
  await new Promise(resolve => setTimeout(resolve, 0));
  epoch = 11;
  await f.retryCallbacks.get('q1')!();
  await vi.waitFor(() => expect(f.writers()).toHaveLength(2));
  expect(f.writers()[1][1]?.params.expected_gui_target).toBe(11);
});

// Execute the production App launchers with an epoch response arriving after Stop.
it.each(['upload', 'download'] as const)('does not start staged %s or Retry after Stop during epoch capture', async direction => {
  let resolveEpoch!: (value: number) => void;
  let epoch: number | Promise<number> = 7;
  const f = fixture({ direction, destination: [], policy: 'overwrite', staged: true, epoch: () => epoch });
  await f.runtime.runFileBatch(direction, items, '/actual');
  epoch = new Promise<number>(resolve => { resolveEpoch = resolve; });
  await f.retryCallbacks.get('q0')!();
  f.stop();
  resolveEpoch(9);
  await vi.waitFor(() => expect(f.transferQueue.cancelTransfer).toHaveBeenCalledTimes(2));
  expect(f.writers()).toHaveLength(0);
  epoch = 11;
  await f.retryCallbacks.get('q0')!();
  await vi.waitFor(() => expect(f.writers()).toHaveLength(1));
  // A subsequent retry must also abandon its captured action when Stop wins.
  epoch = new Promise<number>(resolve => { resolveEpoch = resolve; });
  await new Promise(resolve => setTimeout(resolve, 0));
  await f.retryCallbacks.get('q1')!();
  f.stop();
  resolveEpoch(13);
  await vi.waitFor(() => expect(f.transferQueue.cancelTransfer).toHaveBeenCalledTimes(3));
  expect(f.writers()).toHaveLength(1);
});

it.each(['upload', 'download'] as const)('does not clear Stop during initial %s epoch capture', async direction => {
  let finish!: (epoch: number) => void;
  const epoch = new Promise<number>(resolve => { finish = resolve; });
  const f = fixture({ direction, destination: [], policy: 'overwrite', epoch: () => epoch });
  const pending = f.runtime.runFileBatch(direction, items, '/actual');
  f.stop();
  finish(7);
  expect(await pending).toMatchObject({ aborted: true, sent: 0 });
  expect(f.writers()).toHaveLength(0);
  expect(f.invoke.mock.calls.some(([cmd]) => cmd === 'reset_cancel_flag')).toBe(false);
});
