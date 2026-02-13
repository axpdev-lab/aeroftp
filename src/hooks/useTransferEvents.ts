import { useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { TransferEvent, TransferProgress } from '../types';

/* eslint-disable @typescript-eslint/no-explicit-any */
interface UseTransferEventsOptions {
  t: (key: string, params?: Record<string, string>) => string;
  activityLog: any;
  humanLog: any;
  transferQueue: any;
  notify: any;
  setActiveTransfer: (transfer: TransferProgress | null) => void;
  loadRemoteFiles: (overrideProtocol?: string) => unknown;
  loadLocalFiles: (path: string) => void;
  currentLocalPath: string;
  currentRemotePath: string;
}
/* eslint-enable @typescript-eslint/no-explicit-any */

export function useTransferEvents(options: UseTransferEventsOptions) {
  // Store ALL options in a ref to avoid stale closures AND prevent re-subscribing.
  // The event listener subscribes once ([] deps) and always reads fresh values via ref.
  // This eliminates the micro-gap where events could be lost during re-subscription.
  const optRef = useRef(options);
  optRef.current = options;

  // Correlation maps between backend transfer IDs and frontend UI elements
  const transferIdToQueueId = useRef<Map<string, string>>(new Map());
  const transferIdToLogId = useRef<Map<string, string>>(new Map());
  const pendingFileLogIds = useRef<Map<string, string>>(new Map());
  const pendingDeleteLogIds = useRef<Map<string, string>>(new Map());
  const transferIdToDisplayPath = useRef<Map<string, string>>(new Map());
  const detailedDeleteCompletedIds = useRef<Set<string>>(new Set());
  // Track completed transfer IDs to prevent late progress events from re-showing the toast
  const completedTransferIds = useRef<Set<string>>(new Set());
  // Track last known file-level speed for display in folder transfer toast
  const lastFileSpeedRef = useRef<number>(0);

  useEffect(() => {
    const joinPath = (base: string, name: string): string => {
      if (!base) return name;
      return `${base.replace(/[\\/]$/, '')}/${name}`;
    };

    const resolveTransferDisplayPath = (data: TransferEvent, currentLocalPath: string, currentRemotePath: string): string => {
      if (data.path && data.path.trim().length > 0) return data.path;
      if (!data.filename) return '';
      if (data.direction === 'upload') return joinPath(currentRemotePath, data.filename);
      if (data.direction === 'download') return joinPath(currentLocalPath, data.filename);
      return data.filename;
    };

    const resolveDisplayPath = (data: TransferEvent, currentLocalPath: string, currentRemotePath: string): string => {
      if (data.path && data.path.trim().length > 0) return data.path;
      const base = data.direction === 'remote' ? currentRemotePath : currentLocalPath;
      if (!base || !data.filename) return data.filename;
      return `${base.replace(/\/$/, '')}/${data.filename}`;
    };

    const unlisten = listen<TransferEvent>('transfer_event', (event) => {
      const { t, activityLog, humanLog, transferQueue, notify, setActiveTransfer } = optRef.current;
      const data = event.payload;

      // ========== TRANSFER EVENTS (download/upload) ==========
      if (data.event_type === 'start') {
        // Clean up completed set and reset speed tracking for this new transfer
        completedTransferIds.current.delete(data.transfer_id);
        lastFileSpeedRef.current = 0;
        const displayName = resolveTransferDisplayPath(data, optRef.current.currentLocalPath, optRef.current.currentRemotePath);
        transferIdToDisplayPath.current.set(data.transfer_id, displayName);
        let logId = '';
        // Check if we have a pending manual log for this file (deduplication)
        if (pendingFileLogIds.current.has(data.filename)) {
          logId = pendingFileLogIds.current.get(data.filename)!;
          pendingFileLogIds.current.delete(data.filename);
        } else {
          logId = humanLog.logStart(data.direction === 'download' ? 'DOWNLOAD' : 'UPLOAD', { filename: displayName });
        }
        transferIdToLogId.current.set(data.transfer_id, logId);

        const queueItem = transferQueue.items.find((i: { filename: string; status: string; id: string }) =>
          i.filename === data.filename && (i.status === 'pending' || i.status === 'transferring'));
        if (queueItem) {
          transferIdToQueueId.current.set(data.transfer_id, queueItem.id);
          transferQueue.markAsFolder(queueItem.id);
          if (queueItem.status === 'pending') transferQueue.startTransfer(queueItem.id);
        }
      } else if (data.event_type === 'scanning') {
        // Update the activity log entry with scanning progress (message from Rust)
        const logId = transferIdToLogId.current.get(data.transfer_id);
        if (logId && data.message) {
          activityLog.updateEntry(logId, { message: data.message });
        }
      } else if (data.event_type === 'file_start') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        // Use full path from event if available, otherwise fall back to filename
        const displayName = data.path || data.filename;
        // Use full path as key to handle duplicate filenames across subdirectories
        const fileKey = `${data.transfer_id}:${data.path || data.filename}`;
        const fileLogId = humanLog.logRaw(data.direction === 'download' ? 'activity.download_start' : 'activity.upload_start',
          data.direction === 'download' ? 'DOWNLOAD' : 'UPLOAD',
          { filename: displayName, location: loc }, 'running');
        pendingFileLogIds.current.set(fileKey, fileLogId);

        // Add individual file to transfer queue
        const fileDirection = data.direction === 'upload' ? 'upload' : 'download';
        const fileSize = data.progress?.total || 0;
        const fileQueueId = transferQueue.addItem(data.filename, data.path || '', fileSize, fileDirection);
        transferQueue.startTransfer(fileQueueId);
        transferIdToQueueId.current.set(fileKey, fileQueueId);
      } else if (data.event_type === 'file_complete') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        // Use full path as key to match file_start (handles duplicate filenames across subdirs)
        const fileKey = `${data.transfer_id}:${data.path || data.filename}`;
        const existingId = pendingFileLogIds.current.get(fileKey);
        const displayName = data.path || data.filename;
        const successKey = data.direction === 'upload' ? 'activity.upload_success' : 'activity.download_success';
        const msg = t(successKey, { filename: displayName, location: loc, details: '' }).trim();
        if (existingId) {
          activityLog.updateEntry(existingId, { status: 'success', message: msg });
          pendingFileLogIds.current.delete(fileKey);
        } else {
          humanLog.logRaw(successKey, data.direction === 'upload' ? 'UPLOAD' : 'DOWNLOAD',
            { filename: displayName, location: loc, details: '' }, 'success');
        }

        // Complete individual file queue item (key matches file_start)
        const fileQueueId = transferIdToQueueId.current.get(fileKey);
        if (fileQueueId) {
          transferQueue.completeTransfer(fileQueueId);
          transferIdToQueueId.current.delete(fileKey);
        }
      } else if (data.event_type === 'file_error') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const displayName = data.path || data.filename;
        humanLog.logRaw(data.direction === 'download' ? 'activity.download_error' : 'activity.upload_error',
          'ERROR', { filename: displayName, location: loc }, 'error');

        // Fail individual file queue item (key matches file_start)
        const fileErrorKey = `${data.transfer_id}:${data.path || data.filename}`;
        const fileErrQueueId = transferIdToQueueId.current.get(fileErrorKey);
        if (fileErrQueueId) {
          transferQueue.failTransfer(fileErrQueueId, data.message || 'Transfer failed');
          transferIdToQueueId.current.delete(fileErrorKey);
        }
      } else if (data.event_type === 'file_skip') {
        // File skipped due to file_exists_action setting (identical/not newer)
        const displayName = resolveTransferDisplayPath(data, optRef.current.currentLocalPath, optRef.current.currentRemotePath);
        humanLog.logRaw('activity.file_skipped', 'SKIP', { filename: displayName }, 'success');

        // Add skipped file to queue and mark as completed
        const skipDirection = data.direction === 'upload' ? 'upload' : 'download';
        const skipQueueId = transferQueue.addItem(data.filename, '', 0, skipDirection);
        transferQueue.completeTransfer(skipQueueId);
      } else if (data.event_type === 'progress' && data.progress) {
        // Ignore late progress events for already-completed transfers (race condition fix)
        if (!completedTransferIds.current.has(data.transfer_id)) {
          if (data.progress.total_files) {
            // Folder-level progress: merge in last known file speed for display
            setActiveTransfer({ ...data.progress, speed_bps: lastFileSpeedRef.current });
          } else {
            // File-level progress: track speed for folder toast display
            if (data.progress.speed_bps > 0) {
              lastFileSpeedRef.current = data.progress.speed_bps;
            }
            setActiveTransfer(data.progress);
          }
        }

        if (data.transfer_id.includes('folder')) {
          const queueId = transferIdToQueueId.current.get(data.transfer_id);
          if (queueId) {
            transferQueue.updateFolderProgress(queueId, data.progress.total, data.progress.transferred);
          }
        }
      } else if (data.event_type === 'complete') {
        completedTransferIds.current.add(data.transfer_id);
        setActiveTransfer(null);

        let size = '';
        let time = '';
        if (data.message) {
          const match = data.message.match(/\(([^)]+)\)$/);
          if (match) {
            const content = match[1];
            if (content.includes(' in ')) {
              const parts = content.split(' in ');
              size = parts[0];
              time = parts[1];
            } else {
              size = content;
            }
          }
        }

        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const displayName = transferIdToDisplayPath.current.get(data.transfer_id)
          || resolveTransferDisplayPath(data, optRef.current.currentLocalPath, optRef.current.currentRemotePath);
        const successKey = data.direction === 'upload' ? 'activity.upload_success' : 'activity.download_success';
        const details = size && time ? `(${size} in ${time})` : size ? `(${size})` : '';
        const formattedMessage = t(successKey, { filename: displayName, location: loc, details });

        const logId = transferIdToLogId.current.get(data.transfer_id);
        if (logId) {
          activityLog.updateEntry(logId, { status: 'success', message: formattedMessage });
          transferIdToLogId.current.delete(data.transfer_id);
        }
        transferIdToDisplayPath.current.delete(data.transfer_id);

        const queueId = transferIdToQueueId.current.get(data.transfer_id);
        if (queueId) {
          transferQueue.completeTransfer(queueId);
          transferIdToQueueId.current.delete(data.transfer_id);
        }

        if (data.direction === 'upload') optRef.current.loadRemoteFiles();
        else if (data.direction === 'download') optRef.current.loadLocalFiles(optRef.current.currentLocalPath);
      } else if (data.event_type === 'error') {
        setActiveTransfer(null);

        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const displayName = transferIdToDisplayPath.current.get(data.transfer_id)
          || resolveTransferDisplayPath(data, optRef.current.currentLocalPath, optRef.current.currentRemotePath);
        const errorKey = data.direction === 'upload' ? 'activity.upload_error' : 'activity.download_error';
        const formattedMessage = t(errorKey, { filename: displayName, location: loc });

        const logId = transferIdToLogId.current.get(data.transfer_id);
        if (logId) {
          activityLog.updateEntry(logId, { status: 'error', message: formattedMessage });
          transferIdToLogId.current.delete(data.transfer_id);
        } else {
          humanLog.logRaw(errorKey, 'ERROR', { filename: displayName, location: loc }, 'error');
        }

        const queueId = transferIdToQueueId.current.get(data.transfer_id);
        if (queueId) {
          transferQueue.failTransfer(queueId, data.message || 'Transfer failed');
          transferIdToQueueId.current.delete(data.transfer_id);
        }
        transferIdToDisplayPath.current.delete(data.transfer_id);

        notify.error('Transfer Failed', data.message);
      } else if (data.event_type === 'cancelled') {
        setActiveTransfer(null);

        // Update activity log for this transfer
        const logId = transferIdToLogId.current.get(data.transfer_id);
        if (logId) {
          activityLog.updateEntry(logId, { status: 'error', message: data.message || 'Cancelled by user' });
          transferIdToLogId.current.delete(data.transfer_id);
        }

        // Mark the queue item as failed
        const queueId = transferIdToQueueId.current.get(data.transfer_id);
        if (queueId) {
          transferQueue.failTransfer(queueId, 'Cancelled by user');
          transferIdToQueueId.current.delete(data.transfer_id);
        }
        transferIdToDisplayPath.current.delete(data.transfer_id);

        // Clean up any in-progress file log entries that belong to this folder transfer.
        // Keys in pendingFileLogIds are like "dl-folder-123-5:/path/to/file" where
        // the prefix before ":" contains the folder's transfer_id.
        const cancelPrefix = data.transfer_id;
        for (const [fileKey, fileLogId] of pendingFileLogIds.current.entries()) {
          if (fileKey.startsWith(cancelPrefix)) {
            activityLog.updateEntry(fileLogId, { status: 'error', message: 'Cancelled by user' });
            pendingFileLogIds.current.delete(fileKey);
          }
        }
        // Also clean up file-level queue items
        for (const [fileKey, fileQueueId] of transferIdToQueueId.current.entries()) {
          if (fileKey.startsWith(cancelPrefix) && fileKey.includes(':')) {
            transferQueue.failTransfer(fileQueueId, 'Cancelled by user');
            transferIdToQueueId.current.delete(fileKey);
          }
        }

        notify.warning('Transfer Cancelled', data.message);
      }

      // ========== DELETE EVENTS ==========
      else if (data.event_type === 'delete_start') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const displayName = resolveDisplayPath(data, optRef.current.currentLocalPath, optRef.current.currentRemotePath);
        const logId = humanLog.logRaw('activity.delete_start', 'DELETE', { location: loc, filename: displayName }, 'running');
        // Track by transfer_id (like upload/download) so delete_complete can find it
        transferIdToLogId.current.set(data.transfer_id, logId);
        pendingDeleteLogIds.current.set(data.filename, logId);
        pendingDeleteLogIds.current.set(displayName, logId);
      } else if (data.event_type === 'delete_file_start') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const displayName = resolveDisplayPath(data, optRef.current.currentLocalPath, optRef.current.currentRemotePath);
        const logId = humanLog.logRaw('activity.delete_start', 'DELETE', { location: loc, filename: displayName }, 'running');
        pendingDeleteLogIds.current.set(data.filename, logId);
        pendingDeleteLogIds.current.set(displayName, logId);
      } else if (data.event_type === 'delete_file_complete') {
        detailedDeleteCompletedIds.current.add(data.transfer_id);
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const displayName = resolveDisplayPath(data, optRef.current.currentLocalPath, optRef.current.currentRemotePath);
        const existingId = pendingDeleteLogIds.current.get(data.filename) || pendingDeleteLogIds.current.get(displayName);
        if (existingId) {
          const msg = t('activity.delete_file_success', { location: loc, filename: displayName });
          activityLog.updateEntry(existingId, { status: 'success', message: msg });
          pendingDeleteLogIds.current.delete(data.filename);
          pendingDeleteLogIds.current.delete(displayName);
        } else {
          humanLog.logRaw('activity.delete_file_success', 'DELETE', { location: loc, filename: displayName }, 'success');
        }
      } else if (data.event_type === 'delete_dir_complete') {
        detailedDeleteCompletedIds.current.add(data.transfer_id);
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const displayName = resolveDisplayPath(data, optRef.current.currentLocalPath, optRef.current.currentRemotePath);
        const existingId = pendingDeleteLogIds.current.get(data.filename) || pendingDeleteLogIds.current.get(displayName);
        if (existingId) {
          const msg = t('activity.delete_dir_success', { location: loc, filename: displayName });
          activityLog.updateEntry(existingId, { status: 'success', message: msg });
          pendingDeleteLogIds.current.delete(data.filename);
          pendingDeleteLogIds.current.delete(displayName);
        }
      } else if (data.event_type === 'delete_complete') {
        // Update the overall delete log entry to "success" (same pattern as upload/download complete)
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const displayName = resolveDisplayPath(data, optRef.current.currentLocalPath, optRef.current.currentRemotePath);
        const hasDetailedCompletion = detailedDeleteCompletedIds.current.has(data.transfer_id);
        const logId = transferIdToLogId.current.get(data.transfer_id);
        if (!hasDetailedCompletion && logId) {
          const msg = t('activity.delete_success', { location: loc, filename: displayName });
          activityLog.updateEntry(logId, { status: 'success', message: msg });
        }
        transferIdToLogId.current.delete(data.transfer_id);
        detailedDeleteCompletedIds.current.delete(data.transfer_id);
        // Also clean up any remaining pending delete log for this filename
        pendingDeleteLogIds.current.delete(data.filename);
        pendingDeleteLogIds.current.delete(displayName);

        const { loadRemoteFiles, loadLocalFiles, currentLocalPath } = optRef.current;
        if (data.direction === 'remote') loadRemoteFiles();
        else if (data.direction === 'local') loadLocalFiles(currentLocalPath);
      } else if (data.event_type === 'delete_error') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const displayName = resolveDisplayPath(data, optRef.current.currentLocalPath, optRef.current.currentRemotePath);
        // Try transfer_id first (overall delete), then filename (file-level)
        const logId = transferIdToLogId.current.get(data.transfer_id);
        const existingId = logId || pendingDeleteLogIds.current.get(data.filename) || pendingDeleteLogIds.current.get(displayName);
        if (existingId) {
          const msg = t('activity.delete_error', { location: loc, filename: displayName });
          activityLog.updateEntry(existingId, { status: 'error', message: msg });
          if (logId) transferIdToLogId.current.delete(data.transfer_id);
          pendingDeleteLogIds.current.delete(data.filename);
          pendingDeleteLogIds.current.delete(displayName);
        } else {
          humanLog.logRaw('activity.delete_error', 'ERROR', { location: loc, filename: data.message || t('errors.unknown') }, 'error');
        }
      }
    });
    return () => { unlisten.then(fn => fn()); };
  // Subscribe once, never re-subscribe. All mutable values accessed via optRef.current.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return { pendingFileLogIds, pendingDeleteLogIds };
}
