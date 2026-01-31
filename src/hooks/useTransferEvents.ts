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
  loadRemoteFiles: () => void;
  loadLocalFiles: (path: string) => void;
  currentLocalPath: string;
}
/* eslint-enable @typescript-eslint/no-explicit-any */

export function useTransferEvents(options: UseTransferEventsOptions) {
  const {
    t, activityLog, humanLog, transferQueue, notify,
    setActiveTransfer, loadRemoteFiles, loadLocalFiles, currentLocalPath,
  } = options;

  // Correlation maps between backend transfer IDs and frontend UI elements
  const transferIdToQueueId = useRef<Map<string, string>>(new Map());
  const transferIdToLogId = useRef<Map<string, string>>(new Map());
  const pendingFileLogIds = useRef<Map<string, string>>(new Map());
  const pendingDeleteLogIds = useRef<Map<string, string>>(new Map());

  // Use refs for callbacks to avoid stale closures without re-subscribing
  const callbacksRef = useRef({ loadRemoteFiles, loadLocalFiles, currentLocalPath });
  callbacksRef.current = { loadRemoteFiles, loadLocalFiles, currentLocalPath };

  useEffect(() => {
    const unlisten = listen<TransferEvent>('transfer_event', (event) => {
      const data = event.payload;

      // ========== TRANSFER EVENTS (download/upload) ==========
      if (data.event_type === 'start') {
        let logId = '';
        // Check if we have a pending manual log for this file (deduplication)
        if (pendingFileLogIds.current.has(data.filename)) {
          logId = pendingFileLogIds.current.get(data.filename)!;
          pendingFileLogIds.current.delete(data.filename);
        } else {
          logId = humanLog.logStart(data.direction === 'download' ? 'DOWNLOAD' : 'UPLOAD', { filename: data.filename });
        }
        transferIdToLogId.current.set(data.transfer_id, logId);

        const queueItem = transferQueue.items.find((i: { filename: string; status: string; id: string }) => i.filename === data.filename && i.status === 'pending');
        if (queueItem) {
          transferIdToQueueId.current.set(data.transfer_id, queueItem.id);
          transferQueue.markAsFolder(queueItem.id);
          transferQueue.startTransfer(queueItem.id);
        }
      } else if (data.event_type === 'file_start') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const fileLogId = humanLog.logRaw(data.direction === 'download' ? 'activity.download_start' : 'activity.upload_start',
          data.direction === 'download' ? 'DOWNLOAD' : 'UPLOAD',
          { filename: data.filename, location: loc }, 'running');
        pendingFileLogIds.current.set(`${data.transfer_id}:${data.filename}`, fileLogId);
      } else if (data.event_type === 'file_complete') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const key = `${data.transfer_id}:${data.filename}`;
        const existingId = pendingFileLogIds.current.get(key);
        const successKey = data.direction === 'upload' ? 'activity.upload_success' : 'activity.download_success';
        const msg = t(successKey, { filename: data.filename, location: loc, details: '' }).trim();
        if (existingId) {
          activityLog.updateEntry(existingId, { status: 'success', message: msg });
          pendingFileLogIds.current.delete(key);
        } else {
          humanLog.logRaw(successKey, data.direction === 'upload' ? 'UPLOAD' : 'DOWNLOAD',
            { filename: data.filename, location: loc, details: '' }, 'success');
        }
      } else if (data.event_type === 'file_error') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        humanLog.logRaw(data.direction === 'download' ? 'activity.download_error' : 'activity.upload_error',
          'ERROR', { filename: data.filename, location: loc }, 'error');
      } else if (data.event_type === 'progress' && data.progress) {
        setActiveTransfer(data.progress);

        if (data.transfer_id.includes('folder')) {
          const queueId = transferIdToQueueId.current.get(data.transfer_id);
          if (queueId) {
            transferQueue.updateFolderProgress(queueId, data.progress.total, data.progress.transferred);
          }
        }
      } else if (data.event_type === 'complete') {
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
        const successKey = data.direction === 'upload' ? 'activity.upload_success' : 'activity.download_success';
        const details = size && time ? `(${size} in ${time})` : size ? `(${size})` : '';
        const formattedMessage = t(successKey, { filename: data.filename, location: loc, details });

        const logId = transferIdToLogId.current.get(data.transfer_id);
        if (logId) {
          activityLog.updateEntry(logId, { status: 'success', message: formattedMessage });
          transferIdToLogId.current.delete(data.transfer_id);
        }

        const queueId = transferIdToQueueId.current.get(data.transfer_id);
        if (queueId) {
          transferQueue.completeTransfer(queueId);
          transferIdToQueueId.current.delete(data.transfer_id);
        }

        if (data.direction === 'upload') callbacksRef.current.loadRemoteFiles();
        else if (data.direction === 'download') callbacksRef.current.loadLocalFiles(callbacksRef.current.currentLocalPath);
      } else if (data.event_type === 'error') {
        setActiveTransfer(null);

        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const errorKey = data.direction === 'upload' ? 'activity.upload_error' : 'activity.download_error';
        const formattedMessage = t(errorKey, { filename: data.filename, location: loc });

        const logId = transferIdToLogId.current.get(data.transfer_id);
        if (logId) {
          activityLog.updateEntry(logId, { status: 'error', message: formattedMessage });
          transferIdToLogId.current.delete(data.transfer_id);
        } else {
          humanLog.logRaw(errorKey, 'ERROR', { filename: data.filename, location: loc }, 'error');
        }

        const queueId = transferIdToQueueId.current.get(data.transfer_id);
        if (queueId) {
          transferQueue.failTransfer(queueId, data.message || 'Transfer failed');
          transferIdToQueueId.current.delete(data.transfer_id);
        }

        notify.error('Transfer Failed', data.message);
      } else if (data.event_type === 'cancelled') {
        setActiveTransfer(null);
        notify.warning('Transfer Cancelled', data.message);
      }

      // ========== DELETE EVENTS ==========
      else if (data.event_type === 'delete_start') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const logId = humanLog.logRaw('activity.delete_start', 'DELETE', { location: loc, filename: data.filename }, 'running');
        pendingDeleteLogIds.current.set(data.filename, logId);
      } else if (data.event_type === 'delete_file_start') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const logId = humanLog.logRaw('activity.delete_start', 'DELETE', { location: loc, filename: data.filename }, 'running');
        pendingDeleteLogIds.current.set(data.filename, logId);
      } else if (data.event_type === 'delete_file_complete') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const existingId = pendingDeleteLogIds.current.get(data.filename);
        if (existingId) {
          const msg = t('activity.delete_file_success', { location: loc, filename: data.filename });
          activityLog.updateEntry(existingId, { status: 'success', message: msg });
          pendingDeleteLogIds.current.delete(data.filename);
        } else {
          humanLog.logRaw('activity.delete_file_success', 'DELETE', { location: loc, filename: data.filename }, 'success');
        }
      } else if (data.event_type === 'delete_dir_complete') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const existingId = pendingDeleteLogIds.current.get(data.filename);
        if (existingId) {
          const msg = t('activity.delete_dir_success', { location: loc, filename: data.filename });
          activityLog.updateEntry(existingId, { status: 'success', message: msg });
          pendingDeleteLogIds.current.delete(data.filename);
        } else {
          humanLog.logRaw('activity.delete_dir_success', 'DELETE', { location: loc, filename: data.filename }, 'success');
        }
      } else if (data.event_type === 'delete_complete') {
        if (data.direction === 'remote') loadRemoteFiles();
        else if (data.direction === 'local') loadLocalFiles(currentLocalPath);
      } else if (data.event_type === 'delete_error') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const existingId = pendingDeleteLogIds.current.get(data.filename);
        if (existingId) {
          const msg = t('activity.delete_error', { location: loc, filename: data.filename });
          activityLog.updateEntry(existingId, { status: 'error', message: msg });
          pendingDeleteLogIds.current.delete(data.filename);
        } else {
          humanLog.logRaw('activity.delete_error', 'ERROR', { location: loc, filename: data.message || t('errors.unknown') }, 'error');
        }
      }
    });
    return () => { unlisten.then(fn => fn()); };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activityLog, transferQueue]);

  return { pendingFileLogIds, pendingDeleteLogIds };
}
