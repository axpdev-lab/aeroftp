import { useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { RemoteFile, LocalFile, FileListResponse } from '../types';
import { homeDir, downloadDir } from '@tauri-apps/api/path';
import { useActivityLog } from './useActivityLog';

interface UseFileOperationsParams {
    toast: any;
    setConfirmDialog: (dialog: any) => void;
    setInputDialog: (dialog: any) => void;
    setRemoteFiles: (files: RemoteFile[]) => void;
    setLocalFiles: (files: LocalFile[]) => void;
    setCurrentRemotePath: (path: string) => void;
    setCurrentLocalPath: (path: string) => void;
    setSyncNavDialog: (dialog: any) => void;
    currentRemotePath: string;
    currentLocalPath: string;
    remoteFiles: RemoteFile[];
    localFiles: LocalFile[];
    selectedRemoteFiles: Set<string>;
    selectedLocalFiles: Set<string>;
    setSelectedRemoteFiles: (files: Set<string>) => void;
    setSelectedLocalFiles: (files: Set<string>) => void;
    isSyncNavigation?: boolean;
    syncBasePaths?: { remote: string; local: string };
    showHiddenFiles?: boolean;
    isConnected?: boolean;
    protocol?: string;
}

export function useFileOperations({
    toast,
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
    showHiddenFiles,
    isConnected,
    protocol
}: UseFileOperationsParams) {

    const activityLog = useActivityLog();

    // ===================================
    // LOADING FILES
    // ===================================

    const loadRemoteFiles = useCallback(async () => {
        try {
            const response: FileListResponse = await invoke('list_files');
            setRemoteFiles(response.files);
            setCurrentRemotePath(response.current_path);
        } catch (error) {
            toast.error('Error', `Failed to list files: ${error}`);
        }
    }, [setRemoteFiles, setCurrentRemotePath, toast]);

    const loadLocalFiles = useCallback(async (path: string) => {
        try {
            const files: LocalFile[] = await invoke('get_local_files', { path, showHidden: showHiddenFiles });
            setLocalFiles(files);
            setCurrentLocalPath(path);
        } catch (error) {
            toast.error('Local Error', `Failed to load local files: ${error}`);
        }
    }, [setLocalFiles, setCurrentLocalPath, toast, showHiddenFiles]);

    // ===================================
    // NAVIGATION
    // ===================================

    const changeRemoteDirectory = useCallback(async (path: string) => {
        try {
            // Use provider_change_dir for S3 and WebDAV, change_directory for FTP
            const isProviderProtocol = protocol && ['s3', 'webdav'].includes(protocol);
            let response: FileListResponse;

            if (isProviderProtocol) {
                response = await invoke('provider_change_dir', { path });
            } else {
                response = await invoke('change_directory', { path });
            }

            setRemoteFiles(response.files);
            setCurrentRemotePath(response.current_path);
            activityLog.log('NAVIGATE', `Remote: ${response.current_path}`, 'success');

            // Navigation Sync: mirror to local panel if enabled
            if (isSyncNavigation && syncBasePaths) {
                const relativePath = response.current_path.startsWith(syncBasePaths.remote)
                    ? response.current_path.slice(syncBasePaths.remote.length)
                    : '';
                // Join paths avoiding double slashes
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
            toast.error('Error', `Failed to change directory: ${error}`);
        }
    }, [protocol, isSyncNavigation, syncBasePaths, showHiddenFiles, activityLog, toast, setRemoteFiles, setCurrentRemotePath, setLocalFiles, setCurrentLocalPath, setSyncNavDialog]);

    const changeLocalDirectory = useCallback(async (path: string) => {
        await loadLocalFiles(path);
        activityLog.log('NAVIGATE', `Local: ${path}`, 'success');

        // Navigation Sync: mirror to remote panel if enabled
        if (isSyncNavigation && syncBasePaths && isConnected) {
            const relativePath = path.startsWith(syncBasePaths.local)
                ? path.slice(syncBasePaths.local.length)
                : '';
            // Join paths avoiding double slashes
            const basePath = syncBasePaths.remote.endsWith('/') ? syncBasePaths.remote.slice(0, -1) : syncBasePaths.remote;
            const relPath = relativePath.startsWith('/') ? relativePath : '/' + relativePath;
            const newRemotePath = relativePath ? basePath + relPath : basePath;
            try {
                // Use provider_change_dir for S3 and WebDAV, change_directory for FTP
                const isProviderProtocol = protocol && ['s3', 'webdav'].includes(protocol);
                let response: FileListResponse;

                if (isProviderProtocol) {
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
    }, [loadLocalFiles, protocol, isSyncNavigation, syncBasePaths, isConnected, activityLog, setRemoteFiles, setCurrentRemotePath, setSyncNavDialog]);

    // ===================================
    // MUTATION Operations (Delete, Rename, Create)
    // ===================================

    const createFolder = useCallback((isRemote: boolean) => {
        setInputDialog({
            title: 'New Folder',
            defaultValue: '',
            onConfirm: async (name: string) => {
                setInputDialog(null);
                if (!name) return;
                try {
                    if (isRemote) {
                        const path = currentRemotePath + (currentRemotePath.endsWith('/') ? '' : '/') + name;
                        await invoke('create_remote_folder', { path });
                        await loadRemoteFiles();
                        activityLog.log('MKDIR', `Created remote folder: ${name}`, 'success');
                    } else {
                        const path = currentLocalPath + '/' + name;
                        await invoke('create_local_folder', { path });
                        await loadLocalFiles(currentLocalPath);
                        activityLog.log('MKDIR', `Created local folder: ${name}`, 'success');
                    }
                    toast.success('Created', name);
                } catch (error) {
                    activityLog.log('ERROR', `Failed to create folder: ${name}`, 'error');
                    toast.error('Create Failed', String(error));
                }
            }
        });
    }, [currentRemotePath, currentLocalPath, loadRemoteFiles, loadLocalFiles, activityLog, toast, setInputDialog]);

    const renameFile = useCallback((path: string, currentName: string, isRemote: boolean) => {
        setInputDialog({
            title: 'Rename',
            defaultValue: currentName,
            onConfirm: async (newName: string) => {
                setInputDialog(null);
                if (!newName || newName === currentName) return;
                try {
                    const parentDir = path.substring(0, path.lastIndexOf('/'));
                    const newPath = parentDir + '/' + newName;

                    if (isRemote) {
                        await invoke('rename_remote_file', { from: path, to: newPath });
                        await loadRemoteFiles();
                        activityLog.log('RENAME', `Renamed remote: ${currentName} → ${newName}`, 'success');
                    } else {
                        await invoke('rename_local_file', { from: path, to: newPath });
                        await loadLocalFiles(currentLocalPath);
                        activityLog.log('RENAME', `Renamed local: ${currentName} → ${newName}`, 'success');
                    }
                    toast.success('Renamed', newName);
                } catch (error) {
                    activityLog.log('ERROR', `Failed to rename: ${currentName}`, 'error');
                    toast.error('Rename Failed', String(error));
                }
            }
        });
    }, [loadRemoteFiles, loadLocalFiles, currentLocalPath, activityLog, toast, setInputDialog]);

    const deleteRemoteFile = useCallback((path: string, isDir: boolean) => {
        const fileName = path.split('/').pop() || path;
        setConfirmDialog({
            message: `Delete "${fileName}"?`,
            onConfirm: async () => {
                setConfirmDialog(null);
                try {
                    // Backend now emits detailed events for each file deleted
                    const result = await invoke<string>('delete_remote_file', { path, isDir });
                    toast.success('Deleted', result);
                    // Refresh is handled by transfer_event listener in App.tsx
                } catch (error) {
                    toast.error('Delete Failed', String(error));
                }
            }
        });
    }, [toast, setConfirmDialog]);

    const deleteLocalFile = useCallback((path: string) => {
        const fileName = path.split('/').pop() || path;
        setConfirmDialog({
            message: `Delete "${fileName}"?`,
            onConfirm: async () => {
                setConfirmDialog(null);
                try {
                    // Backend now emits detailed events for each file deleted
                    const result = await invoke<string>('delete_local_file', { path });
                    toast.success('Deleted', result);
                    // Refresh is handled by transfer_event listener in App.tsx
                } catch (error) {
                    toast.error('Delete Failed', String(error));
                }
            }
        });
    }, [toast, setConfirmDialog]);

    const deleteMultipleRemoteFiles = useCallback((filesOverride?: string[]) => {
        const names = filesOverride || Array.from(selectedRemoteFiles);
        if (names.length === 0) return;

        setConfirmDialog({
            message: `Delete ${names.length} selected items?`,
            onConfirm: async () => {
                setConfirmDialog(null);
                // Backend emits detailed events for each file deleted
                let deleted = 0;
                let errors = 0;
                for (const name of names) {
                    const file = remoteFiles.find(f => f.name === name);
                    if (file) {
                        try {
                            await invoke<string>('delete_remote_file', { path: file.path, isDir: file.is_dir });
                            deleted++;
                        } catch {
                            errors++;
                        }
                    }
                }
                setSelectedRemoteFiles(new Set());
                // Refresh handled by transfer_event listener for each delete

                const folderCount = names.filter(n => remoteFiles.find(f => f.name === n)?.is_dir).length;
                const fileCount = names.length - folderCount;
                const messages = [];
                if (folderCount > 0) messages.push(`${folderCount} folder${folderCount > 1 ? 's' : ''}`);
                if (fileCount > 0) messages.push(`${fileCount} file${fileCount > 1 ? 's' : ''}`);

                if (errors > 0) {
                    toast.warning('Delete Complete', `${deleted} deleted, ${errors} failed`);
                } else {
                    toast.success(messages.join(', '), `${messages.join(' and ')} deleted`);
                }
            }
        });
    }, [selectedRemoteFiles, remoteFiles, toast, setConfirmDialog, setSelectedRemoteFiles]);

    const deleteMultipleLocalFiles = useCallback((filesOverride?: string[]) => {
        const names = filesOverride || Array.from(selectedLocalFiles);
        if (names.length === 0) return;

        setConfirmDialog({
            message: `Delete ${names.length} selected items?`,
            onConfirm: async () => {
                setConfirmDialog(null);
                // Backend emits detailed events for each file deleted
                let deleted = 0;
                let errors = 0;
                for (const name of names) {
                    const file = localFiles.find(f => f.name === name);
                    if (file) {
                        try {
                            await invoke<string>('delete_local_file', { path: file.path });
                            deleted++;
                        } catch {
                            errors++;
                        }
                    }
                }
                setSelectedLocalFiles(new Set());
                // Refresh handled by transfer_event listener for each delete

                const folderCount = names.filter(n => localFiles.find(f => f.name === n)?.is_dir).length;
                const fileCount = names.length - folderCount;
                const messages = [];
                if (folderCount > 0) messages.push(`${folderCount} folder${folderCount > 1 ? 's' : ''}`);
                if (fileCount > 0) messages.push(`${fileCount} file${fileCount > 1 ? 's' : ''}`);

                if (errors > 0) {
                    toast.warning('Delete Complete', `${deleted} deleted, ${errors} failed`);
                } else {
                    toast.success(messages.join(', '), `${messages.join(' and ')} deleted`);
                }
            }
        });
    }, [selectedLocalFiles, localFiles, toast, setConfirmDialog, setSelectedLocalFiles]);

    return {
        loadRemoteFiles,
        loadLocalFiles,
        changeRemoteDirectory,
        changeLocalDirectory,
        createFolder,
        renameFile,
        deleteRemoteFile,
        deleteLocalFile,
        deleteMultipleRemoteFiles,
        deleteMultipleLocalFiles
    };
}
