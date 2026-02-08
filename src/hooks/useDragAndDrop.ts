/**
 * useDragAndDrop Hook
 * Wired into App.tsx during modularization (v1.3.1) - template existed since v1.2.x
 *
 * Handles intra-panel drag & drop file moves AND cross-panel drag for upload/download.
 * Supports both FTP (ftp_rename) and Provider protocols (provider_rename) for remote moves,
 * and rename_local_file for local moves. Validates drop targets to prevent self-drops
 * and parent drops.
 *
 * Props: notify, humanLog, currentRemotePath, currentLocalPath, loadRemoteFiles,
 *        loadLocalFiles, activeSessionId, sessions, connectionParams, onCrossPanelDrop
 * Returns: dragData, dropTargetPath, crossPanelTarget, handleDragStart/Over/Drop/End/Leave,
 *          handlePanelDragOver, handlePanelDrop, handlePanelDragLeave,
 *          isInDragSource, isDropTarget
 */

import { useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { HumanizedOperationType, HumanizedLogParams } from './useHumanizedLog';

// List of provider protocols (non-FTP)
const PROVIDER_PROTOCOLS = ['googledrive', 'dropbox', 'onedrive', 's3', 'webdav', 'mega', 'sftp'];

interface DragData {
    files: string[];  // File names being dragged
    sourcePaths: string[];  // Full paths of files being dragged
    isRemote: boolean;  // Whether dragging from remote or local panel
    sourceDir: string;  // Source directory path
}

interface UseDragAndDropParams {
    notify: {
        success: (title: string, message?: string) => string | null;
        error: (title: string, message?: string) => string;
    };
    humanLog: {
        logStart: (operation: HumanizedOperationType, params?: HumanizedLogParams) => string;
        logSuccess: (operation: HumanizedOperationType, params?: HumanizedLogParams, logId?: string) => string;
        logError: (operation: HumanizedOperationType, params?: HumanizedLogParams, logId?: string) => string;
    };
    currentRemotePath: string;
    currentLocalPath: string;
    loadRemoteFiles: () => Promise<void>;
    loadLocalFiles: (path: string) => Promise<boolean | void>;
    // Provider detection
    activeSessionId?: string | null;
    sessions?: Array<{ id: string; connectionParams?: { protocol?: string } }>;
    connectionParams?: { protocol?: string };
    // Cross-panel transfer callback
    onCrossPanelDrop?: (files: { name: string; path: string }[], fromRemote: boolean, targetDir: string) => Promise<void>;
}

export function useDragAndDrop({
    notify,
    humanLog,
    currentRemotePath,
    currentLocalPath,
    loadRemoteFiles,
    loadLocalFiles,
    activeSessionId,
    sessions,
    connectionParams,
    onCrossPanelDrop,
}: UseDragAndDropParams) {

    // Drag state
    const [dragData, setDragData] = useState<DragData | null>(null);
    const [dropTargetPath, setDropTargetPath] = useState<string | null>(null);
    // Which panel is being hovered for cross-panel drop: 'remote' | 'local' | null
    const [crossPanelTarget, setCrossPanelTarget] = useState<'remote' | 'local' | null>(null);

    // Helper: Get effective protocol from various sources
    const getEffectiveProtocol = useCallback(() => {
        if (connectionParams?.protocol) return connectionParams.protocol;
        if (sessions && activeSessionId) {
            const activeSession = sessions.find(s => s.id === activeSessionId);
            return activeSession?.connectionParams?.protocol;
        }
        return undefined;
    }, [connectionParams, sessions, activeSessionId]);

    // Helper: Check if current connection is a Provider (non-FTP)
    const isProvider = useCallback(() => {
        const effectiveProtocol = getEffectiveProtocol();
        return effectiveProtocol && PROVIDER_PROTOCOLS.includes(effectiveProtocol);
    }, [getEffectiveProtocol]);

    /**
     * Start dragging file(s)
     */
    const handleDragStart = useCallback((
        e: React.DragEvent,
        file: { name: string; path: string; is_dir: boolean },
        isRemote: boolean,
        allSelected: Set<string>,
        allFiles: { name: string; path: string }[]
    ) => {
        // Don't allow dragging ".." (go up)
        if (file.name === '..') {
            e.preventDefault();
            return;
        }

        // Get all selected files, or just the dragged file if not in selection
        const filesToDrag = allSelected.has(file.name)
            ? allFiles.filter(f => allSelected.has(f.name))
            : [file];

        const sourceDir = isRemote ? currentRemotePath : currentLocalPath;

        setDragData({
            files: filesToDrag.map(f => f.name),
            sourcePaths: filesToDrag.map(f => f.path),
            isRemote,
            sourceDir,
        });

        // Allow both move (same panel) and copy (cross panel)
        e.dataTransfer.effectAllowed = 'copyMove';
        e.dataTransfer.setData('text/plain', filesToDrag.map(f => f.name).join(', '));
    }, [currentRemotePath, currentLocalPath]);

    /**
     * Allow dropping on files/folders within a panel
     */
    const handleDragOver = useCallback((
        e: React.DragEvent,
        targetPath: string,
        isFolder: boolean,
        isRemotePanel: boolean
    ) => {
        e.preventDefault();
        e.stopPropagation();

        if (!dragData) {
            e.dataTransfer.dropEffect = 'none';
            setDropTargetPath(null);
            return;
        }

        // Same-panel drag: move/rename (existing behavior)
        if (dragData.isRemote === isRemotePanel) {
            if (!isFolder) {
                e.dataTransfer.dropEffect = 'none';
                setDropTargetPath(null);
                return;
            }
            // Don't allow dropping on source directory or parent (..)
            const targetName = targetPath.split('/').pop();
            if (targetPath === dragData.sourceDir || targetName === '..') {
                e.dataTransfer.dropEffect = 'none';
                setDropTargetPath(null);
                return;
            }
            // Don't allow dropping a folder into itself
            if (dragData.sourcePaths.includes(targetPath)) {
                e.dataTransfer.dropEffect = 'none';
                setDropTargetPath(null);
                return;
            }
            e.dataTransfer.dropEffect = 'move';
            setDropTargetPath(targetPath);
        }
        // Cross-panel drag: upload/download — allow drop on folders
        else {
            if (isFolder && targetPath.split('/').pop() !== '..') {
                e.dataTransfer.dropEffect = 'copy';
                setDropTargetPath(targetPath);
                setCrossPanelTarget(isRemotePanel ? 'remote' : 'local');
            } else {
                e.dataTransfer.dropEffect = 'copy';
                // Don't highlight non-folder items, but still allow panel-level drop
                setDropTargetPath(null);
                setCrossPanelTarget(isRemotePanel ? 'remote' : 'local');
            }
        }
    }, [dragData]);

    /**
     * Handle drop on a specific file/folder
     */
    const handleDrop = useCallback(async (
        e: React.DragEvent,
        targetPath: string,
        isRemotePanel: boolean
    ) => {
        e.preventDefault();
        e.stopPropagation();

        if (!dragData) {
            setDragData(null);
            setDropTargetPath(null);
            setCrossPanelTarget(null);
            return;
        }

        const { files, sourcePaths, isRemote } = dragData;
        setDragData(null);
        setDropTargetPath(null);
        setCrossPanelTarget(null);

        // Cross-panel: trigger upload or download
        if (isRemote !== isRemotePanel) {
            const fileData = files.map((name, i) => ({ name, path: sourcePaths[i] }));
            await onCrossPanelDrop?.(fileData, isRemote, targetPath);
            return;
        }

        // Same-panel: existing rename/move logic
        const useProviderCmd = isProvider();

        for (let i = 0; i < files.length; i++) {
            const fileName = files[i];
            const sourcePath = sourcePaths[i];
            const destPath = `${targetPath}/${fileName}`;

            const logId = humanLog.logStart('MOVE', { isRemote, filename: fileName });

            try {
                if (isRemote) {
                    if (useProviderCmd) {
                        await invoke('provider_rename', { from: sourcePath, to: destPath });
                    } else {
                        await invoke('rename_remote_file', { from: sourcePath, to: destPath });
                    }
                } else {
                    await invoke('rename_local_file', { from: sourcePath, to: destPath });
                }
                humanLog.logSuccess('MOVE', { isRemote, filename: fileName, destination: targetPath }, logId);
                notify.success(`Moved ${fileName}`, `→ ${targetPath}`);
            } catch (err) {
                humanLog.logError('MOVE', { isRemote, filename: fileName }, logId);
                notify.error(`Failed to move ${fileName}`, String(err));
            }
        }

        if (isRemote) {
            await loadRemoteFiles();
        } else {
            await loadLocalFiles(currentLocalPath);
        }
    }, [dragData, isProvider, humanLog, notify, loadRemoteFiles, loadLocalFiles, currentLocalPath, onCrossPanelDrop]);

    /**
     * Panel-level drag over (for cross-panel drops on empty space)
     */
    const handlePanelDragOver = useCallback((
        e: React.DragEvent,
        isRemotePanel: boolean
    ) => {
        e.preventDefault();
        if (!dragData || dragData.isRemote === isRemotePanel) return;
        e.dataTransfer.dropEffect = 'copy';
        setCrossPanelTarget(isRemotePanel ? 'remote' : 'local');
    }, [dragData]);

    /**
     * Panel-level drop (cross-panel drop on empty space → use current directory)
     */
    const handlePanelDrop = useCallback(async (
        e: React.DragEvent,
        isRemotePanel: boolean
    ) => {
        e.preventDefault();
        e.stopPropagation();

        if (!dragData || dragData.isRemote === isRemotePanel) {
            setCrossPanelTarget(null);
            return;
        }

        const { files, sourcePaths, isRemote } = dragData;
        setDragData(null);
        setDropTargetPath(null);
        setCrossPanelTarget(null);

        const targetDir = isRemotePanel ? currentRemotePath : currentLocalPath;
        const fileData = files.map((name, i) => ({ name, path: sourcePaths[i] }));
        await onCrossPanelDrop?.(fileData, isRemote, targetDir);
    }, [dragData, currentRemotePath, currentLocalPath, onCrossPanelDrop]);

    /**
     * Panel-level drag leave
     */
    const handlePanelDragLeave = useCallback((e: React.DragEvent) => {
        e.preventDefault();
        const relatedTarget = e.relatedTarget as HTMLElement;
        if (!relatedTarget || !e.currentTarget.contains(relatedTarget)) {
            setCrossPanelTarget(null);
        }
    }, []);

    /**
     * Clean up drag state
     */
    const handleDragEnd = useCallback(() => {
        setDragData(null);
        setDropTargetPath(null);
        setCrossPanelTarget(null);
    }, []);

    /**
     * Handle drag leave on individual items
     */
    const handleDragLeave = useCallback((e: React.DragEvent) => {
        e.preventDefault();
        const relatedTarget = e.relatedTarget as HTMLElement;
        if (!relatedTarget || !e.currentTarget.contains(relatedTarget)) {
            setDropTargetPath(null);
        }
    }, []);

    const isDragging = dragData !== null;

    const isInDragSource = useCallback((filePath: string) => {
        return dragData?.sourcePaths.includes(filePath) || false;
    }, [dragData]);

    const isDropTarget = useCallback((filePath: string, isDir: boolean) => {
        return dropTargetPath === filePath && isDir;
    }, [dropTargetPath]);

    return {
        // State
        dragData,
        dropTargetPath,
        crossPanelTarget,
        isDragging,
        // Handlers for individual items
        handleDragStart,
        handleDragOver,
        handleDrop,
        handleDragEnd,
        handleDragLeave,
        // Handlers for panel-level drop zones
        handlePanelDragOver,
        handlePanelDrop,
        handlePanelDragLeave,
        // Helper functions
        isInDragSource,
        isDropTarget,
    };
}

export default useDragAndDrop;
