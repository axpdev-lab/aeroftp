/**
 * useAutoUpdate Hook
 * Extracted from App.tsx during modularization (v1.3.1)
 *
 * Checks for app updates on startup (5s delay) and provides manual check.
 * Uses invoke('check_for_updates') backend command and sends OS notifications.
 * Prevents duplicate checks via updateCheckedRef.
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

export const useAutoUpdate = ({ activityLog }: UseAutoUpdateProps) => {
  const [updateAvailable, setUpdateAvailable] = useState<UpdateInfo | null>(null);
  const updateCheckedRef = useRef(false);

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
      } else if (manual) {
        sendNotification({ title: 'No Update Available', body: `You're running the latest version (${info.current_version})` });
        activityLog.log('INFO', `[Manual] Up to date: v${info.current_version} (${info.install_format?.toUpperCase() || 'DEB'})`, 'success');
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

      // Periodic check every 24 hours
      const interval = setInterval(() => {
        checkForUpdate(false);
      }, 24 * 60 * 60 * 1000);

      return () => {
        clearTimeout(timer);
        clearInterval(interval);
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
