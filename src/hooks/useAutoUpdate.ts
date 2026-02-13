/**
 * useAutoUpdate Hook
 * Extracted from App.tsx during modularization (v1.3.1)
 *
 * Checks for app updates on startup (5s delay) and provides manual check.
 * Uses invoke('check_update') backend command and sends OS notifications.
 * Prevents duplicate checks via updateCheckedRef.
 *
 * When a newer version exists but the asset for the installed format (e.g. .deb)
 * is not yet available (CI still building), retries every hour instead of 24h.
 *
 * Props: activityLog (for logging update check results)
 * Returns: updateAvailable (UpdateInfo | null), setUpdateAvailable, checkForUpdate
 */

import { useState, useCallback, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { sendNotification } from '@tauri-apps/plugin-notification';
import type { OperationType, OperationStatus } from './useActivityLog';

export interface UpdateInfo {
  has_update: boolean;
  latest_version?: string;
  download_url?: string;
  current_version: string;
  install_format: string;
}

interface UseAutoUpdateProps {
  activityLog: {
    log: (operation: OperationType, message: string, status?: OperationStatus, details?: string) => string;
  };
}

const ONE_HOUR = 60 * 60 * 1000;
const TWENTY_FOUR_HOURS = 24 * ONE_HOUR;

export const useAutoUpdate = ({ activityLog }: UseAutoUpdateProps) => {
  const [updateAvailable, setUpdateAvailable] = useState<UpdateInfo | null>(null);
  const updateCheckedRef = useRef(false);
  const pendingRetryRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const checkForUpdate = useCallback(async (manual = false) => {
    try {
      const info: UpdateInfo = await invoke('check_update');
      setUpdateAvailable(info);

      if (info.has_update) {
        sendNotification({
          title: 'AeroFTP Update Available!',
          body: `Version ${info.latest_version} is ready.`,
        });
        const checkType = manual ? '[Manual]' : '[Auto]';
        activityLog.log('INFO', `${checkType} Update v${info.latest_version} available! (current: v${info.current_version}, format: ${info.install_format?.toUpperCase() || 'DEB'})`, 'success');
        await invoke('log_update_detection', { version: info.latest_version || '' });

        // Asset found — clear any pending retry
        if (pendingRetryRef.current) {
          clearTimeout(pendingRetryRef.current);
          pendingRetryRef.current = null;
        }
      } else {
        // Newer version exists but asset not yet available — retry in 1 hour
        const assetPending = info.latest_version && info.latest_version !== info.current_version;
        if (assetPending) {
          activityLog.log('INFO', `[Auto] v${info.latest_version} released but .${info.install_format || 'deb'} not yet available, retrying in 1h`, 'pending');
          if (pendingRetryRef.current) clearTimeout(pendingRetryRef.current);
          pendingRetryRef.current = setTimeout(() => {
            pendingRetryRef.current = null;
            checkForUpdate(false);
          }, ONE_HOUR);
        } else if (manual) {
          sendNotification({ title: 'No Update Available', body: `You're running the latest version (${info.current_version})` });
          activityLog.log('INFO', `[Manual] Up to date: v${info.current_version} (${info.install_format?.toUpperCase() || 'DEB'})`, 'success');
        }
      }
    } catch (error) {
      console.error('Update check failed:', error);
      if (manual) {
        activityLog.log('ERROR', `Update check failed: ${error}`, 'error');
      }
    }
  }, [activityLog]);

  // Check on startup (5s delay, once) + periodic every 24h
  useEffect(() => {
    if (!updateCheckedRef.current) {
      updateCheckedRef.current = true;
      const timer = setTimeout(() => {
        checkForUpdate(false);
      }, 5000);

      const interval = setInterval(() => {
        checkForUpdate(false);
      }, TWENTY_FOUR_HOURS);

      return () => {
        clearTimeout(timer);
        clearInterval(interval);
        if (pendingRetryRef.current) clearTimeout(pendingRetryRef.current);
      };
    }
  }, []);

  return {
    updateAvailable,
    setUpdateAvailable,
    checkForUpdate,
  };
};

export default useAutoUpdate;
