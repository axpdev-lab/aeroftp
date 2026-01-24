import { useState, useCallback, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { RemoteFile, LocalFile, FtpSession, ConnectionParams, FileListResponse } from '../types';
import { useActivityLog } from './useActivityLog';
import { useTranslation } from '../i18n';

interface UseFtpOperationsParams {
    connectionParams: ConnectionParams;
    quickConnectDirs: { remoteDir: string; localDir: string };
    setConnectionParams: (params: ConnectionParams) => void;

    // Callbacks provided by App.tsx
    toast: { success: (t: string, m: string) => void; error: (t: string, m: string) => void; info: (t: string, m: string) => void };
    loadRemoteFiles: () => Promise<void>;
    changeRemoteDirectory: (path: string) => Promise<void>;
    changeLocalDirectory: (path: string) => Promise<void>;
    setRemoteFiles: (files: RemoteFile[]) => void;
    setCurrentRemotePath: (path: string) => void;
    setSessions: (sessions: FtpSession[]) => void;
    setActiveSessionId: (id: string | null) => void;
    setDevToolsOpen: (open: boolean) => void;
    setDevToolsPreviewFile: (file: any) => void;
    setLoading: (loading: boolean) => void;

    // Lifted state
    isConnected: boolean;
    setIsConnected: (connected: boolean) => void;
}

export function useFtpOperations({
    connectionParams,
    quickConnectDirs,
    setConnectionParams,
    toast,
    loadRemoteFiles,
    changeRemoteDirectory,
    changeLocalDirectory,
    setRemoteFiles,
    setCurrentRemotePath,
    setSessions,
    setActiveSessionId,
    setDevToolsOpen,
    setDevToolsPreviewFile,
    setLoading,
    isConnected,
    setIsConnected
}: UseFtpOperationsParams) {

    const [isReconnecting, setIsReconnecting] = useState(false);
    const activityLog = useActivityLog();
    const t = useTranslation();

    const connectToFtp = useCallback(async () => {
        if (!connectionParams.server || !connectionParams.username) {
            toast.error('Missing Fields', 'Please fill in server and username');
            return;
        }

        setLoading(true);
        const logId = activityLog.log('CONNECT', t('activity.connect_start', { server: connectionParams.server }), 'running');

        try {
            await invoke('connect_ftp', { params: connectionParams });
            setIsConnected(true);
            activityLog.updateEntry(logId, { status: 'success', message: t('activity.connect_success', { server: connectionParams.server }) });
            toast.success('Connected', `Connected to ${connectionParams.server}`);

            // Navigate to initial remote directory if specified
            if (quickConnectDirs.remoteDir) {
                await changeRemoteDirectory(quickConnectDirs.remoteDir);
            } else {
                await loadRemoteFiles();
            }

            // Navigate to initial local directory if specified
            if (quickConnectDirs.localDir) {
                await changeLocalDirectory(quickConnectDirs.localDir);
            }
        } catch (error) {
            activityLog.updateEntry(logId, { status: 'error', message: `Connection failed: ${error}` });
            toast.error('Connection Failed', String(error));
        } finally {
            setLoading(false);
        }
    }, [connectionParams, quickConnectDirs, activityLog, toast, changeRemoteDirectory, loadRemoteFiles, changeLocalDirectory, setLoading, t]);

    const disconnectFromFtp = useCallback(async () => {
        activityLog.log('DISCONNECT', t('activity.disconnect_start'), 'running');
        try {
            await invoke('disconnect_ftp');
            setIsConnected(false);
            setRemoteFiles([]);
            setCurrentRemotePath('/');
            // Close all session tabs on disconnect
            setSessions([]);
            setActiveSessionId(null);
            // Close DevTools panel and clear preview
            setDevToolsOpen(false);
            setDevToolsPreviewFile(null);
            activityLog.log('DISCONNECT', t('activity.disconnect_success'), 'success');
            toast.info('Disconnected', 'Disconnected from server');
        } catch (error) {
            activityLog.log('ERROR', t('activity.disconnect_error'), 'error');
            toast.error('Error', `Disconnection failed: ${error}`);
        }
    }, [connectionParams.server, activityLog, toast, setRemoteFiles, setCurrentRemotePath, setSessions, setActiveSessionId, setDevToolsOpen, setDevToolsPreviewFile, t]);

    // Keep-Alive logic
    useEffect(() => {
        if (!isConnected) return;

        // Skip keep-alive for non-FTP providers (stateless REST APIs don't need keep-alive)
        const protocol = connectionParams.protocol;
        const isProvider = protocol && ['googledrive', 'dropbox', 'onedrive', 's3', 'webdav', 'mega'].includes(protocol);
        if (isProvider) return;

        const KEEP_ALIVE_INTERVAL = 60000; // 60 seconds
        const keepAliveInterval = setInterval(async () => {
            try {
                await invoke('ftp_noop');
            } catch (error) {
                console.warn('Keep-alive NOOP failed, attempting reconnect...', error);

                // Connection lost - attempt auto-reconnect
                setIsReconnecting(true);
                activityLog.log('DISCONNECT', t('activity.disconnect_error'), 'error');
                toast.info('Reconnecting...', 'Connection lost, attempting to reconnect');

                try {
                    await invoke('reconnect_ftp');
                    toast.success('Reconnected', 'FTP connection restored');
                    activityLog.log('CONNECT', t('activity.reconnect_success', { server: connectionParams.server }), 'success');
                    // Refresh file list after reconnection
                    const response = await invoke<{ files: RemoteFile[]; current_path: string }>('list_files');
                    setRemoteFiles(response.files);
                    setCurrentRemotePath(response.current_path);
                } catch (reconnectError) {
                    console.error('Auto-reconnect failed:', reconnectError);
                    toast.error('Connection Lost', 'Could not reconnect. Please reconnect manually.');
                    activityLog.log('DISCONNECT', t('activity.reconnect_error'), 'error');
                    setIsConnected(false);
                } finally {
                    setIsReconnecting(false);
                }
            }
        }, KEEP_ALIVE_INTERVAL);

        return () => clearInterval(keepAliveInterval);
    }, [isConnected, toast, setRemoteFiles, setCurrentRemotePath, t, connectionParams.server]);

    return {
        isConnected,
        isReconnecting,
        setIsConnected, // Exported if needed by other components
        connectToFtp,
        disconnectFromFtp
    };
}
