// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * useDragAndDrop Hook
 * Wired into App.tsx during modularization (v1.3.1) - template existed since v1.2.x
 *
 * Handles drag & drop file moves within a panel, between local and remote panels
 * (upload/download), and between the two local panels in AeroFile dual-panel mode
 * (move by default, copy on Ctrl). Validates drop targets to prevent self-drops
 * and parent drops.
 */

import { useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { isNonFtpProvider, ProviderType } from '../types';
import type { HumanizedOperationType, HumanizedLogParams } from './useHumanizedLog';
import { dropsIntoItself, localRowName, outermostLocalRows } from '../utils/localSelection';

export type PanelKey = 'remote' | 'local' | 'local2';

const isLocalPanel = (key: PanelKey): key is 'local' | 'local2' => key !== 'remote';

interface DragData {
    files: string[];
    sourcePaths: string[];
    panelKey: PanelKey;
    sourceDir: string;
}

interface UseDragAndDropParams {
    /**
     * Refuses local rows that one folder cannot hold together (files of two
     * folders sharing a name, from a flattened search) and tells the user.
     * True when it refused.
     */
    refuseLocalNameClashes?: (rows: readonly { name: string; path: string }[]) => boolean;
    notify: {
        success: (title: string, message?: string) => string | null;
        error: (title: string, message?: string) => string | null;
    };
    humanLog: {
        logStart: (operation: HumanizedOperationType, params?: HumanizedLogParams) => string;
        logSuccess: (operation: HumanizedOperationType, params?: HumanizedLogParams, logId?: string) => string;
        logError: (operation: HumanizedOperationType, params?: HumanizedLogParams, logId?: string) => string;
    };
    currentRemotePath: string;
    currentLocalPath: string;
    /** Path of the secondary local panel (AeroFile dual-panel mode). */
    currentLocalPath2?: string;
    loadRemoteFiles: (overrideProtocol?: string) => Promise<unknown>;
    loadLocalFiles: (path: string) => Promise<boolean | void>;
    /** Loader for the secondary local panel. */
    loadLocalFiles2?: (path: string) => Promise<boolean | void>;
    activeSessionId?: string | null;
    sessions?: Array<{ id: string; connectionParams?: { protocol?: string } }>;
    connectionParams?: { protocol?: string };
    onCrossPanelDrop?: (files: { name: string; path: string }[], fromRemote: boolean, targetDir: string, sourceKey: PanelKey) => Promise<void>;
}

export function useDragAndDrop({
    refuseLocalNameClashes,
    notify,
    humanLog,
    currentRemotePath,
    currentLocalPath,
    currentLocalPath2,
    loadRemoteFiles,
    loadLocalFiles,
    loadLocalFiles2,
    activeSessionId,
    sessions,
    connectionParams,
    onCrossPanelDrop,
}: UseDragAndDropParams) {

    const [dragData, setDragData] = useState<DragData | null>(null);
    const [dropTargetPath, setDropTargetPath] = useState<string | null>(null);
    /** Which panel is being hovered for cross-panel drop. */
    const [crossPanelTarget, setCrossPanelTarget] = useState<PanelKey | null>(null);

    const getEffectiveProtocol = useCallback(() => {
        if (connectionParams?.protocol) return connectionParams.protocol;
        if (sessions && activeSessionId) {
            const activeSession = sessions.find(s => s.id === activeSessionId);
            return activeSession?.connectionParams?.protocol;
        }
        return undefined;
    }, [connectionParams, sessions, activeSessionId]);

    const isProvider = useCallback(() => {
        const effectiveProtocol = getEffectiveProtocol();
        if (!effectiveProtocol) return false;
        return isNonFtpProvider(effectiveProtocol as ProviderType);
    }, [getEffectiveProtocol]);

    const sourceDirFor = useCallback((key: PanelKey): string => {
        if (key === 'remote') return currentRemotePath;
        if (key === 'local2') return currentLocalPath2 || currentLocalPath;
        return currentLocalPath;
    }, [currentRemotePath, currentLocalPath, currentLocalPath2]);

    const refreshAfterLocal = useCallback(async (key: PanelKey) => {
        if (key === 'local2' && loadLocalFiles2 && currentLocalPath2) {
            await loadLocalFiles2(currentLocalPath2);
        } else {
            await loadLocalFiles(currentLocalPath);
        }
    }, [loadLocalFiles, loadLocalFiles2, currentLocalPath, currentLocalPath2]);

    /**
     * Start dragging file(s)
     */
    const handleDragStart = useCallback((
        e: React.DragEvent,
        file: { name: string; path: string; is_dir: boolean },
        panelKey: PanelKey,
        allSelected: Set<string>,
        allFiles: { name: string; path: string; is_dir: boolean }[]
    ) => {
        if (file.name === '..') {
            e.preventDefault();
            return;
        }

        const selectedRows = allSelected.has(file.name)
            ? allFiles.filter(f => allSelected.has(f.name))
            : [file];
        // A selected folder takes what is below it: a flattened search can
        // select both, and the entry would land twice, or be refused as a
        // clash with the folder that holds it.
        const filesToDrag = isLocalPanel(panelKey) ? outermostLocalRows(selectedRows) : selectedRows;

        // Dropped into one folder, two files of one name would replace each other.
        if (isLocalPanel(panelKey) && refuseLocalNameClashes?.(filesToDrag)) {
            e.preventDefault();
            return;
        }

        const sourceDir = sourceDirFor(panelKey);

        setDragData({
            // The name each file lands under. A nested row of a local
            // flattened search is named by its relative path; the drop gives
            // the file its own name, as a copy or move would.
            files: filesToDrag.map(f => (isLocalPanel(panelKey) ? localRowName(f) : f.name)),
            sourcePaths: filesToDrag.map(f => f.path),
            panelKey,
            sourceDir,
        });

        e.dataTransfer.effectAllowed = 'copyMove';

        if (isLocalPanel(panelKey)) {
            const localPaths = filesToDrag.map(f => f.path);
            e.dataTransfer.setData('application/x-aeroftp-local-paths', JSON.stringify(localPaths));
            if (localPaths.length > 0) {
                e.dataTransfer.setData('application/x-aeroftp-local-path', localPaths[0]);
            }
            e.dataTransfer.setData('text/plain', localPaths[0] || filesToDrag.map(f => f.name).join(', '));
        } else {
            e.dataTransfer.setData('text/plain', filesToDrag.map(f => f.name).join(', '));
        }
    }, [sourceDirFor, refuseLocalNameClashes]);

    /**
     * Allow dropping on files/folders within a panel
     */
    const handleDragOver = useCallback((
        e: React.DragEvent,
        targetPath: string,
        isFolder: boolean,
        targetPanelKey: PanelKey
    ) => {
        e.preventDefault();
        e.stopPropagation();

        if (!dragData) {
            e.dataTransfer.dropEffect = 'none';
            setDropTargetPath(null);
            return;
        }

        const samePanel = dragData.panelKey === targetPanelKey;

        if (samePanel) {
            if (!isFolder) {
                e.dataTransfer.dropEffect = 'none';
                setDropTargetPath(null);
                return;
            }
            const targetName = targetPath.split('/').pop();
            if (targetPath === dragData.sourceDir || targetName === '..') {
                e.dataTransfer.dropEffect = 'none';
                setDropTargetPath(null);
                return;
            }
            // A dragged folder, or a folder below one: never into itself.
            if (dropsIntoItself(targetPath, dragData.sourcePaths)) {
                e.dataTransfer.dropEffect = 'none';
                setDropTargetPath(null);
                return;
            }
            e.dataTransfer.dropEffect = 'move';
            setDropTargetPath(targetPath);
            return;
        }

        // Cross-panel drag.
        const sourceIsLocal = isLocalPanel(dragData.panelKey);
        const targetIsLocal = isLocalPanel(targetPanelKey);
        const localToLocal = sourceIsLocal && targetIsLocal;

        // local-to-local: Ctrl = copy, default = move
        if (localToLocal) {
            // The other panel can show a dragged folder, or a folder below it.
            if (isFolder && dropsIntoItself(targetPath, dragData.sourcePaths)) {
                e.dataTransfer.dropEffect = 'none';
                setDropTargetPath(null);
                setCrossPanelTarget(null);
                return;
            }
            if (isFolder && targetPath.split('/').pop() !== '..') {
                e.dataTransfer.dropEffect = e.ctrlKey ? 'copy' : 'move';
                setDropTargetPath(targetPath);
                setCrossPanelTarget(targetPanelKey);
            } else {
                e.dataTransfer.dropEffect = e.ctrlKey ? 'copy' : 'move';
                setDropTargetPath(null);
                setCrossPanelTarget(targetPanelKey);
            }
            return;
        }

        // local-to-remote or remote-to-local: copy (upload/download)
        if (isFolder && targetPath.split('/').pop() !== '..') {
            e.dataTransfer.dropEffect = 'copy';
            setDropTargetPath(targetPath);
            setCrossPanelTarget(targetPanelKey);
        } else {
            e.dataTransfer.dropEffect = 'copy';
            setDropTargetPath(null);
            setCrossPanelTarget(targetPanelKey);
        }
    }, [dragData]);

    /**
     * Move/copy files between the two local panels (or within a single local panel
     * when destination differs from source dir). Backed by rename_local_file /
     * copy_local_file. Refreshes both panels on completion.
     */
    const performLocalLocalTransfer = useCallback(async (
        sourcePaths: string[],
        files: string[],
        targetDir: string,
        isCopy: boolean,
        sourceKey: PanelKey,
        targetKey: PanelKey
    ) => {
        // Refused while dragging (no drop effect); a drop that gets here anyway
        // does nothing rather than move or copy a folder into itself.
        if (dropsIntoItself(targetDir, sourcePaths)) return;
        for (let i = 0; i < files.length; i++) {
            const fileName = files[i];
            const sourcePath = sourcePaths[i];
            const sep = targetDir.includes('\\') && !targetDir.includes('/') ? '\\' : '/';
            const destPath = `${targetDir}${targetDir.endsWith(sep) ? '' : sep}${fileName}`;

            if (destPath === sourcePath) continue;

            const logId = humanLog.logStart(isCopy ? 'COPY' : 'MOVE', { isRemote: false, filename: fileName });
            try {
                // Never over an existing entry: a drop asks nothing first.
                await invoke(isCopy ? 'copy_local_file' : 'rename_local_file', {
                    from: sourcePath,
                    to: destPath,
                    overwrite: false,
                });
                humanLog.logSuccess(isCopy ? 'COPY' : 'MOVE', { isRemote: false, filename: fileName, destination: targetDir }, logId);
                notify.success(isCopy ? `Copied ${fileName}` : `Moved ${fileName}`, `→ ${targetDir}`);
            } catch (err) {
                humanLog.logError(isCopy ? 'COPY' : 'MOVE', { isRemote: false, filename: fileName }, logId);
                notify.error(isCopy ? `Failed to copy ${fileName}` : `Failed to move ${fileName}`, String(err));
            }
        }
        await refreshAfterLocal(sourceKey);
        if (sourceKey !== targetKey) await refreshAfterLocal(targetKey);
    }, [humanLog, notify, refreshAfterLocal]);

    /**
     * Handle drop on a specific file/folder
     */
    const handleDrop = useCallback(async (
        e: React.DragEvent,
        targetPath: string,
        targetPanelKey: PanelKey
    ) => {
        e.preventDefault();
        e.stopPropagation();

        if (!dragData) {
            setDragData(null);
            setDropTargetPath(null);
            setCrossPanelTarget(null);
            return;
        }

        const { files, sourcePaths, panelKey: sourceKey } = dragData;
        const ctrlKey = e.ctrlKey;
        setDragData(null);
        setDropTargetPath(null);
        setCrossPanelTarget(null);

        const sourceIsRemote = sourceKey === 'remote';
        const targetIsRemote = targetPanelKey === 'remote';

        // local↔remote cross-panel: upload/download via callback
        if (sourceIsRemote !== targetIsRemote) {
            const fileData = files.map((name, i) => ({ name, path: sourcePaths[i] }));
            await onCrossPanelDrop?.(fileData, sourceIsRemote, targetPath, sourceKey);
            return;
        }

        // local-to-local across panels (local ↔ local2)
        if (!sourceIsRemote && !targetIsRemote && sourceKey !== targetPanelKey) {
            await performLocalLocalTransfer(sourcePaths, files, targetPath, ctrlKey, sourceKey, targetPanelKey);
            return;
        }

        // Same-panel move (rename into another folder), never into itself.
        if (dropsIntoItself(targetPath, sourcePaths)) return;
        const useProviderCmd = isProvider();

        for (let i = 0; i < files.length; i++) {
            const fileName = files[i];
            const sourcePath = sourcePaths[i];
            const destPath = `${targetPath}/${fileName}`;
            // A nested row dropped on the folder it is already in.
            if (destPath === sourcePath) continue;

            const logId = humanLog.logStart('MOVE', { isRemote: sourceIsRemote, filename: fileName });
            try {
                if (sourceIsRemote) {
                    if (useProviderCmd) {
                        await invoke('provider_rename', { from: sourcePath, to: destPath });
                    } else {
                        await invoke('rename_remote_file', { from: sourcePath, to: destPath });
                    }
                } else {
                    // Never over an existing entry: a drop asks nothing first.
                    await invoke('rename_local_file', { from: sourcePath, to: destPath, overwrite: false });
                }
                humanLog.logSuccess('MOVE', { isRemote: sourceIsRemote, filename: fileName, destination: targetPath }, logId);
                notify.success(`Moved ${fileName}`, `→ ${targetPath}`);
            } catch (err) {
                humanLog.logError('MOVE', { isRemote: sourceIsRemote, filename: fileName }, logId);
                notify.error(`Failed to move ${fileName}`, String(err));
            }
        }

        if (sourceIsRemote) {
            await loadRemoteFiles();
        } else {
            await refreshAfterLocal(sourceKey);
        }
    }, [dragData, isProvider, humanLog, notify, loadRemoteFiles, refreshAfterLocal, onCrossPanelDrop, performLocalLocalTransfer]);

    /**
     * Panel-level drag over (for cross-panel drops on empty space)
     */
    const handlePanelDragOver = useCallback((
        e: React.DragEvent,
        targetPanelKey: PanelKey
    ) => {
        e.preventDefault();
        if (!dragData || dragData.panelKey === targetPanelKey) return;
        const sourceIsRemote = dragData.panelKey === 'remote';
        const targetIsRemote = targetPanelKey === 'remote';
        if (sourceIsRemote === targetIsRemote) {
            // local-to-local cross panel, into the other panel's folder: not
            // when that folder is a dragged one or below one.
            const targetDir = targetPanelKey === 'local2' ? (currentLocalPath2 || currentLocalPath) : currentLocalPath;
            if (dropsIntoItself(targetDir, dragData.sourcePaths)) {
                e.dataTransfer.dropEffect = 'none';
                setCrossPanelTarget(null);
                return;
            }
            e.dataTransfer.dropEffect = e.ctrlKey ? 'copy' : 'move';
        } else {
            e.dataTransfer.dropEffect = 'copy';
        }
        setCrossPanelTarget(targetPanelKey);
    }, [dragData, currentLocalPath, currentLocalPath2]);

    /**
     * Panel-level drop (cross-panel drop on empty space → use current directory)
     */
    const handlePanelDrop = useCallback(async (
        e: React.DragEvent,
        targetPanelKey: PanelKey
    ) => {
        e.preventDefault();
        e.stopPropagation();

        if (!dragData || dragData.panelKey === targetPanelKey) {
            setCrossPanelTarget(null);
            return;
        }

        const { files, sourcePaths, panelKey: sourceKey } = dragData;
        const ctrlKey = e.ctrlKey;
        setDragData(null);
        setDropTargetPath(null);
        setCrossPanelTarget(null);

        const sourceIsRemote = sourceKey === 'remote';
        const targetIsRemote = targetPanelKey === 'remote';

        const targetDir = targetPanelKey === 'remote'
            ? currentRemotePath
            : targetPanelKey === 'local2'
                ? (currentLocalPath2 || currentLocalPath)
                : currentLocalPath;

        if (sourceIsRemote !== targetIsRemote) {
            const fileData = files.map((name, i) => ({ name, path: sourcePaths[i] }));
            await onCrossPanelDrop?.(fileData, sourceIsRemote, targetDir, sourceKey);
            return;
        }

        if (!sourceIsRemote && !targetIsRemote && sourceKey !== targetPanelKey) {
            await performLocalLocalTransfer(sourcePaths, files, targetDir, ctrlKey, sourceKey, targetPanelKey);
        }
    }, [dragData, currentRemotePath, currentLocalPath, currentLocalPath2, onCrossPanelDrop, performLocalLocalTransfer]);

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
