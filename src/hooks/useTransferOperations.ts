/**
 * useTransferOperations Hook
 * Handles file upload/download operations with provider support and overwrite checking
 * Supports both FTP and Provider protocols (S3, WebDAV, MEGA, SFTP, OAuth)
 */

import { useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { downloadDir } from '@tauri-apps/api/path';
import { DownloadParams, UploadParams, RemoteFile, LocalFile } from '../types';
import { OverwriteAction, FileCompareInfo } from '../components/OverwriteDialog';
import { useHumanizedLog } from './useHumanizedLog';
import { useTranslation } from '../i18n';
import { formatBytes } from '../utils';

interface DownloadFolderParams {
    remote_path: string;
    local_path: string;
}

interface UploadFolderParams {
    local_path: string;
    remote_path: string;
}

// List of provider protocols (non-FTP)
const PROVIDER_PROTOCOLS = ['googledrive', 'dropbox', 'onedrive', 's3', 'webdav', 'mega', 'sftp'];

interface UseTransferOperationsParams {
    notify: {
        success: (title: string, message?: string) => string | null;
        error: (title: string, message?: string) => string;
        info: (title: string, message?: string) => string | null;
        warning: (title: string, message?: string) => string | null;
    };
    currentRemotePath: string;
    currentLocalPath: string;
    remoteFiles: RemoteFile[];
    localFiles: LocalFile[];
    selectedRemoteFiles: Set<string>;
    selectedLocalFiles: Set<string>;
    setSelectedRemoteFiles: (files: Set<string>) => void;
    setSelectedLocalFiles: (files: Set<string>) => void;
    loadRemoteFiles: () => Promise<void>;
    loadLocalFiles: (path: string) => Promise<void>;
    // Provider detection
    protocol?: string;
    activeSessionId?: string | null;
    sessions?: Array<{ id: string; connectionParams?: { protocol?: string } }>;
    connectionParams?: { protocol?: string };
    isConnected?: boolean;
    // Transfer queue
    transferQueue?: {
        addItem: (filename: string, path: string, size: number, type: 'upload' | 'download') => string;
        startTransfer: (id: string) => void;
        completeTransfer: (id: string) => void;
        failTransfer: (id: string, error: string) => void;
        markAsFolder: (id: string) => void;
        items: Array<{ id: string; filename: string; status: string }>;
    };
    // Overwrite dialog
    setOverwriteDialog?: (dialog: {
        isOpen: boolean;
        source: FileCompareInfo | null;
        destination: FileCompareInfo | null;
        queueCount: number;
        resolve: ((result: { action: OverwriteAction; applyToAll: boolean; newName?: string }) => void) | null;
    }) => void;
    overwriteApplyToAll?: { action: OverwriteAction; enabled: boolean };
    setOverwriteApplyToAll?: (value: { action: OverwriteAction; enabled: boolean }) => void;
}

export function useTransferOperations({
    notify,
    currentRemotePath,
    currentLocalPath,
    remoteFiles,
    localFiles,
    selectedRemoteFiles,
    selectedLocalFiles,
    setSelectedRemoteFiles,
    setSelectedLocalFiles,
    loadRemoteFiles,
    loadLocalFiles,
    protocol,
    activeSessionId,
    sessions,
    connectionParams,
    isConnected = false,
    transferQueue,
    setOverwriteDialog,
    overwriteApplyToAll,
    setOverwriteApplyToAll,
}: UseTransferOperationsParams) {

    const humanLog = useHumanizedLog();
    const t = useTranslation();

    // Maps filenames to manually started log IDs (to merge frontend and backend logs)
    const pendingFileLogIds = useRef<Map<string, string>>(new Map());

    // Helper: Get effective protocol from various sources
    const getEffectiveProtocol = useCallback(() => {
        if (protocol) return protocol;
        if (connectionParams?.protocol) return connectionParams.protocol;
        if (sessions && activeSessionId) {
            const activeSession = sessions.find(s => s.id === activeSessionId);
            return activeSession?.connectionParams?.protocol;
        }
        return undefined;
    }, [protocol, connectionParams, sessions, activeSessionId]);

    // Helper: Check if current connection is a Provider (non-FTP)
    const isProvider = useCallback(() => {
        const effectiveProtocol = getEffectiveProtocol();
        return effectiveProtocol && PROVIDER_PROTOCOLS.includes(effectiveProtocol);
    }, [getEffectiveProtocol]);

    // Reset apply-to-all when transfer batch is complete
    const resetOverwriteSettings = useCallback(() => {
        if (setOverwriteApplyToAll) {
            setOverwriteApplyToAll({ action: 'overwrite', enabled: false });
        }
    }, [setOverwriteApplyToAll]);

    /**
     * Check if a file exists and prompt for overwrite decision
     */
    const checkOverwrite = useCallback(async (
        sourceName: string,
        sourceSize: number,
        sourceModified: Date | undefined,
        sourceIsRemote: boolean,
        queueCount: number = 0
    ): Promise<{ action: OverwriteAction; newName?: string }> => {
        // If "apply to all" is already set (for current batch), use that decision
        if (overwriteApplyToAll?.enabled) {
            return { action: overwriteApplyToAll.action };
        }

        // Check if destination file exists in the appropriate file list
        let destFile: LocalFile | RemoteFile | undefined;

        if (sourceIsRemote) {
            // Download scenario: check if local file exists
            destFile = localFiles.find(f => f.name === sourceName && !f.is_dir);
        } else {
            // Upload scenario: check if remote file exists
            destFile = remoteFiles.find(f => f.name === sourceName && !f.is_dir);
        }

        // If file doesn't exist, proceed with transfer
        if (!destFile) {
            return { action: 'overwrite' };
        }

        // File exists - check settings for configured behavior
        try {
            const savedSettings = localStorage.getItem('aeroftp_settings');
            if (savedSettings) {
                const settings = JSON.parse(savedSettings);
                const fileExistsAction = settings.fileExistsAction as 'ask' | 'overwrite' | 'skip' | 'rename' | 'resume';

                // If user has configured a default action (not 'ask'), apply it automatically
                if (fileExistsAction && fileExistsAction !== 'ask') {
                    if (fileExistsAction === 'overwrite' || fileExistsAction === 'resume') {
                        return { action: 'overwrite' };
                    }
                    if (fileExistsAction === 'skip') {
                        return { action: 'skip' };
                    }
                    if (fileExistsAction === 'rename') {
                        // Auto-generate unique name: file.txt → file (1).txt
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
                }
            }
        } catch (e) {
            console.warn('[checkOverwrite] Could not read settings:', e);
        }

        // Setting is 'ask' or not set - show dialog and wait for user decision
        if (!setOverwriteDialog) {
            return { action: 'overwrite' };
        }

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
                    if (result.applyToAll && setOverwriteApplyToAll) {
                        setOverwriteApplyToAll({ action: result.action, enabled: true });
                    }
                    resolve({ action: result.action, newName: result.newName });
                },
            });
        });
    }, [localFiles, remoteFiles, overwriteApplyToAll, setOverwriteDialog, setOverwriteApplyToAll]);

    // ===================================
    // DOWNLOAD Operations
    // ===================================

    const downloadFile = useCallback(async (
        remoteFilePath: string,
        fileName: string,
        destinationPath?: string,
        isDir: boolean = false,
        fileSize?: number
    ) => {
        const logId = humanLog.logStart('DOWNLOAD', { filename: fileName });
        pendingFileLogIds.current.set(fileName, logId);
        const startTime = Date.now();
        const useProvider = isProvider();

        try {
            if (isDir) {
                const downloadPath = destinationPath || await open({ directory: true, multiple: false, defaultPath: await downloadDir() });
                if (downloadPath) {
                    const folderPath = `${downloadPath}/${fileName}`;
                    if (useProvider) {
                        await invoke('provider_download_folder', { remotePath: remoteFilePath, localPath: folderPath });
                    } else {
                        const params: DownloadFolderParams = { remote_path: remoteFilePath, local_path: folderPath };
                        await invoke('download_folder', { params });
                    }
                    const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
                    humanLog.updateEntry(logId, { status: 'success', message: `Downloaded folder ${fileName} in ${elapsed}s` });
                } else {
                    humanLog.logError('DOWNLOAD', { filename: fileName }, logId);
                }
            } else {
                const downloadPath = destinationPath || await open({ directory: true, multiple: false, defaultPath: await downloadDir() });
                if (downloadPath) {
                    const localFilePath = `${downloadPath}/${fileName}`;
                    if (useProvider) {
                        await invoke('provider_download_file', { remotePath: remoteFilePath, localPath: localFilePath });
                    } else {
                        const params: DownloadParams = { remote_path: remoteFilePath, local_path: localFilePath };
                        await invoke('download_file', { params });
                    }
                    const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
                    const sizeStr = fileSize ? formatBytes(fileSize) : '';
                    const details = sizeStr ? `(${sizeStr} in ${elapsed}s)` : `(${elapsed}s)`;
                    const msg = t('activity.download_success', { filename: fileName, details });
                    humanLog.updateEntry(logId, { status: 'success', message: msg });
                } else {
                    humanLog.logError('DOWNLOAD', { filename: fileName }, logId);
                }
            }
        } catch (error) {
            humanLog.logError('DOWNLOAD', { filename: fileName }, logId);
            notify.error('Download Failed', String(error));
        }
    }, [isProvider, humanLog, t, notify]);

    const downloadMultipleFiles = useCallback(async (filesOverride?: string[]) => {
        if (!isConnected) return;
        const names = filesOverride || Array.from(selectedRemoteFiles);
        if (names.length === 0) return;

        resetOverwriteSettings();

        const filesToDownload = names.map(n => remoteFiles.find(f => f.name === n)).filter(Boolean) as RemoteFile[];
        if (filesToDownload.length === 0) return;

        // Add all files to queue first
        const queueItems = filesToDownload.map(file => ({
            id: transferQueue?.addItem(file.name, file.path, file.size || 0, 'download') || '',
            file
        }));

        // Download sequentially with queue tracking and overwrite checking
        let skippedCount = 0;
        for (let i = 0; i < queueItems.length; i++) {
            const item = queueItems[i];
            const remainingInQueue = queueItems.length - i - 1;

            // Check for overwrite (only for files, not directories)
            if (!item.file.is_dir) {
                const overwriteResult = await checkOverwrite(
                    item.file.name,
                    item.file.size || 0,
                    item.file.modified ? new Date(item.file.modified) : undefined,
                    true, // sourceIsRemote
                    remainingInQueue
                );

                if (overwriteResult.action === 'cancel') {
                    // Cancel entire batch
                    if (transferQueue) {
                        transferQueue.failTransfer(item.id, 'Cancelled by user');
                        for (let j = i + 1; j < queueItems.length; j++) {
                            transferQueue.failTransfer(queueItems[j].id, 'Cancelled by user');
                        }
                    }
                    break;
                }

                if (overwriteResult.action === 'skip') {
                    if (transferQueue) {
                        transferQueue.completeTransfer(item.id);
                    }
                    humanLog.logRaw('activity.download_skipped', 'DOWNLOAD', { filename: item.file.name }, 'success');
                    skippedCount++;
                    continue;
                }
            }

            if (transferQueue) {
                transferQueue.startTransfer(item.id);
            }

            try {
                await downloadFile(item.file.path, item.file.name, currentLocalPath, item.file.is_dir, item.file.size || undefined);
                if (transferQueue) {
                    transferQueue.completeTransfer(item.id);
                }
            } catch (error) {
                if (transferQueue) {
                    transferQueue.failTransfer(item.id, String(error));
                }
            }
        }

        resetOverwriteSettings();

        if (skippedCount > 0) {
            notify.info(`${skippedCount} file(s) skipped`);
        }
        setSelectedRemoteFiles(new Set());
        await loadLocalFiles(currentLocalPath);
    }, [isConnected, selectedRemoteFiles, remoteFiles, currentLocalPath, transferQueue, checkOverwrite,
        downloadFile, humanLog, notify, loadLocalFiles, setSelectedRemoteFiles, resetOverwriteSettings]);

    // ===================================
    // UPLOAD Operations
    // ===================================

    const uploadFile = useCallback(async (
        localFilePath: string,
        fileName: string,
        isDir: boolean = false,
        fileSize?: number
    ) => {
        const logId = humanLog.logStart('UPLOAD', { filename: fileName });
        pendingFileLogIds.current.set(fileName, logId);
        const startTime = Date.now();
        const useProvider = isProvider();

        try {
            if (isDir) {
                if (useProvider) {
                    const remoteRootForFolder = `${currentRemotePath}${currentRemotePath.endsWith('/') ? '' : '/'}${fileName}`;

                    // Recursive upload function
                    const processFolder = async (currentLocalPath: string, currentRemoteBase: string) => {
                        // Create the directory itself first
                        try {
                            await invoke('provider_mkdir', { path: currentRemoteBase });
                        } catch {
                            // Ignore if already exists
                        }

                        const entries = await invoke<LocalFile[]>('get_local_files', { path: currentLocalPath, showHidden: true });
                        for (const entry of entries) {
                            const newRemotePath = `${currentRemoteBase}/${entry.name}`;
                            if (entry.is_dir) {
                                await processFolder(entry.path, newRemotePath);
                            } else {
                                try {
                                    humanLog.updateEntry(logId, { message: `Uploading ${entry.name}...` });
                                    await invoke('provider_upload_file', { localPath: entry.path, remotePath: newRemotePath });
                                } catch (e) {
                                    console.error(`Failed to upload ${entry.name}:`, e);
                                }
                            }
                        }
                    };

                    await processFolder(localFilePath, remoteRootForFolder);

                    const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
                    const details = `(${elapsed}s)`;
                    const msg = t('activity.upload_success', { filename: fileName, details });
                    humanLog.updateEntry(logId, { message: msg });
                    loadRemoteFiles();
                    return;
                }

                const remotePath = `${currentRemotePath}${currentRemotePath.endsWith('/') ? '' : '/'}${fileName}`;
                const params: UploadFolderParams = { local_path: localFilePath, remote_path: remotePath };
                await invoke('upload_folder', { params });
                const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
                const details = `(${elapsed}s)`;
                const msg = t('activity.upload_success', { filename: fileName, details });
                humanLog.updateEntry(logId, { status: 'success', message: msg });
            } else {
                const remotePath = `${currentRemotePath}${currentRemotePath.endsWith('/') ? '' : '/'}${fileName}`;

                if (useProvider) {
                    await invoke('provider_upload_file', { localPath: localFilePath, remotePath });
                } else {
                    await invoke('upload_file', { params: { local_path: localFilePath, remote_path: remotePath } as UploadParams });
                }

                const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
                const sizeStr = fileSize ? formatBytes(fileSize) : '';
                const details = sizeStr ? `(${sizeStr} in ${elapsed}s)` : `(${elapsed}s)`;
                const msg = t('activity.upload_success', { filename: fileName, details });
                humanLog.updateEntry(logId, { status: 'success', message: msg });
            }
        } catch (error) {
            humanLog.logError('UPLOAD', { filename: fileName }, logId);
            notify.error('Upload Failed', String(error));
        }
    }, [isProvider, currentRemotePath, humanLog, t, notify, loadRemoteFiles]);

    const uploadMultipleFiles = useCallback(async (filesOverride?: string[]) => {
        if (!isConnected) return;

        resetOverwriteSettings();

        const targetNames = filesOverride || Array.from(selectedLocalFiles);

        // Priority 1: Upload specific target files
        if (targetNames.length > 0) {
            const filesToUpload = targetNames.map(name => {
                const file = localFiles.find(f => f.name === name);
                return file ? { path: file.path, file } : null;
            }).filter(Boolean) as { path: string; file: LocalFile }[];

            if (filesToUpload.length > 0) {
                // Add all files to queue first
                const queueItems = filesToUpload.map(({ path: filePath, file }) => {
                    const fileName = filePath.split(/[/\\]/).pop() || filePath;
                    const size = file?.size || 0;
                    return {
                        id: transferQueue?.addItem(fileName, filePath, size, 'upload') || '',
                        filePath,
                        fileName,
                        file
                    };
                });

                // Upload sequentially with queue tracking and overwrite checking
                let skippedCount = 0;
                for (let i = 0; i < queueItems.length; i++) {
                    const item = queueItems[i];
                    const remainingInQueue = queueItems.length - i - 1;

                    // Check for overwrite (only for files, not directories)
                    if (!item.file.is_dir) {
                        const overwriteResult = await checkOverwrite(
                            item.fileName,
                            item.file.size || 0,
                            item.file.modified ? new Date(item.file.modified) : undefined,
                            false, // sourceIsRemote = false for upload
                            remainingInQueue
                        );

                        if (overwriteResult.action === 'cancel') {
                            // Cancel entire batch
                            if (transferQueue) {
                                transferQueue.failTransfer(item.id, 'Cancelled by user');
                                for (let j = i + 1; j < queueItems.length; j++) {
                                    transferQueue.failTransfer(queueItems[j].id, 'Cancelled by user');
                                }
                            }
                            break;
                        }

                        if (overwriteResult.action === 'skip') {
                            if (transferQueue) {
                                transferQueue.completeTransfer(item.id);
                            }
                            humanLog.logRaw('activity.upload_skipped', 'UPLOAD', { filename: item.fileName }, 'success');
                            skippedCount++;
                            continue;
                        }
                    }

                    if (transferQueue) {
                        transferQueue.startTransfer(item.id);
                    }

                    try {
                        await uploadFile(item.filePath, item.fileName, item.file?.is_dir || false, item.file?.size || undefined);
                        if (transferQueue) {
                            transferQueue.completeTransfer(item.id);
                        }
                    } catch (error) {
                        if (transferQueue) {
                            transferQueue.failTransfer(item.id, String(error));
                        }
                    }
                }

                resetOverwriteSettings();

                if (skippedCount > 0) {
                    notify.info(`${skippedCount} file(s) skipped`);
                }
                setSelectedLocalFiles(new Set());
                loadRemoteFiles();
                return;
            }
        }

        // Priority 2: Open Dialog if no selection
        const selected = await open({
            multiple: true,
            directory: false,
            title: 'Select Files to Upload',
        });

        if (!selected) return;
        const files = Array.isArray(selected) ? selected : [selected];

        if (files.length > 0) {
            resetOverwriteSettings();
            let skippedCount = 0;

            for (let i = 0; i < files.length; i++) {
                const filePath = files[i];
                const fileName = filePath.replace(/^.*[\\\/]/, '');
                const remainingInQueue = files.length - i - 1;

                // Check for overwrite
                const overwriteResult = await checkOverwrite(
                    fileName,
                    0, // Size unknown from dialog
                    undefined, // Modified unknown from dialog
                    false, // sourceIsRemote = false for upload
                    remainingInQueue
                );

                if (overwriteResult.action === 'cancel') {
                    break;
                }

                if (overwriteResult.action === 'skip') {
                    humanLog.logRaw('activity.upload_skipped', 'UPLOAD', { filename: fileName }, 'success');
                    skippedCount++;
                    continue;
                }

                await uploadFile(filePath, fileName, false);
            }

            resetOverwriteSettings();
            if (skippedCount > 0) {
                notify.info(`${skippedCount} file(s) skipped`);
            }
        }
    }, [isConnected, selectedLocalFiles, localFiles, transferQueue, checkOverwrite,
        uploadFile, humanLog, notify, loadRemoteFiles, setSelectedLocalFiles, resetOverwriteSettings]);

    // ===================================
    // CANCEL Transfer
    // ===================================

    const cancelTransfer = useCallback(async () => {
        try {
            await invoke('cancel_transfer');
        } catch { }
    }, []);

    return {
        // Single file operations
        downloadFile,
        uploadFile,
        cancelTransfer,
        // Multiple file operations
        downloadMultipleFiles,
        uploadMultipleFiles,
        // Overwrite checking
        checkOverwrite,
        resetOverwriteSettings,
        // Helpers
        pendingFileLogIds,
        isProvider,
        getEffectiveProtocol,
    };
}

export default useTransferOperations;
