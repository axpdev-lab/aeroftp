/**
 * useFileOperations Hook
 * Handles file loading, navigation, and mutation operations (delete, rename, create)
 * Supports both FTP and Provider protocols (S3, WebDAV, MEGA, SFTP, OAuth)
 */

import { useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { RemoteFile, LocalFile, FileListResponse } from '../types';
import { useActivityLog } from './useActivityLog';
import { useHumanizedLog } from './useHumanizedLog';
import { useTranslation } from '../i18n';

// List of provider protocols (non-FTP)
const PROVIDER_PROTOCOLS = ['googledrive', 'dropbox', 'onedrive', 's3', 'webdav', 'mega', 'sftp'];

interface UseFileOperationsParams {
    toast: {
        success: (title: string, message?: string) => void;
        error: (title: string, message?: string) => void;
        info: (title: string, message?: string) => void;
        warning: (title: string, message?: string) => void;
    };
    notify: {
        success: (title: string, message?: string) => string | null;
        error: (title: string, message?: string) => string;
        info: (title: string, message?: string) => string | null;
        warning: (title: string, message?: string) => string | null;
    };
    setConfirmDialog: (dialog: { message: string; onConfirm: () => void } | null) => void;
    setInputDialog: (dialog: { title: string; defaultValue: string; onConfirm: (v: string) => void } | null) => void;
    setRemoteFiles: (files: RemoteFile[]) => void;
    setLocalFiles: (files: LocalFile[]) => void;
    setCurrentRemotePath: (path: string) => void;
    setCurrentLocalPath: (path: string) => void;
    setSyncNavDialog: (dialog: { missingPath: string; isRemote: boolean; targetPath: string } | null) => void;
    currentRemotePath: string;
    currentLocalPath: string;
    remoteFiles: RemoteFile[];
    localFiles: LocalFile[];
    selectedRemoteFiles: Set<string>;
    selectedLocalFiles: Set<string>;
    setSelectedRemoteFiles: (files: Set<string>) => void;
    setSelectedLocalFiles: (files: Set<string>) => void;
    isSyncNavigation?: boolean;
    syncBasePaths?: { remote: string; local: string } | null;
    showHiddenFiles?: boolean;
    isConnected?: boolean;
    protocol?: string;
    confirmBeforeDelete?: boolean;
    // Session info for provider detection
    activeSessionId?: string | null;
    sessions?: Array<{ id: string; connectionParams?: { protocol?: string } }>;
    connectionParams?: { protocol?: string };
}

export function useFileOperations({
    toast,
    notify,
    setConfirmDialog,
    setInputDialog,
    setRemoteFiles,
    setLocalFiles,
    setCurrentRemotePath,
    setCurrentLocalPath,
    setSyncNavDialog,
    currentRemotePath,
    currentLocalPath,
    remoteFiles,
    localFiles,
    selectedRemoteFiles,
    selectedLocalFiles,
    setSelectedRemoteFiles,
    setSelectedLocalFiles,
    isSyncNavigation,
    syncBasePaths,
    showHiddenFiles = true,
    isConnected = false,
    protocol,
    confirmBeforeDelete = true,
    activeSessionId,
    sessions,
    connectionParams,
}: UseFileOperationsParams) {

    const activityLog = useActivityLog();
    const humanLog = useHumanizedLog();
    const t = useTranslation();

    // Helper: Get effective protocol from various sources
    const getEffectiveProtocol = useCallback(() => {
        // Priority: explicit protocol > connectionParams > active session
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

    // ===================================
    // LOADING FILES
    // ===================================

    const loadRemoteFiles = useCallback(async (overrideProtocol?: string) => {
        try {
            const effectiveProtocol = overrideProtocol || getEffectiveProtocol();
            const useProvider = effectiveProtocol && PROVIDER_PROTOCOLS.includes(effectiveProtocol);

            let response: FileListResponse;

            if (useProvider) {
                response = await invoke('provider_list_files', { path: null });
                if (response.files?.length > 0) {
                    humanLog.logRaw('activity.loaded_items', 'INFO', { count: response.files.length, provider: effectiveProtocol }, 'success');
                }
            } else {
                response = await invoke('list_files');
            }

            setRemoteFiles(response.files);
            setCurrentRemotePath(response.current_path);
        } catch (error) {
            console.error('[loadRemoteFiles] Error:', error);
            activityLog.log('ERROR', `Failed to list files: ${error}`, 'error');
            notify.error('Error', `Failed to list files: ${error}`);
        }
    }, [getEffectiveProtocol, setRemoteFiles, setCurrentRemotePath, activityLog, humanLog, notify]);

    const loadLocalFiles = useCallback(async (path: string) => {
        try {
            const files: LocalFile[] = await invoke('get_local_files', { path, showHidden: showHiddenFiles });
            setLocalFiles(files);
            setCurrentLocalPath(path);
        } catch (error) {
            notify.error('Error', `Failed to list local files: ${error}`);
        }
    }, [setLocalFiles, setCurrentLocalPath, notify, showHiddenFiles]);

    // ===================================
    // NAVIGATION
    // ===================================

    const changeRemoteDirectory = useCallback(async (path: string, overrideProtocol?: string) => {
        try {
            const effectiveProtocol = overrideProtocol || getEffectiveProtocol();
            const useProvider = effectiveProtocol && PROVIDER_PROTOCOLS.includes(effectiveProtocol);

            let response: FileListResponse;

            if (useProvider) {
                response = await invoke('provider_change_dir', { path });
            } else {
                response = await invoke('change_directory', { path });
            }

            setRemoteFiles(response.files);
            setCurrentRemotePath(response.current_path);

            // Navigation Sync: mirror to local panel if enabled
            if (isSyncNavigation && syncBasePaths) {
                const relativePath = response.current_path.startsWith(syncBasePaths.remote)
                    ? response.current_path.slice(syncBasePaths.remote.length)
                    : '';
                const basePath = syncBasePaths.local.endsWith('/') ? syncBasePaths.local.slice(0, -1) : syncBasePaths.local;
                const relPath = relativePath.startsWith('/') ? relativePath : '/' + relativePath;
                const newLocalPath = relativePath ? basePath + relPath : basePath;

                try {
                    const files: LocalFile[] = await invoke('get_local_files', { path: newLocalPath, showHidden: showHiddenFiles });
                    setLocalFiles(files);
                    setCurrentLocalPath(newLocalPath);
                } catch {
                    setSyncNavDialog({ missingPath: newLocalPath, isRemote: false, targetPath: newLocalPath });
                }
            }
        } catch (error) {
            activityLog.log('ERROR', `Failed to navigate: ${path}`, 'error');
            notify.error('Error', `Failed to change directory: ${error}`);
        }
    }, [getEffectiveProtocol, isSyncNavigation, syncBasePaths, showHiddenFiles, activityLog, notify,
        setRemoteFiles, setCurrentRemotePath, setLocalFiles, setCurrentLocalPath, setSyncNavDialog]);

    const changeLocalDirectory = useCallback(async (path: string) => {
        await loadLocalFiles(path);

        // Navigation Sync: mirror to remote panel if enabled
        if (isSyncNavigation && syncBasePaths && isConnected) {
            const relativePath = path.startsWith(syncBasePaths.local)
                ? path.slice(syncBasePaths.local.length)
                : '';
            const basePath = syncBasePaths.remote.endsWith('/') ? syncBasePaths.remote.slice(0, -1) : syncBasePaths.remote;
            const relPath = relativePath.startsWith('/') ? relativePath : '/' + relativePath;
            const newRemotePath = relativePath ? basePath + relPath : basePath;

            try {
                const effectiveProtocol = getEffectiveProtocol();
                const useProvider = effectiveProtocol && PROVIDER_PROTOCOLS.includes(effectiveProtocol);

                let response: FileListResponse;
                if (useProvider) {
                    response = await invoke('provider_change_dir', { path: newRemotePath });
                } else {
                    response = await invoke('change_directory', { path: newRemotePath });
                }

                setRemoteFiles(response.files);
                setCurrentRemotePath(response.current_path);
            } catch {
                setSyncNavDialog({ missingPath: newRemotePath, isRemote: true, targetPath: newRemotePath });
            }
        }
    }, [loadLocalFiles, getEffectiveProtocol, isSyncNavigation, syncBasePaths, isConnected,
        setRemoteFiles, setCurrentRemotePath, setSyncNavDialog]);

    // ===================================
    // DELETE Operations
    // ===================================

    const deleteRemoteFile = useCallback((path: string, isDir: boolean) => {
        const fileName = path.split('/').pop() || path;

        const performDelete = async () => {
            const logId = humanLog.logStart('DELETE', { filename: fileName });
            try {
                const useProvider = isProvider();

                if (useProvider) {
                    if (isDir) {
                        await invoke('provider_delete_dir', { path, recursive: true });
                    } else {
                        await invoke('provider_delete_file', { path });
                    }
                } else {
                    await invoke('delete_remote_file', { path, isDir });
                }

                humanLog.logSuccess('DELETE', { filename: fileName }, logId);
                notify.success('Deleted', fileName);
                await loadRemoteFiles();
            } catch (error) {
                humanLog.logError('DELETE', { filename: fileName }, logId);
                notify.error('Delete Failed', String(error));
            }
        };

        if (confirmBeforeDelete) {
            setConfirmDialog({
                message: `Delete "${fileName}"?`,
                onConfirm: async () => {
                    setConfirmDialog(null);
                    await performDelete();
                }
            });
        } else {
            performDelete();
        }
    }, [isProvider, confirmBeforeDelete, humanLog, notify, loadRemoteFiles, setConfirmDialog]);

    const deleteLocalFile = useCallback((path: string) => {
        const fileName = path.split('/').pop() || path;

        const performDelete = async () => {
            const logId = humanLog.logStart('DELETE', { filename: fileName });
            try {
                await invoke('delete_local_file', { path });
                humanLog.logSuccess('DELETE', { filename: fileName }, logId);
                notify.success('Deleted', fileName);
                await loadLocalFiles(currentLocalPath);
            } catch (error) {
                humanLog.logError('DELETE', { filename: fileName }, logId);
                notify.error('Delete Failed', String(error));
            }
        };

        if (confirmBeforeDelete) {
            setConfirmDialog({
                message: `Delete "${fileName}"?`,
                onConfirm: async () => {
                    setConfirmDialog(null);
                    await performDelete();
                }
            });
        } else {
            performDelete();
        }
    }, [confirmBeforeDelete, humanLog, notify, loadLocalFiles, currentLocalPath, setConfirmDialog]);

    const deleteMultipleRemoteFiles = useCallback((filesOverride?: string[]) => {
        const names = filesOverride || Array.from(selectedRemoteFiles);
        if (names.length === 0) return;

        const performDelete = async () => {
            const logId = humanLog.logStart('DELETE_MULTIPLE', { count: names.length });
            const deletedFiles: string[] = [];
            const deletedFolders: string[] = [];
            const useProvider = isProvider();

            for (const name of names) {
                const file = remoteFiles.find(f => f.name === name);
                if (file) {
                    try {
                        if (useProvider) {
                            if (file.is_dir) {
                                await invoke('provider_delete_dir', { path: file.path, recursive: true });
                            } else {
                                await invoke('provider_delete_file', { path: file.path });
                            }
                        } else {
                            await invoke('delete_remote_file', { path: file.path, isDir: file.is_dir });
                        }

                        if (file.is_dir) {
                            deletedFolders.push(name);
                        } else {
                            deletedFiles.push(name);
                        }
                    } catch { }
                }
            }

            await loadRemoteFiles();
            setSelectedRemoteFiles(new Set());

            const parts = [];
            if (deletedFolders.length > 0) parts.push(`${deletedFolders.length} folder${deletedFolders.length > 1 ? 's' : ''}`);
            if (deletedFiles.length > 0) parts.push(`${deletedFiles.length} file${deletedFiles.length > 1 ? 's' : ''}`);
            const count = deletedFolders.length + deletedFiles.length;
            const msg = t('activity.delete_multiple_success', { count });
            humanLog.updateEntry(logId, { status: 'success', message: msg });
            notify.success(parts.join(', '), `${parts.join(' and ')} deleted`);
        };

        if (confirmBeforeDelete) {
            setConfirmDialog({
                message: `Delete ${names.length} selected items?`,
                onConfirm: async () => {
                    setConfirmDialog(null);
                    await performDelete();
                }
            });
        } else {
            performDelete();
        }
    }, [selectedRemoteFiles, remoteFiles, isProvider, confirmBeforeDelete, humanLog, t, notify,
        loadRemoteFiles, setSelectedRemoteFiles, setConfirmDialog]);

    const deleteMultipleLocalFiles = useCallback((filesOverride?: string[]) => {
        const names = filesOverride || Array.from(selectedLocalFiles);
        if (names.length === 0) return;

        const performDelete = async () => {
            const logId = humanLog.logStart('DELETE_MULTIPLE', { count: names.length });
            const deletedFiles: string[] = [];
            const deletedFolders: string[] = [];

            for (const name of names) {
                const file = localFiles.find(f => f.name === name);
                if (file) {
                    try {
                        await invoke('delete_local_file', { path: file.path });
                        if (file.is_dir) {
                            deletedFolders.push(name);
                        } else {
                            deletedFiles.push(name);
                        }
                    } catch { }
                }
            }

            await loadLocalFiles(currentLocalPath);
            setSelectedLocalFiles(new Set());

            const parts = [];
            if (deletedFolders.length > 0) parts.push(`${deletedFolders.length} folder${deletedFolders.length > 1 ? 's' : ''}`);
            if (deletedFiles.length > 0) parts.push(`${deletedFiles.length} file${deletedFiles.length > 1 ? 's' : ''}`);
            const count = deletedFolders.length + deletedFiles.length;
            const msg = t('activity.delete_multiple_success', { count });
            humanLog.updateEntry(logId, { status: 'success', message: msg });
            notify.success(parts.join(', '), `${parts.join(' and ')} deleted`);
        };

        if (confirmBeforeDelete) {
            setConfirmDialog({
                message: `Delete ${names.length} selected items?`,
                onConfirm: async () => {
                    setConfirmDialog(null);
                    await performDelete();
                }
            });
        } else {
            performDelete();
        }
    }, [selectedLocalFiles, localFiles, confirmBeforeDelete, humanLog, t, notify,
        loadLocalFiles, currentLocalPath, setSelectedLocalFiles, setConfirmDialog]);

    // ===================================
    // RENAME Operation
    // ===================================

    const renameFile = useCallback((path: string, currentName: string, isRemote: boolean) => {
        setInputDialog({
            title: 'Rename',
            defaultValue: currentName,
            onConfirm: async (newName: string) => {
                setInputDialog(null);
                if (!newName || newName === currentName) return;

                const logId = humanLog.logStart('RENAME', { oldname: currentName, newname: newName });
                try {
                    const parentDir = path.substring(0, path.lastIndexOf('/'));
                    const newPath = parentDir + '/' + newName;

                    if (isRemote) {
                        const useProvider = isProvider();
                        if (useProvider) {
                            await invoke('provider_rename', { from: path, to: newPath });
                        } else {
                            await invoke('rename_remote_file', { from: path, to: newPath });
                        }
                        await loadRemoteFiles();
                    } else {
                        await invoke('rename_local_file', { from: path, to: newPath });
                        await loadLocalFiles(currentLocalPath);
                    }

                    humanLog.logSuccess('RENAME', { oldname: currentName, newname: newName }, logId);
                    notify.success('Renamed', newName);
                } catch (error) {
                    humanLog.logError('RENAME', { oldname: currentName, newname: newName }, logId);
                    notify.error('Rename Failed', String(error));
                }
            }
        });
    }, [isProvider, humanLog, notify, loadRemoteFiles, loadLocalFiles, currentLocalPath, setInputDialog]);

    // ===================================
    // CREATE FOLDER Operation
    // ===================================

    const createFolder = useCallback((isRemote: boolean) => {
        setInputDialog({
            title: 'New Folder',
            defaultValue: '',
            onConfirm: async (name: string) => {
                setInputDialog(null);
                if (!name) return;

                const logId = humanLog.logStart('MKDIR', { foldername: name });
                try {
                    if (isRemote) {
                        const path = currentRemotePath + (currentRemotePath.endsWith('/') ? '' : '/') + name;
                        const useProvider = isProvider();

                        if (useProvider) {
                            await invoke('provider_mkdir', { path });
                        } else {
                            await invoke('create_remote_folder', { path });
                        }
                        await loadRemoteFiles();
                    } else {
                        const path = currentLocalPath + '/' + name;
                        await invoke('create_local_folder', { path });
                        await loadLocalFiles(currentLocalPath);
                    }

                    humanLog.logSuccess('MKDIR', { foldername: name }, logId);
                    notify.success('Created', name);
                } catch (error) {
                    humanLog.logError('MKDIR', { foldername: name }, logId);
                    notify.error('Create Failed', String(error));
                }
            }
        });
    }, [currentRemotePath, currentLocalPath, isProvider, humanLog, notify, loadRemoteFiles, loadLocalFiles, setInputDialog]);

    return {
        // Loading
        loadRemoteFiles,
        loadLocalFiles,
        // Navigation
        changeRemoteDirectory,
        changeLocalDirectory,
        // Mutations
        createFolder,
        renameFile,
        deleteRemoteFile,
        deleteLocalFile,
        deleteMultipleRemoteFiles,
        deleteMultipleLocalFiles,
        // Helpers
        isProvider,
        getEffectiveProtocol,
    };
}

export default useFileOperations;
