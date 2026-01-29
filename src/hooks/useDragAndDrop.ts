/**
 * useDragAndDrop Hook
 * Wired into App.tsx during modularization (v1.3.1) - template existed since v1.2.x
 *
 * Handles intra-panel drag & drop file moves. Supports both FTP (ftp_rename)
 * and Provider protocols (provider_rename) for remote moves, and rename_local_file
 * for local moves. Validates drop targets to prevent self-drops and parent drops.
 *
 * Props: notify, humanLog, currentRemotePath, currentLocalPath, loadRemoteFiles,
 *        loadLocalFiles, activeSessionId, sessions, connectionParams
 * Returns: dragData, dropTargetPath, handleDragStart/Over/Drop/End/Leave,
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
    loadLocalFiles: (path: string) => Promise<void>;
    // Provider detection
    activeSessionId?: string | null;
    sessions?: Array<{ id: string; connectionParams?: { protocol?: string } }>;
    connectionParams?: { protocol?: string };
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
}: UseDragAndDropParams) {

    // Drag state
    const [dragData, setDragData] = useState<DragData | null>(null);
    const [dropTargetPath, setDropTargetPath] = useState<string | null>(null);

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

        // Set drag image/effect
        e.dataTransfer.effectAllowed = 'move';
        e.dataTransfer.setData('text/plain', filesToDrag.map(f => f.name).join(', '));
    }, [currentRemotePath, currentLocalPath]);

    /**
     * Allow dropping on folders
     */
    const handleDragOver = useCallback((
        e: React.DragEvent,
        targetPath: string,
        isFolder: boolean,
        isRemotePanel: boolean
    ) => {
        e.preventDefault();
        e.stopPropagation();

        // Only allow drop on folders in the same panel
        if (!dragData || dragData.isRemote !== isRemotePanel || !isFolder) {
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
    }, [dragData]);

    /**
     * Handle drop - move files to target folder
     */
    const handleDrop = useCallback(async (
        e: React.DragEvent,
        targetPath: string,
        isRemotePanel: boolean
    ) => {
        e.preventDefault();
        e.stopPropagation();

        if (!dragData || dragData.isRemote !== isRemotePanel) {
            setDragData(null);
            setDropTargetPath(null);
            return;
        }

        const { files, sourcePaths, isRemote } = dragData;
        setDragData(null);
        setDropTargetPath(null);

        // Check if using provider
        const useProvider = isProvider();

        // Move each file
        for (let i = 0; i < files.length; i++) {
            const fileName = files[i];
            const sourcePath = sourcePaths[i];
            const destPath = `${targetPath}/${fileName}`;

            // Log move start
            const logId = humanLog.logStart('MOVE', { isRemote, filename: fileName });

            try {
                if (isRemote) {
                    // Remote file move (rename)
                    if (useProvider) {
                        await invoke('provider_rename', { from: sourcePath, to: destPath });
                    } else {
                        await invoke('ftp_rename', { from: sourcePath, to: destPath });
                    }
                } else {
                    // Local file move
                    await invoke('rename_local_file', { from: sourcePath, to: destPath });
                }
                // Log move success
                humanLog.logSuccess('MOVE', { isRemote, filename: fileName, destination: targetPath }, logId);
                notify.success(`Moved ${fileName}`, `→ ${targetPath}`);
            } catch (err) {
                // Log move error
                humanLog.logError('MOVE', { isRemote, filename: fileName }, logId);
                notify.error(`Failed to move ${fileName}`, String(err));
            }
        }

        // Refresh the file list
        if (isRemote) {
            await loadRemoteFiles();
        } else {
            await loadLocalFiles(currentLocalPath);
        }
    }, [dragData, isProvider, humanLog, notify, loadRemoteFiles, loadLocalFiles, currentLocalPath]);

    /**
     * Clean up drag state
     */
    const handleDragEnd = useCallback(() => {
        setDragData(null);
        setDropTargetPath(null);
    }, []);

    /**
     * Handle drag leave
     */
    const handleDragLeave = useCallback((e: React.DragEvent) => {
        e.preventDefault();
        // Only clear if leaving the drop target completely
        const relatedTarget = e.relatedTarget as HTMLElement;
        if (!relatedTarget || !e.currentTarget.contains(relatedTarget)) {
            setDropTargetPath(null);
        }
    }, []);

    /**
     * Check if a file is being dragged (for styling)
     */
    const isDragging = dragData !== null;

    /**
     * Check if a file path is in the drag source (for opacity styling)
     */
    const isInDragSource = useCallback((filePath: string) => {
        return dragData?.sourcePaths.includes(filePath) || false;
    }, [dragData]);

    /**
     * Check if a path is the current drop target (for highlight styling)
     */
    const isDropTarget = useCallback((filePath: string, isDir: boolean) => {
        return dropTargetPath === filePath && isDir;
    }, [dropTargetPath]);

    return {
        // State
        dragData,
        dropTargetPath,
        isDragging,
        // Handlers
        handleDragStart,
        handleDragOver,
        handleDrop,
        handleDragEnd,
        handleDragLeave,
        // Helper functions
        isInDragSource,
        isDropTarget,
    };
}

export default useDragAndDrop;
