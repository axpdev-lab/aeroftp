// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// The App transfer closures, run with a mocked IPC boundary. Shared by the
// wiring tests that drive them, so each exercises the same App source.

import { transformWithOxc } from 'vite';
import { vi } from 'vitest';
import app from '../../App.tsx?raw';
import overwriteHook from '../../hooks/useOverwriteCheck.ts?raw';
import { usesProviderApi, type RemoteFile } from '../../types';
import { loadOverwriteDestination } from '../overwriteDestination';
import { fileBatchCommand, folderTransferIsComplete, keepsSingleFilePath } from '../fileBatchRouting';
import { createFileBatchDispatcher, rearmedOnUserAction } from '../../components/transferQueueActions';
import { isWindowsLocalPath, isSafeRemoteDownloadName, remoteDownloadTarget } from '../localPath';


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
  closure('captureGuiTarget', '\n  const getOverwriteDestination =')
  + (app.includes('const getOverwriteDestination =') ? closure('getOverwriteDestination', '\n  // Helper: check folder overwrite') : '')
  + closure('downloadFile', '\n  const uploadFile =')
  + closure('uploadFile', '\n  // The retry of an item restored from the journal')
  + closure('runFileBatch', '\n  const uploadMultipleFiles ='),
  'overwrite-wiring.ts',
);
// The paste: what it hands the batch of a copy to the other side.
export const { code: pasteCode } = await transformWithOxc(
  closure('clipboardPaste', '\n  // openDevToolsPreview, openUniversalPreview'),
  'paste-wiring.ts',
);
const { code: overwriteCode } = await transformWithOxc(
  overwriteHook.slice(overwriteHook.indexOf('  const checkOverwrite ='), overwriteHook.indexOf('  const resetOverwriteSettings =')),
  'overwrite-check.ts',
);

export type Direction = 'upload' | 'download';
export type Policy = 'skip' | 'overwrite' | 'resume' | 'rename';
export const file = (name: string, size = 40): RemoteFile => ({ name, path: `/actual/${name}`, size, modified: null, is_dir: false, permissions: null });
export function fixture(options: { hold?: { holdFileCompletions: (ids: readonly string[]) => () => void; registerFileBatchRows: (batchId: string, queueIds: readonly string[]) => void }; queue?: Record<string, unknown>; privacyError?: string; staged?: boolean; epoch?: () => number | Promise<number>; direction: Direction; policy?: Policy; destination?: RemoteFile[]; panel?: RemoteFile[]; nested?: RemoteFile[]; panel2?: RemoteFile[]; failListing?: boolean; beforeListing?: () => Promise<void>; ownsProfile?: () => boolean; protocol?: 'webdav' | 'sftp' | 'imagekit' | 'opendrive' | 'fourshared' }) {
  const direction = options.direction;
  const protocol = options.protocol || 'webdav';
  const destination = options.destination ?? [file('a.bin')];
  const panel = options.panel ?? [];
  const invoke = vi.fn(async (command: string, args?: Record<string, any>) => {
    if (options.privacyError && (command === 'opendrive_set_path_privacy' || command === 'fourshared_set_path_privacy')) throw options.privacyError;
    if (command === 'provider_gui_target_epoch') return options.epoch?.() ?? 7;
    if (['get_local_files', 'provider_list_files'].includes(command)) {
      await options.beforeListing?.();
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
  const checkOverwrite = vi.fn(new Function('isWindowsLocalPath', 'useCallback', 'localFiles', 'remoteFiles', 'fileExistsAction', 'overwriteApplyToAllRef', 'setOverwriteDialog',
    `${overwriteCode}; return checkOverwrite;`)(isWindowsLocalPath, (fn: unknown) => fn, panel, panel, options.policy || 'skip',
      { current: { enabled: false, action: 'overwrite' } }, () => { throw new Error('Unexpected interactive overwrite'); }));
  const resolveDestination = vi.fn(loadOverwriteDestination);
  const queueItemsRef = { current: [] as { id: string; status: string }[] };
  const retryCallbacksRef = { current: new Map<string, () => unknown>() };
  const transferQueue = {
    addItem: vi.fn(() => {
      const id = `q${queueItemsRef.current.length}`;
      queueItemsRef.current.push({ id, status: 'pending' });
      return id;
    }),
    startTransfer: vi.fn(), completeTransfer: vi.fn(), failTransfer: vi.fn(), cancelTransfer: vi.fn(),
    // A test that follows real rows replaces the settle functions.
    ...options.queue,
  };
  const notify = { info: vi.fn(), error: vi.fn() };
  const resetOverwriteSettings = vi.fn();
  const context = {
    invoke, checkOverwrite, loadOverwriteDestination: resolveDestination, resetOverwriteSettings,
    isWindowsLocalPath, isSafeRemoteDownloadName, remoteDownloadTarget,
    isLiveOnProfile: options.ownsProfile ?? (() => true),
    sessions: [{ id: 'A', connectionParams: { protocol } }], activeSessionId: 'A', connectionParams: { protocol },
    liveConnectionParams: { protocol },
    usesProviderApi, aeroVaultOverlaySession: null, currentRemotePath: '/panel',
    localFiles: direction === 'upload' ? [file('a.bin', 100)] : panel,
    // The rows shown, which a nested row of a flattened search is found in.
    sortedLocalFiles: options.nested ?? [],
    localFiles2: options.panel2 ?? [], sortedLocalFiles2: options.panel2 ?? [],
    remoteFiles: direction === 'download' ? [file('a.bin', 100)] : panel,
    humanLog: { logStart: () => 'log', logRaw: vi.fn(), log: vi.fn(), logError: vi.fn(), updateEntry: vi.fn() },
    stopCountRef: { current: 0 }, pendingFileLogIds: { current: new Map() }, batchCancelledRef: { current: false }, cancelLevelRef: { current: 0 },
    circuitBreaker: { reset: vi.fn() }, retryCallbacksRef, withTransferGeneration: <T,>(fn: T) => fn,
    registerTransferCallback: (id: string, cb: () => unknown) => { retryCallbacksRef.current.set(id, cb); }, queueItemsRef, transferQueue,
    recordQueueDescriptor: vi.fn(),
    // The event hook's own functions when a test mounts it; inert otherwise.
    holdFileCompletions: options.hold?.holdFileCompletions ?? (() => () => undefined), registerFileBatchRows: options.hold?.registerFileBatchRows ?? vi.fn(), fileBatchCommand, keepsSingleFilePath,
    folderTransferIsComplete, createFileBatchDispatcher, rearmedOnUserAction,
    // One server for the whole run (the server check is pinned in wrongTarget.wiring.test.ts).
    boundToQueuedServer: (_target: unknown, _id: string, run: () => unknown) => async () => { await run(); },
    snapshotQueuedTarget: () => ({ params: {}, profileId: null, overlay: null }),
    queuedTargetProblem: () => null,
    queuedTargetGuard: () => () => undefined,
    failQueuedRestIfRetargeted: () => false,
    QueuedTargetChangedError: Error,
    joinRemotePath: (dir: string, name: string) => `${dir}/${name}`,
    settings: { autoStartTransfers: !options.staged }, fileExistsAction: options.policy || 'skip',
    effectiveMaxConcurrentTransfers: 4, retryCount: 1, timeoutSeconds: 30,
    downloadSegments: 0, sftpDownloadPreset: 'balanced', supportsFtpTransferPresets: false,
    buildSftpDownloadPresetPayload: () => ({}), resolveFtpDownloadSegments: () => 0,
    formatBytes: (size: number) => String(size), t: (key: string) => key, notify,
    // Outside React, a function from useLatestCallback is the function itself.
    useLatestCallback: <F,>(fn: F) => fn,
  };
  const runtime = new Function(...Object.keys(context), `${code}; return { downloadFile, uploadFile, runFileBatch };`)(...Object.values(context));
  const writers = () => invoke.mock.calls.filter(([name]) => /provider_(upload|download)_(file|files_batch|folder)$/.test(name));
  const listings = () => invoke.mock.calls.filter(([name]) => ['get_local_files', 'provider_list_files'].includes(name));
  return { runtime, invoke, checkOverwrite, resolveDestination, notify, transferQueue, listings, writers, resetOverwriteSettings, retryCallbacks: context.retryCallbacksRef.current, stop: () => { context.stopCountRef.current++; context.batchCancelledRef.current = true; } };
}
