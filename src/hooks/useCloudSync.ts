/**
 * useCloudSync Hook
 * Extracted from App.tsx during modularization (v1.4.1)
 *
 * Manages AeroCloud state, event listeners, and tray menu actions:
 *   - Cloud config initialization on mount
 *   - cloud-sync-status event listener (active/idle/syncing/error/disabled)
 *   - cloud_sync_complete event listener
 *   - menu-event listener for tray actions (sync_now, pause, open_folder, check_update)
 */

import { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { logger } from '../utils/logger';
import type { ActivityLogContextValue } from './useActivityLog';
import type { useHumanizedLog } from './useHumanizedLog';

interface UseCloudSyncOptions {
  activityLog: ActivityLogContextValue;
  humanLog: ReturnType<typeof useHumanizedLog>;
  t: (key: string, params?: Record<string, string | number>) => string;
  checkForUpdate: (manual?: boolean) => void;
  isAppLocked: boolean;
}

export function useCloudSync(options: UseCloudSyncOptions) {
  const { activityLog, humanLog, t, checkForUpdate, isAppLocked } = options;

  // Cloud state
  const [showCloudPanel, setShowCloudPanel] = useState(false);
  const [cloudSyncing, setCloudSyncing] = useState(false);
  const [isCloudActive, setIsCloudActive] = useState(false);
  const [cloudServerName, setCloudServerName] = useState<string>('');
  const [cloudLastSync, setCloudLastSync] = useState<string | null>(null);
  const [cloudLocalFolder, setCloudLocalFolder] = useState<string>('');
  const [cloudRemoteFolder, setCloudRemoteFolder] = useState<string>('');
  const [cloudPublicUrlBase, setCloudPublicUrlBase] = useState<string>('');

  // Refs for callbacks to avoid stale closures without re-subscribing
  const callbacksRef = useRef({ activityLog, humanLog, t, checkForUpdate, cloudServerName });
  callbacksRef.current = { activityLog, humanLog, t, checkForUpdate, cloudServerName };

  useEffect(() => {
    const formatBytes = (bytes: number): string => {
      if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
      const units = ['B', 'KB', 'MB', 'GB', 'TB'];
      const exp = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
      const value = bytes / Math.pow(1024, exp);
      return `${value.toFixed(value >= 10 || exp === 0 ? 0 : 1)} ${units[exp]}`;
    };

    // Check initial cloud config
    const checkCloudConfig = async () => {
      try {
        const config = await invoke<{
          enabled: boolean;
          cloud_name?: string;
          server_profile?: string;
          last_sync?: string;
          local_folder?: string;
          remote_folder?: string;
          public_url_base?: string | null;
        }>('get_cloud_config');
        setIsCloudActive(config.enabled);
        if (config.cloud_name) {
          setCloudServerName(config.cloud_name);
        } else if (config.server_profile) {
          setCloudServerName(config.server_profile);
        }
        if (config.last_sync) setCloudLastSync(config.last_sync);
        if (config.local_folder) setCloudLocalFolder(config.local_folder);
        if (config.remote_folder) setCloudRemoteFolder(config.remote_folder);
        if (config.public_url_base) setCloudPublicUrlBase(config.public_url_base);
      } catch (e) {
        console.error('Failed to check cloud config:', e);
      }
    };
    checkCloudConfig();

    // Debounce cloud sync status logs to avoid duplicates from React StrictMode
    let lastCloudLogStatus = '';
    let lastCloudLogTime = 0;
    let cloudSyncLogId: string | null = null;

    // Listen for cloud sync status events
    const unlistenStatus = listen<{ status: string; message: string }>('cloud-sync-status', (event) => {
      const { status, message } = event.payload;
      const { activityLog: al, humanLog: hl, t: tr, cloudServerName: csn } = callbacksRef.current;
      logger.debug('Cloud status:', status, message);

      const now = Date.now();
      if (status === lastCloudLogStatus && now - lastCloudLogTime < 500) return;
      lastCloudLogStatus = status;
      lastCloudLogTime = now;

      if (status === 'active') {
        setCloudSyncing(false);
        setIsCloudActive(true);
        setCloudLastSync(new Date().toISOString());
        // Don't update log here — cloud_sync_complete handles the summary log
        // to avoid duplicate entries. Just clear syncing state.
      } else if (status === 'idle') {
        setCloudSyncing(false);
        setIsCloudActive(true);
      } else if (status === 'syncing') {
        setCloudSyncing(true);
        setIsCloudActive(true);
        if (!cloudSyncLogId) {
          cloudSyncLogId = hl.logRaw('activity.sync_start', 'INFO', { server: csn }, 'running');
        } else {
          al.updateEntry(cloudSyncLogId, {
            status: 'running',
            message: tr('activity.sync_start', { server: csn })
          });
        }
      } else if (status === 'error') {
        setCloudSyncing(false);
        console.error('Cloud sync error:', message);
        if (cloudSyncLogId) {
          al.updateEntry(cloudSyncLogId, {
            status: 'error',
            message: tr('activity.sync_error', { server: csn, message })
          });
          cloudSyncLogId = null;
        } else {
          hl.logRaw('activity.sync_error', 'ERROR', { server: csn, message }, 'error');
        }
      } else if (status === 'disabled') {
        setCloudSyncing(false);
        setIsCloudActive(false);
      }
    });

    // Listen for tray menu events
    const unlistenMenu = listen<string>('menu-event', async (event) => {
      const action = event.payload;
      const { activityLog: al, checkForUpdate: cfu, t: tr } = callbacksRef.current;
      logger.debug('Tray menu action:', action);

      if (action === 'cloud_sync_now') {
        try {
          al.log('INFO', tr('cloud.manualSyncStarted'), 'success');
          await invoke('trigger_cloud_sync');
        } catch (e) {
          al.log('INFO', tr('cloud.syncFailed', { error: String(e) }), 'error');
        }
      } else if (action === 'cloud_pause') {
        try {
          await invoke('stop_background_sync');
          logger.debug('Background sync paused from tray');
        } catch (e) {
          console.error('Failed to pause sync:', e);
        }
      } else if (action === 'cloud_open_folder') {
        try {
          const config = await invoke<{ local_folder: string }>('get_cloud_config');
          if (config.local_folder) {
            await invoke('open_in_file_manager', { path: config.local_folder });
          }
        } catch (e) {
          console.error('Failed to open cloud folder:', e);
        }
      } else if (action === 'check_update') {
        cfu(true);
      }
    });

    // Listen for cloud sync completion — update the existing "Syncing..." log entry
    // instead of creating a duplicate. Clear cloudSyncLogId so the later 'active' status
    // event doesn't create yet another update.
    const unlistenSyncComplete = listen<{
      uploaded: number;
      downloaded: number;
      errors: string[];
      file_details?: { path: string; direction: string; size: number }[];
    }>('cloud_sync_complete', (event) => {
      const { activityLog: al, t: tr } = callbacksRef.current;
      const result = event.payload;
      const hasErrors = result.errors.length > 0;
      const syncedFiles = Array.isArray(result.file_details) ? result.file_details : [];
      const details = undefined;
      const msg = hasErrors
        ? tr('cloud.syncError', { count: result.errors.length.toString() })
        : tr('cloud.syncComplete', { uploaded: result.uploaded.toString(), downloaded: result.downloaded.toString() });

      if (cloudSyncLogId) {
        al.updateEntry(cloudSyncLogId, {
          status: hasErrors ? 'error' : 'success',
          message: msg,
          details,
        });
        cloudSyncLogId = null;
      } else {
        al.log('INFO', msg, hasErrors ? 'error' : 'success', details);
      }

      // Also emit per-file records for clearer activity browsing/copying
      if (!hasErrors && syncedFiles.length > 0) {
        const MAX_FILE_RECORDS = 30;
        const visibleFiles = syncedFiles.slice(0, MAX_FILE_RECORDS);

        for (const file of visibleFiles) {
          const isUpload = file.direction === 'upload';
          al.log(
            isUpload ? 'UPLOAD' : 'DOWNLOAD',
            `AeroCloud ${isUpload ? '↑' : '↓'} ${file.path}`,
            'success',
            formatBytes(file.size)
          );
        }

        if (syncedFiles.length > MAX_FILE_RECORDS) {
          al.log('INFO', `AeroCloud: +${syncedFiles.length - MAX_FILE_RECORDS} file records omitted`, 'success');
        }
      }
    });

    return () => {
      unlistenStatus.then(fn => fn());
      unlistenMenu.then(fn => fn());
      unlistenSyncComplete.then(fn => fn());
    };
  }, []);

  // Auto-start background sync only after vault is unlocked
  useEffect(() => {
    if (isAppLocked || !isCloudActive) return;

    const startSync = async () => {
      logger.debug('Vault unlocked and cloud enabled, starting background sync...');
      try {
        await invoke('start_background_sync');
        logger.debug('Background sync started');
      } catch (syncError) {
        logger.debug('Background sync start error (may already be running):', syncError);
      }
    };
    startSync();
  }, [isAppLocked, isCloudActive]);

  return {
    showCloudPanel,
    setShowCloudPanel,
    cloudSyncing,
    isCloudActive,
    setIsCloudActive,
    cloudServerName,
    setCloudServerName,
    cloudLastSync,
    setCloudLastSync,
    cloudLocalFolder,
    setCloudLocalFolder,
    cloudRemoteFolder,
    setCloudRemoteFolder,
    cloudPublicUrlBase,
    setCloudPublicUrlBase,
  };
}
