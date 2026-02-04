/**
 * useOverwriteCheck Hook
 * Extracted from App.tsx during modularization (v1.3.1)
 *
 * Manages file overwrite detection during batch transfers.
 * Uses useRef alongside useState to avoid stale closure issues in async for loops
 * (React state is stale inside async callbacks; refs provide synchronous reads).
 *
 * Checks localStorage 'aeroftp_settings' for fileExistsAction preference
 * (ask/overwrite/skip/rename/resume) before showing the dialog.
 *
 * Props: localFiles, remoteFiles (to check if destination file exists)
 * Returns: overwriteDialog, setOverwriteDialog, checkOverwrite, resetOverwriteSettings
 */

import { useState, useRef, useCallback } from 'react';
import { LocalFile, RemoteFile } from '../types';
import { OverwriteAction, FileCompareInfo } from '../components/OverwriteDialog';

interface OverwriteDialogState {
  isOpen: boolean;
  source: FileCompareInfo | null;
  destination: FileCompareInfo | null;
  queueCount: number;
  resolve: ((result: { action: OverwriteAction; applyToAll: boolean; newName?: string }) => void) | null;
}

interface UseOverwriteCheckProps {
  localFiles: LocalFile[];
  remoteFiles: RemoteFile[];
}

export const useOverwriteCheck = ({ localFiles, remoteFiles }: UseOverwriteCheckProps) => {
  const [overwriteDialog, setOverwriteDialog] = useState<OverwriteDialogState>({
    isOpen: false, source: null, destination: null, queueCount: 0, resolve: null,
  });
  const [overwriteApplyToAll, setOverwriteApplyToAll] = useState<{ action: OverwriteAction; enabled: boolean }>({
    action: 'overwrite', enabled: false,
  });
  const overwriteApplyToAllRef = useRef<{ action: OverwriteAction; enabled: boolean }>({
    action: 'overwrite', enabled: false,
  });

  /**
   * Check if a file transfer would overwrite an existing file.
   * Returns the action to take: overwrite, skip, rename, or cancel.
   * Respects fileExistsAction setting from Settings panel.
   */
  const checkOverwrite = useCallback(async (
    sourceName: string,
    sourceSize: number,
    sourceModified: Date | undefined,
    sourceIsRemote: boolean,
    queueCount: number = 0
  ): Promise<{ action: OverwriteAction; newName?: string }> => {
    // Read from ref to get the latest value within async loops (state is stale in closures)
    if (overwriteApplyToAllRef.current.enabled) {
      return { action: overwriteApplyToAllRef.current.action };
    }

    // Check if destination file exists
    let destFile: LocalFile | RemoteFile | undefined;
    if (sourceIsRemote) {
      destFile = localFiles.find(f => f.name === sourceName && !f.is_dir);
    } else {
      destFile = remoteFiles.find(f => f.name === sourceName && !f.is_dir);
    }

    if (!destFile) {
      return { action: 'overwrite' };
    }

    // Check settings for configured behavior
    try {
      const savedSettings = localStorage.getItem('aeroftp_settings');
      if (savedSettings) {
        const settings = JSON.parse(savedSettings);
        const fileExistsAction = settings.fileExistsAction as 'ask' | 'overwrite' | 'skip' | 'rename' | 'resume';

        if (fileExistsAction && fileExistsAction !== 'ask') {
          if (fileExistsAction === 'overwrite' || fileExistsAction === 'resume') {
            return { action: 'overwrite' };
          }
          if (fileExistsAction === 'skip') {
            return { action: 'skip' };
          }
          if (fileExistsAction === 'rename') {
            const ext = sourceName.includes('.') ? '.' + sourceName.split('.').pop() : '';
            const baseName = ext ? sourceName.slice(0, -ext.length) : sourceName;
            let counter = 1;
            let newName = `${baseName} (${counter})${ext}`;
            const existingNames = sourceIsRemote
              ? localFiles.map(f => f.name)
              : remoteFiles.map(f => f.name);
            while (existingNames.includes(newName)) {
              counter++;
              newName = `${baseName} (${counter})${ext}`;
            }
            return { action: 'rename', newName };
          }

          // === SMART SYNC OPTIONS ===
          // Compare timestamps and sizes for intelligent conflict resolution
          const destDate = destFile.modified ? new Date(destFile.modified).getTime() : 0;
          const sourceDate = sourceModified?.getTime() || 0;
          const TOLERANCE_MS = 1000; // 1 second tolerance for timestamp comparison

          if (fileExistsAction === 'overwrite_if_newer') {
            // Overwrite only if source file is more recent (with tolerance)
            if (sourceDate > destDate + TOLERANCE_MS) {
              return { action: 'overwrite' };
            }
            return { action: 'skip' };
          }

          if (fileExistsAction === 'overwrite_if_different') {
            // Overwrite if either date OR size differs
            const dateDiffers = Math.abs(sourceDate - destDate) > TOLERANCE_MS;
            const sizeDiffers = sourceSize !== (destFile.size || 0);
            if (dateDiffers || sizeDiffers) {
              return { action: 'overwrite' };
            }
            return { action: 'skip' };
          }

          if (fileExistsAction === 'skip_if_identical') {
            // Skip only if BOTH date and size are the same (within tolerance)
            const dateSame = Math.abs(sourceDate - destDate) <= TOLERANCE_MS;
            const sizeSame = sourceSize === (destFile.size || 0);
            if (dateSame && sizeSame) {
              return { action: 'skip' };
            }
            return { action: 'overwrite' };
          }
        }
      }
    } catch (e) {
      console.warn('[checkOverwrite] Could not read settings:', e);
    }

    // Show dialog and wait for user decision
    return new Promise((resolve) => {
      setOverwriteDialog({
        isOpen: true,
        source: {
          name: sourceName,
          size: sourceSize,
          modified: sourceModified,
          isRemote: sourceIsRemote,
        },
        destination: {
          name: destFile!.name,
          size: destFile!.size || 0,
          modified: destFile!.modified ? new Date(destFile!.modified) : undefined,
          isRemote: !sourceIsRemote,
        },
        queueCount,
        resolve: (result) => {
          if (result.applyToAll) {
            const applyToAllValue = { action: result.action, enabled: true };
            setOverwriteApplyToAll(applyToAllValue);
            overwriteApplyToAllRef.current = applyToAllValue;
          }
          resolve({ action: result.action, newName: result.newName });
        },
      });
    });
  }, [localFiles, remoteFiles]);

  const resetOverwriteSettings = useCallback(() => {
    const resetValue = { action: 'overwrite' as OverwriteAction, enabled: false };
    setOverwriteApplyToAll(resetValue);
    overwriteApplyToAllRef.current = resetValue;
  }, []);

  return {
    overwriteDialog,
    setOverwriteDialog,
    overwriteApplyToAll,
    checkOverwrite,
    resetOverwriteSettings,
  };
};

export default useOverwriteCheck;
