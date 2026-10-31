// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import overwriteHook from '../hooks/useOverwriteCheck.ts?raw';
import { vi } from 'vitest';
import { usesProviderApi, type LocalFile, type RemoteFile } from '../types';
import { loadOverwriteDestination } from '../utils/overwriteDestination';
import { fileBatchCommand, folderTransferIsComplete, keepsSingleFilePath, splitForFileBatch } from '../utils/fileBatchRouting';
import { asLocalFileEntry, localRowNameClashes, selectedLocalTrees } from '../utils/localSelection';
import { isSafeRemoteDownloadName, remoteDownloadTarget } from '../utils/localPath';
import { RECONNECT_ERROR_KINDS, getErrorKindI18nKey } from '../utils/transferErrorClassifier';
import { withTransferRun } from '../utils/transferRun';
import { createFileBatchDispatcher, rearmedOnUserAction, updateTransferStatus } from './transferQueueActions';
import type { TransferItem } from './TransferQueue';
import { INITIAL_SCANNING_STATE } from './ScanningToast';

/**
 * App's transfer closures, run with the IPC mocked: uploadFile, downloadFile,
 * the two multi-file transfers and the cross-panel drop, from App's own
 * source. The tests that use it check the paths the backend is asked to write
 * and what becomes of each queue row.
 *
 * Not a test file: the tests import it (`*.test.ts` are the tests).
 */

/** App's `const name = ...` up to `endMarker`. */
function closure(name: string, endMarker: string) {
  const start = app.indexOf(`\n  const ${name} =`);
  const end = app.indexOf(endMarker, start + 1);
  if (start < 0 || end < 0) throw new Error(`Missing App closure: ${name}`);
  return app.slice(start, end);
}
const dropStart = app.indexOf('\n    crossPanelDropRef.current = async (');
const dropEnd = app.indexOf('\n  });', dropStart);
if (dropStart < 0 || dropEnd < 0) throw new Error('Missing App closure: crossPanelDropRef');

const { code } = await transformWithOxc(
  closure('getOverwriteDestination', '\n  // Helper: check folder overwrite')
  + closure('downloadFile', '\n  const uploadFile =')
  + closure('uploadFile', '\n  // Keep restore-retry')
  + closure('withBatchLatch', '\n  const uploadMultipleFiles =')
  + closure('uploadMultipleFiles', '\n  // === Bulk Operations ===')
  + closure('downloadMultipleFiles', '\n  // Wire cross-panel drag & drop callback')
  + app.slice(dropStart, dropEnd),
  'drop-on-folder-row.ts',
);
const { code: overwriteCode } = await transformWithOxc(
  overwriteHook.slice(overwriteHook.indexOf('  const checkOverwrite ='), overwriteHook.indexOf('  const resetOverwriteSettings =')),
  'overwrite-check.ts',
);

export const REMOTE = '/remote';
export const INBOX = '/remote/Inbox';
export const LOCAL = '/home/me';
export const SUB = '/home/me/Sub';
export const PANEL2 = '/home/me/other';

export const remote = (name: string, dir = REMOTE, size = 40): RemoteFile =>
  ({ name, path: `${dir}/${name}`, size, modified: null, is_dir: false, permissions: null });
export const local = (name: string, dir = LOCAL, size = 40): LocalFile =>
  ({ name, path: `${dir}/${name}`, size, modified: null, is_dir: false });
export const folder = <T extends LocalFile | RemoteFile>(entry: T): T => ({ ...entry, size: 0, is_dir: true });

/** The part of App's circuit breaker the transfers call. */
export type Breaker = {
  reset: () => void;
  recordSuccess: () => void;
  recordFailure: (error: string) => { isFatal: boolean; retryable: boolean; shouldPause: boolean };
  stateRef: { current: string };
  pauseReasonRef: { current: unknown };
  tripErrorKindRef: { current: unknown };
  config: { maxRetriesPerFile: number };
  getRetryDelay: (attempt: number) => number;
};

export type Options = {
  /** What each folder holds, as the backend would list it. */
  folders?: Record<string, (LocalFile | RemoteFile)[]>;
  /** Folders whose listing fails. */
  unlistable?: string[];
  /** Folders whose listing fails from that call on (1 = the first). */
  listingFailsFrom?: Record<string, number>;
  policy?: 'skip' | 'overwrite' | 'rename';
  /** What the folder question answers. */
  folderAction?: 'merge_overwrite' | 'skip';
  protocol?: 'webdav' | 'github';
  /** The error a single-file write answers with, by command and call number
   *  (1 for the first); none means it lands. */
  fail?: (command: string, call: number) => string | undefined;
  /** App's circuit breaker; by default one that never retries. */
  breaker?: Breaker;
};

export function fixture(options: Options = {}) {
  const folders = options.folders ?? {};
  const listings = new Map<string, number>();
  const listingOf = (path: string) => {
    const call = (listings.get(path) ?? 0) + 1;
    listings.set(path, call);
    if (options.unlistable?.includes(path)) throw new Error(`Listing of ${path} refused`);
    const failsFrom = options.listingFailsFrom?.[path];
    if (failsFrom !== undefined && call >= failsFrom) throw new Error(`Listing of ${path} refused`);
    return folders[path] ?? [];
  };
  const calls: Record<string, number> = {};
  const invoke = vi.fn(async (command: string, args?: Record<string, any>) => {
    if (command === 'get_local_files') return listingOf(args!.path);
    if (command === 'provider_list_files') return { files: listingOf(args!.path), current_path: args!.path };
    if (command === 'provider_download_files_batch' || command === 'provider_upload_files_batch') {
      const entries = args!.params.entries;
      return { completed: entries.length, failed: 0, cancelled: false, succeeded: entries.map((_: unknown, i: number) => i) };
    }
    if (command === 'provider_upload_file' || command === 'provider_download_file') {
      calls[command] = (calls[command] ?? 0) + 1;
      const error = options.fail?.(command, calls[command]);
      // The backend answers a failed command with its message, a string.
      if (error) throw error;
      return undefined;
    }
    if (command === 'reset_cancel_flag') return undefined;
    if (command === 'provider_upload_folder' || command === 'provider_download_folder') return { cancelled: false, failed: 0, skipped: 0 };
    if (command === 'github_check_local_sync') return { is_local_repo: false };
    if (command === 'github_batch_upload') return { commit_sha: 'abcdef1234', files_count: args!.files.length };
    throw new Error(`Unexpected IPC: ${command}`);
  });

  // Panel 1 shows LOCAL, the remote panel REMOTE; what they list is what the
  // folders hold there.
  const localFiles = (folders[LOCAL] ?? []) as LocalFile[];
  const localFiles2 = (folders[PANEL2] ?? []) as LocalFile[];
  const remoteFiles = (folders[REMOTE] ?? []) as RemoteFile[];
  const panels = {
    local: { id: 'local', files: localFiles, sortedFiles: localFiles, currentPath: LOCAL, selection: new Set<string>(), setSelection: vi.fn() },
    local2: { id: 'local2', files: localFiles2, sortedFiles: localFiles2, currentPath: PANEL2, selection: new Set<string>(), setSelection: vi.fn() },
  };
  const checkOverwrite = vi.fn(new Function('useCallback', 'localFiles', 'remoteFiles', 'fileExistsAction', 'overwriteApplyToAllRef', 'setOverwriteDialog',
    `${overwriteCode}; return checkOverwrite;`)((fn: unknown) => fn, localFiles, remoteFiles, options.policy ?? 'skip',
      { current: { enabled: false, action: 'overwrite' } }, () => { throw new Error('Unexpected interactive overwrite'); }));
  // The queue keeps its rows as the app does: a settled row changes only
  // through retryItem (updateTransferStatus).
  const queueItemsRef = { current: [] as TransferItem[] };
  const settle = (id: string, status: TransferItem['status'], error?: string) => {
    queueItemsRef.current = updateTransferStatus(queueItemsRef.current, id, status, undefined, error);
  };
  const transferQueue = {
    addItem: vi.fn(() => {
      const id = `q${queueItemsRef.current.length}`;
      queueItemsRef.current.push({ id, status: 'pending' } as TransferItem);
      return id;
    }),
    startTransfer: vi.fn((id: string) => settle(id, 'transferring')),
    completeTransfer: vi.fn((id: string) => settle(id, 'completed')),
    failTransfer: vi.fn((id: string, error: string) => settle(id, 'error', error)),
    retryItem: vi.fn((id: string) => {
      queueItemsRef.current = queueItemsRef.current.map(item => (item.id === id ? { ...item, status: 'pending', error: undefined } : item));
    }),
  };
  const row = (id: string) => queueItemsRef.current.find(item => item.id === id);
  const batchCancelledRef = { current: false };
  const retryCallbacksRef = { current: new Map<string, () => Promise<void>>() };
  const notify = { info: vi.fn(), error: vi.fn(), success: vi.fn() };
  const checkFolderOverwrite = vi.fn(async (..._args: unknown[]) => ({ action: options.folderAction ?? 'merge_overwrite', applyToAll: false }));
  const refreshLocalPanelForPath = vi.fn(async () => undefined);
  const loadRemoteFiles = vi.fn(async () => null);
  // Panel 1's own loader, which the download called before it reloaded every
  // panel showing the folder written.
  const loadLocalFiles = vi.fn(async () => undefined);
  const crossPanelDropRef = { current: null as null | ((files: { name: string; path: string }[], fromRemote: boolean, targetDir: string | undefined, sourceKey?: string) => Promise<void>) };
  const protocol = options.protocol ?? 'webdav';
  const requestGitHubBatchCommitMessage = vi.fn(async (..._args: unknown[]) => 'Add files');
  const context = {
    invoke, checkOverwrite, loadOverwriteDestination, usesProviderApi,
    resetOverwriteSettings: vi.fn(), checkFolderOverwrite,
    isSafeRemoteDownloadName, remoteDownloadTarget, splitForFileBatch, selectedLocalTrees, asLocalFileEntry,
    refuseLocalNameClashes: (rows: Pick<LocalFile, 'name' | 'path'>[]) => localRowNameClashes(rows).length > 0,
    getActiveLocalState: (id: 'local' | 'local2' = 'local') => panels[id],
    focusedLocalPanelId: 'local',
    sessions: [{ id: 'A', connectionParams: { protocol } }], activeSessionId: 'A', connectionParams: { protocol },
    liveConnectionParams: { protocol }, aeroVaultOverlaySession: null, isConnected: true,
    currentRemotePath: REMOTE, currentLocalPath: LOCAL, localFiles, localFiles2, sortedLocalFiles: localFiles, sortedLocalFiles2: localFiles2, remoteFiles,
    selectedRemoteFiles: new Set<string>(), setSelectedRemoteFiles: vi.fn(),
    humanLog: { logStart: () => 'log', logRaw: vi.fn(), log: vi.fn(), logError: vi.fn(), updateEntry: vi.fn() },
    pendingFileLogIds: { current: new Map() }, batchCancelledRef, cancelLevelRef: { current: 0 },
    batchOpInFlightRef: { current: false }, folderOverwriteApplyToAll: { current: { action: 'merge_overwrite', enabled: false } },
    circuitBreaker: options.breaker ?? {
      reset: vi.fn(), recordSuccess: vi.fn(), recordFailure: () => ({ isFatal: false, retryable: false, shouldPause: false }),
      stateRef: { current: 'closed' }, pauseReasonRef: { current: null }, tripErrorKindRef: { current: null },
      config: { maxRetriesPerFile: 0 }, getRetryDelay: () => 0,
    },
    RECONNECT_ERROR_KINDS, getErrorKindI18nKey, attemptBatchReconnect: vi.fn(), waitForBatchResume: vi.fn(),
    retryCallbacksRef, queueItemsRef, transferQueue, recordQueueDescriptor: vi.fn(), registerFileBatchRows: vi.fn(), withTransferRun,
    fileBatchCommand, keepsSingleFilePath, folderTransferIsComplete, createFileBatchDispatcher, rearmedOnUserAction,
    joinRemotePath: (dir: string, name: string) => `${dir}${dir.endsWith('/') ? '' : '/'}${name}`,
    settings: { autoStartTransfers: true }, fileExistsAction: options.policy ?? 'skip',
    effectiveMaxConcurrentTransfers: 4, retryCount: 1, timeoutSeconds: 30,
    downloadSegments: 0, sftpDownloadPreset: 'balanced', supportsFtpTransferPresets: false,
    buildSftpDownloadPresetPayload: () => ({}), resolveFtpDownloadSegments: () => 0,
    formatBytes: (size: number) => String(size), t: (key: string) => key, notify,
    refreshLocalPanelForPath, loadRemoteFiles, loadLocalFiles, crossPanelDropRef, setScanningState: vi.fn(), INITIAL_SCANNING_STATE,
    requestGitHubBatchCommitMessage, setGitHubSyncWarning: vi.fn(),
  };
  new Function(...Object.keys(context), code)(...Object.values(context));

  /** Where each write lands: the backend path of every single and batch write. */
  const written = () => invoke.mock.calls.flatMap(([command, args]) => {
    if (command === 'provider_upload_files_batch') return args!.params.entries.map((e: any) => e.remote_path);
    if (command === 'github_batch_upload') return args!.files.map((f: any) => f.remotePath);
    if (command === 'provider_download_files_batch') return args!.params.entries.map((e: any) => e.local_path);
    if (command === 'provider_upload_file' || command === 'provider_upload_folder') return [args!.remotePath];
    if (command === 'provider_download_file' || command === 'provider_download_folder') return [args!.localPath];
    return [];
  });
  const drop = (...args: Parameters<NonNullable<typeof crossPanelDropRef.current>>) => crossPanelDropRef.current!(...args);
  const commits = () => invoke.mock.calls.filter(([command]) => command === 'github_batch_upload').map(([, args]) => args!.files.map((f: any) => f.remotePath));
  return { drop, invoke, written, commits, panels, setSelectedRemoteFiles: context.setSelectedRemoteFiles, checkOverwrite, checkFolderOverwrite, requestGitHubBatchCommitMessage, notify, transferQueue, row, batchCancelledRef, retryCallbacksRef, refreshLocalPanelForPath, loadRemoteFiles };
}
