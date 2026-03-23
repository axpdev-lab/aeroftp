// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

// useTraySync.ts - Hook for AeroCloud Tray Icon & Background Sync Management
// Modular approach: separates tray/sync logic from CloudPanel UI

import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';

// Sync status types
export type TrayStatus = 'idle' | 'syncing' | 'error' | 'paused' | 'disabled';

export interface TrayState {
    status: TrayStatus;
    tooltip: string;
    lastSync: Date | null;
    isBackgroundSyncActive: boolean;
}

interface CloudSyncStatusEvent {
    status: 'active' | 'idle' | 'error';
    message: string;
}

interface TrayStatusUpdateEvent {
    status: string;
    tooltip: string;
}

/**
 * Custom hook for managing AeroCloud tray icon and background sync
 * 
 * Features:
 * - Start/stop background sync
 * - Listen to sync status events from Rust backend
 * - Manage tray icon state
 * 
 * Usage:
 * const { trayState, startBackgroundSync, stopBackgroundSync, isRunning } = useTraySync();
 */
export function useTraySync() {
    const [trayState, setTrayState] = useState<TrayState>({
        status: 'idle',
        tooltip: 'AeroCloud',
        lastSync: null,
        isBackgroundSyncActive: false,
    });

    const [isRunning, setIsRunning] = useState(false);
    const [error, setError] = useState<string | null>(null);

    // Check initial background sync state
    useEffect(() => {
        const checkRunningState = async () => {
            try {
                const running = await invoke<boolean>('is_background_sync_running');
                setIsRunning(running);
                setTrayState(prev => ({
                    ...prev,
                    isBackgroundSyncActive: running,
                    status: running ? 'syncing' : 'idle',
                }));
            } catch (err) {
                console.error('Failed to check background sync state:', err);
            }
        };
        checkRunningState();
    }, []);

    // Listen to sync status events from Rust backend
    useEffect(() => {
        let unlistenSyncStatus: UnlistenFn | null = null;
        let unlistenTrayStatus: UnlistenFn | null = null;

        const setupListeners = async () => {
            // Listen for cloud sync status updates
            unlistenSyncStatus = await listen<CloudSyncStatusEvent>('cloud-sync-status', (event) => {
                const { status, message } = event.payload;

                setTrayState(prev => ({
                    ...prev,
                    status: status === 'active' ? 'syncing' : status === 'error' ? 'error' : 'idle',
                    tooltip: message || 'AeroCloud',
                    isBackgroundSyncActive: status === 'active',
                }));

                setIsRunning(status === 'active');

                if (status === 'idle') {
                    setTrayState(prev => ({
                        ...prev,
                        lastSync: new Date(),
                    }));
                }
            });

            // Listen for tray status updates
            unlistenTrayStatus = await listen<TrayStatusUpdateEvent>('tray-status-update', (event) => {
                const { status, tooltip } = event.payload;
                setTrayState(prev => ({
                    ...prev,
                    tooltip: tooltip || prev.tooltip,
                }));
            });
        };

        setupListeners();

        return () => {
            if (unlistenSyncStatus) unlistenSyncStatus();
            if (unlistenTrayStatus) unlistenTrayStatus();
        };
    }, []);

    /**
     * Start background sync process
     */
    const startBackgroundSync = useCallback(async (): Promise<string> => {
        try {
            setError(null);
            const result = await invoke<string>('start_background_sync');
            setIsRunning(true);
            setTrayState(prev => ({
                ...prev,
                status: 'syncing',
                isBackgroundSyncActive: true,
                tooltip: 'AeroCloud - Sync Active',
            }));
            return result;
        } catch (err: any) {
            const errorMessage = err?.toString() || 'Failed to start background sync';
            setError(errorMessage);
            setTrayState(prev => ({
                ...prev,
                status: 'error',
                tooltip: `Error: ${errorMessage}`,
            }));
            throw err;
        }
    }, []);

    /**
     * Stop background sync process
     */
    const stopBackgroundSync = useCallback(async (): Promise<string> => {
        try {
            setError(null);
            const result = await invoke<string>('stop_background_sync');
            setIsRunning(false);
            setTrayState(prev => ({
                ...prev,
                status: 'idle',
                isBackgroundSyncActive: false,
                tooltip: 'AeroCloud - Idle',
            }));
            return result;
        } catch (err: any) {
            const errorMessage = err?.toString() || 'Failed to stop background sync';
            setError(errorMessage);
            throw err;
        }
    }, []);

    /**
     * Update tray status manually
     */
    const setTrayStatus = useCallback(async (status: string, tooltip?: string): Promise<void> => {
        try {
            await invoke('set_tray_status', { status, tooltip });
        } catch (err) {
            console.error('Failed to update tray status:', err);
        }
    }, []);

    /**
     * Toggle background sync on/off
     */
    const toggleBackgroundSync = useCallback(async (): Promise<string> => {
        if (isRunning) {
            return await stopBackgroundSync();
        } else {
            return await startBackgroundSync();
        }
    }, [isRunning, startBackgroundSync, stopBackgroundSync]);

    return {
        trayState,
        isRunning,
        error,
        startBackgroundSync,
        stopBackgroundSync,
        toggleBackgroundSync,
        setTrayStatus,
    };
}

export default useTraySync;
