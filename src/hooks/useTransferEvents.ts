// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { guardedUnlisten } from './useTauriListener';
import { TransferEvent, TransferProgress } from '../types';
import { dispatchTransferToast } from '../components/Transfer/TransferToastContainer';
import type { TransferToastLane, TransferToastState } from '../components/Transfer';
import type { ActivityLogContextValue } from './useActivityLog';
import type { useHumanizedLog } from './useHumanizedLog';
import type { useTransferQueue } from '../components/TransferQueue';

export const TRANSFER_EVENT_BRIDGE = 'aeroftp-transfer-event';
export const TRANSFER_BATCH_FINISHED_EVENT = 'aeroftp-transfer-batch-finished';

interface NotifyMethods {
  success: (title: string, message?: string) => string | null;
  error: (title: string, message?: string) => string | null;
  info: (title: string, message?: string) => string | null;
  warning: (title: string, message?: string) => string | null;
}

interface ScanningUpdate {
  active: boolean;
  folderName: string;
  message: string;
  operation: 'delete' | 'download' | 'upload';
}

interface TransferBatchStartedEvent {
  batch_id: string;
  display_name: string;
  direction: 'download' | 'upload';
  total: number;
}

interface UseTransferEventsOptions {
  t: (key: string, params?: Record<string, string | number>) => string;
  activityLog: ActivityLogContextValue;
  humanLog: ReturnType<typeof useHumanizedLog>;
  transferQueue: ReturnType<typeof useTransferQueue>;
  notify: NotifyMethods;
  setActiveTransfer: (transfer: TransferProgress | null) => void;
  loadRemoteFiles: (overrideProtocol?: string) => unknown;
  loadLocalFiles: (path: string) => void;
  currentLocalPath: string;
  currentRemotePath: string;
  /** Called when a transfer starts: used to auto-open Activity Log */
  onTransferStart?: () => void;
  /** Called when scanning state changes (folder scan for delete/download/upload) */
  onScanningUpdate?: (update: ScanningUpdate) => void;
  /** Max concurrent transfers (from speed preset: 1/3/5): controls visible channel slots */
  maxChannels?: number;
}

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
  // Debounce timer for clearing activeTransfer: prevents flicker between consecutive files
  const clearToastTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Tracks whether a streaming scan is still discovering directories (don't dismiss toast on file_start)
  const streamingScanActive = useRef(false);
  const activeToastBatchId = useRef<string | null>(null);
  const toastSummaryRef = useRef<TransferProgress | null>(null);
  const toastLaneAssignments = useRef<Map<string, number>>(new Map());
  const toastLanesRef = useRef<Map<string, TransferToastLane>>(new Map());
  const toastLaneCleanupTimers = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());
  const toastReservedLaneSlots = useRef(0);

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

    const findTrackedEntry = <T,>(map: Map<string, T>, transferId: string, path?: string, filename?: string): { key: string; value: T } | null => {
      const exactKey = `${transferId}:${path || filename || ''}`;
      const exactValue = map.get(exactKey);
      if (exactValue !== undefined) {
        return { key: exactKey, value: exactValue };
      }

      const prefix = `${transferId}:`;
      for (const [key, value] of map.entries()) {
        if (key.startsWith(prefix)) {
          return { key, value };
        }
      }

      return null;
    };

    const cleanupTrackedTransferEntries = (
      transferId: string,
      markQueueItem?: (queueId: string) => void,
      updatePendingFileLog?: (logId: string) => void,
    ): void => {
      const queueIds = new Set<string>();
      for (const [key, queueId] of transferIdToQueueId.current.entries()) {
        if (key === transferId || key.startsWith(`${transferId}:`)) {
          queueIds.add(queueId);
          transferIdToQueueId.current.delete(key);
        }
      }

      if (markQueueItem) {
        for (const queueId of queueIds) {
          markQueueItem(queueId);
        }
      }

      if (updatePendingFileLog) {
        for (const [key, logId] of pendingFileLogIds.current.entries()) {
          if (key.startsWith(`${transferId}:`)) {
            updatePendingFileLog(logId);
            pendingFileLogIds.current.delete(key);
          }
        }
      }
    };

    const isGroupedToastTransfer = (transferId: string): boolean =>
      transferId.includes('folder') || transferId.includes('files');

    const tryResolveToastBatchId = (transferId: string): string | null => {
      const batchId = activeToastBatchId.current;
      if (!batchId) return null;
      if (!isGroupedToastTransfer(batchId)) return null;
      if (transferId === batchId) return batchId;
      if (transferId.startsWith(`${batchId}-`)) return batchId;
      return null;
    };

    const nextToastLaneIndex = (): number => {
      const maxCh = optRef.current.maxChannels ?? 5;
      const used = new Set(toastLaneAssignments.current.values());
      for (let index = 0; index < maxCh; index++) {
        if (!used.has(index)) return index;
      }
      return toastLaneAssignments.current.size;
    };

    /** Evict the oldest completed lane to free its slot for a new file_start */
    const evictOldestCompletedLane = (): void => {
      let oldestId: string | null = null;
      let oldestIdx = Infinity;
      for (const [id, lane] of toastLanesRef.current) {
        if (lane.state === 'completed' || lane.state === 'error') {
          const idx = toastLaneAssignments.current.get(id) ?? Infinity;
          if (idx < oldestIdx) {
            oldestIdx = idx;
            oldestId = id;
          }
        }
      }
      if (oldestId) {
        clearLaneCleanupTimer(oldestId);
        toastLanesRef.current.delete(oldestId);
        toastLaneAssignments.current.delete(oldestId);
      }
    };

    const emitToastState = () => {
      if (!toastSummaryRef.current) {
        dispatchTransferToast(null);
        return;
      }

      const lanes = Array.from(toastLanesRef.current.values())
        .sort((a, b) => (toastLaneAssignments.current.get(a.id) ?? 0) - (toastLaneAssignments.current.get(b.id) ?? 0));

      const toastState: TransferToastState = {
        summary: toastSummaryRef.current,
        lanes,
        reservedLaneSlots: toastReservedLaneSlots.current,
        maxChannels: optRef.current.maxChannels,
      };
      dispatchTransferToast(toastState);
    };

    const clearLaneCleanupTimer = (laneId: string) => {
      const timer = toastLaneCleanupTimers.current.get(laneId);
      if (timer) {
        clearTimeout(timer);
        toastLaneCleanupTimers.current.delete(laneId);
      }
    };

    const scheduleLaneCleanup = (laneId: string) => {
      clearLaneCleanupTimer(laneId);
      const timer = setTimeout(() => {
        toastLanesRef.current.delete(laneId);
        toastLaneAssignments.current.delete(laneId);
        toastLaneCleanupTimers.current.delete(laneId);
        emitToastState();
      }, 600);
      toastLaneCleanupTimers.current.set(laneId, timer);
    };

    const unlistenBatchStarted = listen<TransferBatchStartedEvent>('transfer_batch_started', (event) => {
      const data = event.payload;
      if (!data?.batch_id || !data.batch_id.includes('files')) {
        return;
      }
      if (clearToastTimer.current) {
        clearTimeout(clearToastTimer.current);
        clearToastTimer.current = null;
      }
      activeToastBatchId.current = data.batch_id;
      const batchSummary: TransferProgress = {
        transfer_id: data.batch_id,
        filename: data.display_name,
        transferred: 0,
        total: data.total,
        percentage: 0,
        speed_bps: 0,
        eta_seconds: 0,
        direction: data.direction,
        total_files: data.total,
        path: transferIdToDisplayPath.current.get(data.batch_id),
      };
      toastSummaryRef.current = batchSummary;
      toastLaneAssignments.current.clear();
      toastLanesRef.current.clear();
      toastReservedLaneSlots.current = 0;
      for (const timer of toastLaneCleanupTimers.current.values()) clearTimeout(timer);
      toastLaneCleanupTimers.current.clear();
      optRef.current.onTransferStart?.();
      optRef.current.setActiveTransfer(batchSummary);
      dispatchTransferToast({ summary: batchSummary });
    });

    const unlisten = listen<TransferEvent>('transfer_event', (event) => {
      const { t, activityLog, humanLog, transferQueue, notify, setActiveTransfer } = optRef.current;
      const data = event.payload;
      window.dispatchEvent(new CustomEvent<TransferEvent>(TRANSFER_EVENT_BRIDGE, { detail: data }));

      // Cross-profile transfers render progress *inside* CrossProfilePanel.
      // Log to Activity Log (the user wants traceability), but skip the floating
      // toast and the auto-opened queue: those compete with the panel's own UI.
      if (data.direction === 'cross-profile') {
        if (data.event_type === 'start') {
          const src = data.message?.split(' -> ')[0] || '';
          const dst = data.message?.split(' -> ')[1] || '';
          const logId = humanLog.logRaw(
            'activity.crossprofile_start',
            'INFO',
            { count: data.filename, source: src, dest: dst },
            'running',
          );
          transferIdToLogId.current.set(data.transfer_id, logId);
        } else if (data.event_type === 'file_complete' && data.progress) {
          humanLog.logRaw(
            'activity.crossprofile_file_success',
            'UPLOAD',
            { filename: data.filename, index: data.progress.transferred, total: data.progress.total },
            'success',
          );
        } else if (data.event_type === 'file_error') {
          humanLog.logRaw(
            'activity.crossprofile_file_error',
            'ERROR',
            { filename: data.filename, error: data.message || '' },
            'error',
          );
        } else if (data.event_type === 'complete' || data.event_type === 'cancelled') {
          const logId = transferIdToLogId.current.get(data.transfer_id);
          if (logId) {
            activityLog.updateEntry(logId, {
              status: data.event_type === 'cancelled' ? 'error' : 'success',
              message: data.message || '',
            });
            transferIdToLogId.current.delete(data.transfer_id);
          }
        }
        return;
      }

      // ========== TRANSFER EVENTS (download/upload) ==========
      if (data.event_type === 'start') {
        // Auto-open activity log on transfer start
        optRef.current.onTransferStart?.();
        if (clearToastTimer.current) {
          clearTimeout(clearToastTimer.current);
          clearToastTimer.current = null;
        }
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
        activeToastBatchId.current = data.transfer_id;
        toastSummaryRef.current = data.progress
          ? { ...data.progress, path: data.progress.path || data.path }
          : !isGroupedToastTransfer(data.transfer_id)
            ? {
              transfer_id: data.transfer_id,
              filename: data.filename,
              transferred: 0,
              total: 0,
              percentage: 0,
              speed_bps: 0,
              eta_seconds: 0,
              direction: data.direction === 'upload' ? 'upload' : 'download',
              path: displayName,
            }
          : null;
        toastLaneAssignments.current.clear();
        toastLanesRef.current.clear();
        toastReservedLaneSlots.current = 0;
        for (const timer of toastLaneCleanupTimers.current.values()) clearTimeout(timer);
        toastLaneCleanupTimers.current.clear();
        if (toastSummaryRef.current) {
          setActiveTransfer(toastSummaryRef.current);
          dispatchTransferToast({ summary: toastSummaryRef.current });
          emitToastState();
        }

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
        // Track streaming scan state: if message contains "dirs queued", scan is still active
        const stillScanning = data.message?.includes('dirs queued') && !data.message?.includes('0 dirs queued');
        streamingScanActive.current = !!stillScanning;
        // Notify ScanningToast
        const scanOp = data.direction === 'remote' ? 'delete' as const
          : data.direction === 'upload' ? 'upload' as const
          : 'download' as const;
        optRef.current.onScanningUpdate?.({
          active: true,
          folderName: data.filename || '',
          message: data.message || '',
          operation: scanOp,
        });
      } else if (data.event_type === 'file_start') {
        // In streaming mode (scan + transfer interleaved), keep the toast visible
        // so it doesn't flicker between scan and transfer phases.
        // Only dismiss when scan is fully complete (no dirs queued).
        if (!streamingScanActive.current) {
          optRef.current.onScanningUpdate?.({ active: false, folderName: '', message: '', operation: 'download' });
        }
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
        const queuePath = data.path || '';
        const existingPendingItem = transferQueue.items.find((item: {
          id: string;
          filename: string;
          path: string;
          status: string;
          type: string;
        }) =>
          item.status === 'pending'
          && item.type === fileDirection
          && item.filename === data.filename
          && item.path === queuePath
        );
        const fileQueueId = existingPendingItem?.id
          || transferQueue.addItem(data.filename, queuePath, fileSize, fileDirection);
        transferQueue.startTransfer(fileQueueId);
        transferIdToQueueId.current.set(fileKey, fileQueueId);
        transferIdToQueueId.current.set(data.transfer_id, fileQueueId);
        if (tryResolveToastBatchId(data.transfer_id)) {
          const maxCh = optRef.current.maxChannels ?? 5;
          clearLaneCleanupTimer(data.transfer_id);
          if (!toastLaneAssignments.current.has(data.transfer_id)) {
            // If all slots are full, evict the oldest completed lane to make room
            if (toastLaneAssignments.current.size >= maxCh) {
              evictOldestCompletedLane();
            }
            // Only assign a slot if one was freed (avoid phantom lanes beyond maxChannels)
            if (toastLaneAssignments.current.size < maxCh) {
              toastLaneAssignments.current.set(data.transfer_id, nextToastLaneIndex());
            }
          }
          toastLanesRef.current.set(data.transfer_id, {
            id: data.transfer_id,
            filename: data.filename,
            transferred: 0,
            total: fileSize,
            percentage: 0,
            speed_bps: 0,
            eta_seconds: 0,
            direction: fileDirection,
            path: data.path,
            state: 'active',
          });
          toastReservedLaneSlots.current = Math.min(
            Math.max(toastReservedLaneSlots.current, toastLanesRef.current.size),
            maxCh,
          );
          emitToastState();
        }
      } else if (data.event_type === 'file_complete') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const trackedLog = findTrackedEntry(pendingFileLogIds.current, data.transfer_id, data.path, data.filename);
        const displayName = data.path || data.filename;
        const successKey = data.direction === 'upload' ? 'activity.upload_success' : 'activity.download_success';
        const msg = t(successKey, { filename: displayName, location: loc, details: '' }).trim();
        if (trackedLog) {
          activityLog.updateEntry(trackedLog.value, { status: 'success', message: msg });
          pendingFileLogIds.current.delete(trackedLog.key);
        } else {
          humanLog.logRaw(successKey, data.direction === 'upload' ? 'UPLOAD' : 'DOWNLOAD',
            { filename: displayName, location: loc, details: '' }, 'success');
        }

        const trackedQueue = findTrackedEntry(transferIdToQueueId.current, data.transfer_id, data.path, data.filename);
        if (trackedQueue) {
          transferQueue.completeTransfer(trackedQueue.value);
          transferIdToQueueId.current.delete(trackedQueue.key);
        }
        transferIdToQueueId.current.delete(data.transfer_id);
        const existingLane = toastLanesRef.current.get(data.transfer_id);
        if (existingLane) {
          toastLanesRef.current.set(data.transfer_id, {
            ...existingLane,
            transferred: existingLane.total,
            percentage: 100,
            speed_bps: 0,
            eta_seconds: 0,
            state: 'completed',
          });
          scheduleLaneCleanup(data.transfer_id);
        }
        emitToastState();
      } else if (data.event_type === 'file_error') {
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const displayName = data.path || data.filename;
        humanLog.logRaw(data.direction === 'download' ? 'activity.download_error' : 'activity.upload_error',
          'ERROR', { filename: displayName, location: loc }, 'error');

        const trackedQueue = findTrackedEntry(transferIdToQueueId.current, data.transfer_id, data.path, data.filename);
        if (trackedQueue) {
          transferQueue.failTransfer(trackedQueue.value, data.message || 'Transfer failed');
          transferIdToQueueId.current.delete(trackedQueue.key);
        }
        transferIdToQueueId.current.delete(data.transfer_id);
        const existingLane = toastLanesRef.current.get(data.transfer_id);
        if (existingLane) {
          toastLanesRef.current.set(data.transfer_id, {
            ...existingLane,
            speed_bps: 0,
            eta_seconds: 0,
            state: 'error',
          });
          scheduleLaneCleanup(data.transfer_id);
        }
        emitToastState();
      } else if (data.event_type === 'file_skip') {
        // File skipped due to file_exists_action setting (identical/not newer)
        const displayName = resolveTransferDisplayPath(data, optRef.current.currentLocalPath, optRef.current.currentRemotePath);
        humanLog.logRaw('activity.file_skipped', 'INFO', { filename: displayName }, 'success');

        // Add skipped file to queue and mark as completed
        const skipDirection = data.direction === 'upload' ? 'upload' : 'download';
        const skipQueueId = transferQueue.addItem(data.filename, '', 0, skipDirection);
        transferQueue.completeTransfer(skipQueueId);
      } else if (data.event_type === 'progress' && data.progress) {
        // Cancel any pending toast-clear timer (prevents flicker between consecutive files)
        if (clearToastTimer.current) {
          clearTimeout(clearToastTimer.current);
          clearToastTimer.current = null;
        }
        // Ignore late progress events for already-completed transfers (race condition fix)
          if (!completedTransferIds.current.has(data.transfer_id)) {
            // Track file-level speed regardless of throttle
            if (!data.progress.total_files && data.progress.speed_bps > 0) {
              lastFileSpeedRef.current = data.progress.speed_bps;
            }
            if (!data.progress.total_files) {
              const fileQueueId = transferIdToQueueId.current.get(data.transfer_id);
              if (fileQueueId) {
                transferQueue.setProgress(fileQueueId, data.progress.percentage);
              }
            }
            // Signal App that a transfer is active (boolean only: no frequent re-renders)
            setActiveTransfer(data.progress);
          // Dispatch progress to isolated TransferToastContainer (no App re-render)
          if (data.progress.total_files) {
            // Aggregate speed from active lanes (more reliable than single-file lastFileSpeedRef)
            let aggregatedSpeed = lastFileSpeedRef.current;
            if (toastLanesRef.current.size > 0) {
              let laneSpeedSum = 0;
              for (const lane of toastLanesRef.current.values()) {
                if (lane.state === 'active' || !lane.state) laneSpeedSum += lane.speed_bps;
              }
              if (laneSpeedSum > 0) aggregatedSpeed = laneSpeedSum;
            }
            toastSummaryRef.current = { ...data.progress, speed_bps: aggregatedSpeed };
            emitToastState();
          } else {
            const batchId = tryResolveToastBatchId(data.transfer_id);
            if (batchId) {
              const maxCh = optRef.current.maxChannels ?? 5;
              clearLaneCleanupTimer(data.transfer_id);
              if (!toastLaneAssignments.current.has(data.transfer_id)) {
                if (toastLaneAssignments.current.size >= maxCh) {
                  evictOldestCompletedLane();
                }
                if (toastLaneAssignments.current.size < maxCh) {
                  toastLaneAssignments.current.set(data.transfer_id, nextToastLaneIndex());
                }
              }
              toastLanesRef.current.set(data.transfer_id, {
                id: data.transfer_id,
                filename: data.progress.filename,
                transferred: data.progress.transferred,
                total: data.progress.total,
                percentage: data.progress.percentage,
                speed_bps: data.progress.speed_bps,
                eta_seconds: data.progress.eta_seconds,
                direction: data.progress.direction,
                path: data.progress.path || data.path,
                state: 'active',
              });
              toastReservedLaneSlots.current = Math.min(
                Math.max(toastReservedLaneSlots.current, toastLanesRef.current.size),
                maxCh,
              );
              emitToastState();
            } else {
              dispatchTransferToast({ summary: data.progress });
            }
          }
        }

        if (data.transfer_id.includes('folder')) {
          const queueId = transferIdToQueueId.current.get(data.transfer_id);
          if (queueId) {
            transferQueue.updateFolderProgress(queueId, data.progress.total, data.progress.transferred);
          }
        }
      } else if (data.event_type === 'complete') {
        window.dispatchEvent(new CustomEvent(TRANSFER_BATCH_FINISHED_EVENT, { detail: data }));
        // Dismiss scanning toast when transfer completes and reset streaming scan state
        streamingScanActive.current = false;
        optRef.current.onScanningUpdate?.({ active: false, folderName: '', message: '', operation: 'download' });
        completedTransferIds.current.add(data.transfer_id);
        // Prevent unbounded growth: trim oldest entries when exceeding cap
        if (completedTransferIds.current.size > 500) {
          const iter = completedTransferIds.current.values();
          for (let i = 0; i < 250; i++) iter.next();
          // Keep only the last 250 entries
          const keep = new Set<string>();
          for (const v of iter) keep.add(v);
          completedTransferIds.current = keep;
        }
        // Debounce toast dismiss: wait 500ms before clearing to prevent flicker
        // between consecutive file transfers. If a new progress event arrives first,
        // the timer is cancelled and the toast stays visible.
        if (clearToastTimer.current) clearTimeout(clearToastTimer.current);
        clearToastTimer.current = setTimeout(() => {
          setActiveTransfer(null);
          dispatchTransferToast(null);
          activeToastBatchId.current = null;
          toastSummaryRef.current = null;
          toastLaneAssignments.current.clear();
          toastLanesRef.current.clear();
          toastReservedLaneSlots.current = 0;
          for (const timer of toastLaneCleanupTimers.current.values()) clearTimeout(timer);
          toastLaneCleanupTimers.current.clear();
          clearToastTimer.current = null;
        }, 500);

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
        cleanupTrackedTransferEntries(
          data.transfer_id,
          (orphanQueueId) => transferQueue.completeTransfer(orphanQueueId),
          (orphanLogId) => activityLog.updateEntry(orphanLogId, { status: 'success', message: formattedMessage })
        );

        if (data.direction === 'upload') optRef.current.loadRemoteFiles();
        else if (data.direction === 'download') optRef.current.loadLocalFiles(optRef.current.currentLocalPath);
      } else if (data.event_type === 'error') {
        setActiveTransfer(null);
        dispatchTransferToast(null);
        activeToastBatchId.current = null;
        toastSummaryRef.current = null;
        toastLaneAssignments.current.clear();
        toastLanesRef.current.clear();
        toastReservedLaneSlots.current = 0;
        for (const timer of toastLaneCleanupTimers.current.values()) clearTimeout(timer);
        toastLaneCleanupTimers.current.clear();

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
        cleanupTrackedTransferEntries(
          data.transfer_id,
          (orphanQueueId) => transferQueue.failTransfer(orphanQueueId, data.message || 'Transfer failed'),
          (orphanLogId) => activityLog.updateEntry(orphanLogId, { status: 'error', message: data.message || formattedMessage })
        );
        transferIdToDisplayPath.current.delete(data.transfer_id);

        notify.error(t('transfer.failed'), data.message);
      } else if (data.event_type === 'cancelled') {
        window.dispatchEvent(new CustomEvent(TRANSFER_BATCH_FINISHED_EVENT, { detail: data }));
        streamingScanActive.current = false;
        optRef.current.onScanningUpdate?.({ active: false, folderName: '', message: '', operation: 'download' });
        setActiveTransfer(null);
        dispatchTransferToast(null);
        activeToastBatchId.current = null;
        toastSummaryRef.current = null;
        toastLaneAssignments.current.clear();
        toastLanesRef.current.clear();
        toastReservedLaneSlots.current = 0;
        for (const timer of toastLaneCleanupTimers.current.values()) clearTimeout(timer);
        toastLaneCleanupTimers.current.clear();

        const cancelledMsg = t('transfer.cancelledByUser');

        // Update activity log for this transfer
        const logId = transferIdToLogId.current.get(data.transfer_id);
        if (logId) {
          activityLog.updateEntry(logId, { status: 'error', message: data.message || cancelledMsg });
          transferIdToLogId.current.delete(data.transfer_id);
        }

        // Mark the queue item as failed
        const queueId = transferIdToQueueId.current.get(data.transfer_id);
        if (queueId) {
          transferQueue.failTransfer(queueId, cancelledMsg);
          transferIdToQueueId.current.delete(data.transfer_id);
        }
        transferIdToDisplayPath.current.delete(data.transfer_id);

        cleanupTrackedTransferEntries(
          data.transfer_id,
          (orphanQueueId) => transferQueue.failTransfer(orphanQueueId, cancelledMsg),
          (orphanLogId) => activityLog.updateEntry(orphanLogId, { status: 'error', message: cancelledMsg })
        );

        notify.warning(t('transfer.cancelled'), data.message);
      }

      // ========== DELETE EVENTS ==========
      else if (data.event_type === 'delete_start') {
        optRef.current.onTransferStart?.();
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const displayName = resolveDisplayPath(data, optRef.current.currentLocalPath, optRef.current.currentRemotePath);
        const logId = humanLog.logRaw('activity.delete_start', 'DELETE', { location: loc, filename: displayName }, 'running');
        // Track by transfer_id (like upload/download) so delete_complete can find it
        transferIdToLogId.current.set(data.transfer_id, logId);
        pendingDeleteLogIds.current.set(data.filename, logId);
        pendingDeleteLogIds.current.set(displayName, logId);
        // Show scanning toast for folder deletions (message contains "Scanning")
        if (data.message && data.message.includes('Scanning')) {
          optRef.current.onScanningUpdate?.({
            active: true,
            folderName: data.filename || '',
            message: t('scanning.preparing'),
            operation: 'delete',
          });
        }
      } else if (data.event_type === 'delete_file_start') {
        // Dismiss scanning toast: actual deletion has started
        optRef.current.onScanningUpdate?.({ active: false, folderName: '', message: '', operation: 'delete' });
        const loc = data.direction === 'remote' ? t('browser.remote') : t('browser.local');
        const displayName = resolveDisplayPath(data, optRef.current.currentLocalPath, optRef.current.currentRemotePath);
        const logId = humanLog.logRaw('activity.delete_start', 'DELETE', { location: loc, filename: displayName }, 'running');
        pendingDeleteLogIds.current.set(data.filename, logId);
        pendingDeleteLogIds.current.set(displayName, logId);
      } else if (data.event_type === 'delete_file_complete') {
        detailedDeleteCompletedIds.current.add(data.transfer_id);
        // Prevent unbounded growth
        if (detailedDeleteCompletedIds.current.size > 500) {
          detailedDeleteCompletedIds.current.clear();
        }
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
      } else if (data.event_type === 'delete_complete' || data.event_type === 'delete_cancelled') {
        // Dismiss scanning toast
        optRef.current.onScanningUpdate?.({ active: false, folderName: '', message: '', operation: 'delete' });
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
        // Dismiss scanning toast on error
        optRef.current.onScanningUpdate?.({ active: false, folderName: '', message: '', operation: 'delete' });
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
    const disposeUnlisten = guardedUnlisten(unlisten);
    const disposeBatchStarted = guardedUnlisten(unlistenBatchStarted);

    return () => {
      // Clear the debounced toast dismiss so it cannot fire after unmount
      // (would otherwise call setActiveTransfer on a dead tree).
      if (clearToastTimer.current) {
        clearTimeout(clearToastTimer.current);
        clearToastTimer.current = null;
      }
      for (const timer of toastLaneCleanupTimers.current.values()) clearTimeout(timer);
      toastLaneCleanupTimers.current.clear();
      disposeUnlisten();
      disposeBatchStarted();
    };
  // Subscribe once, never re-subscribe. All mutable values accessed via optRef.current.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return { pendingFileLogIds, pendingDeleteLogIds };
}
