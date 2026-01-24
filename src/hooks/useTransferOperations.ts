import { useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { downloadDir } from '@tauri-apps/api/path';
import { DownloadParams, DownloadFolderParams, UploadParams, UploadFolderParams } from '../types';
import { useActivityLog } from './useActivityLog';
import { useTranslation } from '../i18n';

interface UseTransferOperationsParams {
    toast: any;
    currentRemotePath: string;
}

export function useTransferOperations({
    toast,
    currentRemotePath
}: UseTransferOperationsParams) {

    const activityLog = useActivityLog();
    const t = useTranslation();

    // ===================================
    // TRANSFER Operations (Upload/Download)
    // ===================================

    const downloadFile = useCallback(async (remoteFilePath: string, fileName: string, destinationPath?: string, isDir: boolean = false) => {
        const logId = activityLog.log('DOWNLOAD', t('activity.download_start', { filename: fileName }), 'running');
        try {
            const defaultDownloadPath = await downloadDir();
            // User selects wrapper folder or uses provided one
            const downloadPath = destinationPath || await open({
                directory: true,
                multiple: false,
                defaultPath: defaultDownloadPath
            });

            if (downloadPath) {
                if (isDir) {
                    const folderPath = `${downloadPath}/${fileName}`;
                    const params: DownloadFolderParams = { remote_path: remoteFilePath, local_path: folderPath };
                    await invoke('download_folder', { params });
                    activityLog.updateEntry(logId, { status: 'success', message: t('activity.download_success', { filename: fileName, size: '-', time: '-' }) });
                } else {
                    const params: DownloadParams = { remote_path: remoteFilePath, local_path: `${downloadPath}/${fileName}` };
                    await invoke('download_file', { params });
                    activityLog.updateEntry(logId, { status: 'success', message: t('activity.download_success', { filename: fileName, size: '-', time: '-' }) });
                }
            } else {
                activityLog.updateEntry(logId, { status: 'error', message: t('activity.download_error', { filename: fileName }) });
            }
        } catch (error) {
            activityLog.updateEntry(logId, { status: 'error', message: t('activity.download_error', { filename: fileName }) });
            toast.error('Download Failed', String(error));
        }
    }, [activityLog, toast, t]);

    const uploadFile = useCallback(async (localFilePath: string, fileName: string, isDir: boolean = false) => {
        const logId = activityLog.log('UPLOAD', t('activity.upload_start', { filename: fileName }), 'running');
        try {
            const remotePath = `${currentRemotePath}${currentRemotePath.endsWith('/') ? '' : '/'}${fileName}`;
            if (isDir) {
                const params: UploadFolderParams = { local_path: localFilePath, remote_path: remotePath };
                await invoke('upload_folder', { params });
                activityLog.updateEntry(logId, { status: 'success', message: t('activity.upload_success', { filename: fileName, size: '-', time: '-' }) });
            } else {
                await invoke('upload_file', { params: { local_path: localFilePath, remote_path: remotePath } as UploadParams });
                activityLog.updateEntry(logId, { status: 'success', message: t('activity.upload_success', { filename: fileName, size: '-', time: '-' }) });
            }
        } catch (error) {
            activityLog.updateEntry(logId, { status: 'error', message: t('activity.upload_error', { filename: fileName }) });
            toast.error('Upload Failed', String(error));
        }
    }, [currentRemotePath, activityLog, toast, t]);

    const cancelTransfer = useCallback(async () => {
        try {
            await invoke('cancel_transfer');
            activityLog.log('ERROR', t('transfer.cancelled'), 'error');
        } catch { }
    }, [activityLog, t]);

    return {
        downloadFile,
        uploadFile,
        cancelTransfer
    };
}
