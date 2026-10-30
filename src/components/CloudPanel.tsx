// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// AeroCloud Panel - Personal Multi-Protocol Cloud Sync
// Setup wizard, cloud dashboard, and sync controls

import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { useTauriListener } from '../hooks/useTauriListener';
import { TransferProgressBar } from './TransferProgressBar';
import {
    Cloud, CloudOff, RefreshCw,
    Folder, FolderOpen, Settings, Play, Pause, Check, X,
    AlertCircle, Clock, Server,
    Loader2, Shield, History, Link2
} from 'lucide-react';
import { useTraySync } from '../hooks/useTraySync';
import { SyncScheduler } from './SyncScheduler';
import { WatcherStatus } from './WatcherStatus';
import { SelectiveSyncTree } from './Sync/SelectiveSyncTree';
import { VersionBrowser } from './Sync/VersionBrowser';
import { CloudPairsEditor } from './CloudPairsEditor';
import { ConfirmOverlay } from './common/ConfirmOverlay';
import { MODAL_Z } from '../utils/modalLayers';
import { useTranslation } from '../i18n';
import { logger } from '../utils/logger';
import { CloudSetupWizard } from './CloudSetupWizard';
import { AeroCloudMaturityBadge, useAeroCloudCaveat } from './AeroCloudProtocols';
import './CloudPanel.css';

// TypeScript interfaces matching Rust structs
export interface CloudConfig {
    enabled: boolean;
    paused?: boolean;
    cloud_name: string;
    local_folder: string;
    remote_folder: string;
    server_profile: string;
    server_profile_id?: string | null;
    sync_interval_secs: number;
    sync_on_change: boolean;
    sync_on_startup: boolean;
    exclude_patterns: string[];
    last_sync: string | null;
    conflict_strategy: 'ask_user' | 'keep_both' | 'prefer_local' | 'prefer_remote' | 'prefer_newer';
    public_url_base?: string | null;
    protocol_type: string;
    connection_params: Record<string, unknown>;
    excluded_folders: string[];
    versioning_strategy: CloudVersioningStrategy;
    // Folder type (Task 2): matches Rust CompareDirection (snake_case).
    sync_direction?: 'bidirectional' | 'local_to_remote' | 'remote_to_local';
    // Send-only / receive-only additive backup: when true (default) deletes on
    // the read side are not mirrored to the write side.
    preserve_remote_deletes?: boolean;
    update_only?: boolean;
    // Real-time watcher tuning (item 1). Debounce quiet period in ms (default
    // 1500) and cooldown between watcher-triggered syncs in seconds (default 30).
    watcher_debounce_ms?: number;
    watcher_cooldown_secs?: number;
}

type CloudVersioningStrategy =
    | { type: 'disabled' }
    | { type: 'trash_can'; max_age_days?: number }
    | { type: 'simple'; max_copies?: number }
    | { type: 'staggered' };

type CloudVersioningSelectValue =
    | 'disabled'
    | 'trash_can_7'
    | 'trash_can_30'
    | 'trash_can_90'
    | 'simple_5'
    | 'staggered';

function toVersioningSelectValue(strategy?: CloudVersioningStrategy | null): CloudVersioningSelectValue {
    if (!strategy) {
        return 'trash_can_30';
    }

    switch (strategy.type) {
        case 'disabled':
            return 'disabled';
        case 'simple':
            return 'simple_5';
        case 'staggered':
            return 'staggered';
        case 'trash_can': {
            const days = strategy.max_age_days ?? 30;
            if (days === 7) return 'trash_can_7';
            if (days === 90) return 'trash_can_90';
            return 'trash_can_30';
        }
        default:
            return 'trash_can_30';
    }
}

function fromVersioningSelectValue(value: CloudVersioningSelectValue): CloudVersioningStrategy {
    switch (value) {
        case 'disabled':
            return { type: 'disabled' };
        case 'simple_5':
            return { type: 'simple', max_copies: 5 };
        case 'staggered':
            return { type: 'staggered' };
        case 'trash_can_7':
            return { type: 'trash_can', max_age_days: 7 };
        case 'trash_can_90':
            return { type: 'trash_can', max_age_days: 90 };
        case 'trash_can_30':
        default:
            return { type: 'trash_can', max_age_days: 30 };
    }
}

interface CloudSyncStatus {
    type: 'not_configured' | 'idle' | 'syncing' | 'paused' | 'has_conflicts' | 'error';
    last_sync?: string;
    next_sync?: string;
    current_file?: string;
    progress?: number;
    files_done?: number;
    files_total?: number;
    count?: number;
    message?: string;
}

interface SyncResult {
    uploaded: number;
    downloaded: number;
    deleted: number;
    skipped: number;
    conflicts: number;
    errors: string[];
    duration_secs: number;
    file_details?: { path: string; direction: string; size: number }[];
}

interface CloudPanelProps {
    isOpen: boolean;
    onClose: () => void;
    /** Leaves AeroCloud for My Servers, offered by the setup when no profile can be used. */
    onOpenServers?: () => void;
}

// Cloud Dashboard Component
const CloudDashboard: React.FC<{
    config: CloudConfig;
    status: CloudSyncStatus;
    onSyncNow: () => void;
    onPause: () => void;
    onResume: () => void;
    onDisable: () => void;
    onOpenFolder: () => void;
    onSettings: () => void;
    onManagePairs: () => void;
}> = ({ config, status, onSyncNow, onPause, onResume, onDisable, onOpenFolder, onSettings, onManagePairs }) => {
    const t = useTranslation();
    const caveatFor = useAeroCloudCaveat();
    const [countdown, setCountdown] = useState<string>('');

    // Countdown timer effect
    useEffect(() => {
        if (!config.last_sync || status.type === 'syncing' || status.type === 'paused') {
            setCountdown('');
            return;
        }

        const updateCountdown = () => {
            const lastSync = new Date(config.last_sync!).getTime();
            const intervalMs = config.sync_interval_secs * 1000;
            const nextSync = lastSync + intervalMs;
            const now = Date.now();
            const remainingMs = nextSync - now;

            if (remainingMs <= 0) {
                setCountdown(t('cloud.countdownSoon'));
            } else {
                const totalSecs = Math.floor(remainingMs / 1000);
                const hours = Math.floor(totalSecs / 3600);
                const mins = Math.floor((totalSecs % 3600) / 60);
                const secs = totalSecs % 60;
                if (hours > 0) {
                    setCountdown(`${hours}h ${mins.toString().padStart(2, '0')}m`);
                } else {
                    setCountdown(`${mins}:${secs.toString().padStart(2, '0')}`);
                }
            }
        };

        updateCountdown();
        const interval = setInterval(updateCountdown, 1000);
        return () => clearInterval(interval);
    }, [config.last_sync, config.sync_interval_secs, status.type, t]);

    const formatLastSync = (timestamp: string | null) => {
        if (!timestamp) return t('cloud.never');
        const date = new Date(timestamp);
        const now = new Date();
        const diffMs = now.getTime() - date.getTime();
        const diffMins = Math.floor(diffMs / 60000);

        if (diffMins < 1) return t('cloud.justNow');
        if (diffMins < 60) return t('cloud.minutesAgo', { count: diffMins });
        if (diffMins < 1440) return t('cloud.hoursAgo', { count: Math.floor(diffMins / 60) });
        return date.toLocaleDateString();
    };

    const getStatusIcon = () => {
        switch (status.type) {
            case 'syncing': return <RefreshCw className="spin" size={24} />;
            case 'paused': return <Pause size={24} />;
            case 'has_conflicts': return <AlertCircle size={24} />;
            case 'error': return <X size={24} />;
            default: return <Check size={24} />;
        }
    };

    const getStatusText = () => {
        switch (status.type) {
            case 'syncing':
                return `${t('cloud.syncing')}: ${status.current_file || t('cloud.starting')} (${status.progress?.toFixed(0) || 0}%)`;
            case 'paused': return t('cloud.paused');
            case 'has_conflicts': return t('cloud.conflictsCount', { count: status.count ?? 0 });
            case 'error': return status.message || t('cloud.error');
            default: return `${t('cloud.synced')} ${formatLastSync(config.last_sync)}`;
        }
    };

    return (
        <div className="cloud-dashboard">
            <div className="dashboard-header">
                <div className={`status-indicator status-${status.type}`}>
                    {getStatusIcon()}
                </div>
                <div className="status-info">
                    <h2>{config.cloud_name || t('cloud.title')}</h2>
                    <p className="status-text">
                        {getStatusText()}
                        {countdown && <span className="countdown"> • {t('common.next')}: {countdown}</span>}
                    </p>
                </div>
                <button onClick={onSettings} className="btn-icon" title={t('common.settings')}>
                    <Settings size={20} />
                </button>
                <button onClick={onManagePairs} className="btn-icon" title={t('cloud.managePairs') || 'Manage pairs'} style={{ marginLeft: 4 }}>
                    <Server size={20} />
                </button>
            </div>

            {status.type === 'syncing' && (
                <div className="px-3 py-1">
                    <TransferProgressBar
                        percentage={status.progress || 0}
                        currentFile={status.files_done || 0}
                        totalFiles={status.files_total || 0}
                        size="sm"
                        variant="gradient"
                    />
                </div>
            )}

            <div className="dashboard-cards">
                <div className="info-card info-card-clickable" onClick={onOpenFolder} title={t('cloud.openFolder')}>
                    <Folder size={20} />
                    <div>
                        <span className="label">{t('cloud.localFolder')}</span>
                        <span className="value">{config.local_folder} <FolderOpen size={12} className="inline ml-1 opacity-50" /></span>
                    </div>
                </div>

                <div className="info-card">
                    <Server size={20} />
                    <div>
                        <span className="label">{t('cloud.remoteFolder')}</span>
                        <span className="value">{config.remote_folder}</span>
                    </div>
                </div>

                <div className="info-card" title={caveatFor(config.protocol_type)}>
                    <Shield size={20} />
                    <div>
                        <span className="label">{t('cloud.protocolType')}</span>
                        <span className="value">
                            {(config.protocol_type || 'ftp').toUpperCase()} {config.server_profile && `\u00b7 ${config.server_profile}`}
                            <AeroCloudMaturityBadge protocol={config.protocol_type} className="ml-1.5" />
                        </span>
                    </div>
                </div>

                <div className="info-card">
                    <History size={20} />
                    <div>
                        <span className="label">{t('cloud.lastSync')}</span>
                        <span className="value">{formatLastSync(config.last_sync)}</span>
                    </div>
                </div>

                <div className="info-card">
                    <Clock size={20} />
                    <div>
                        <span className="label">{t('cloud.syncInterval')}</span>
                        <span className="value">
                            {config.sync_interval_secs >= 3600
                                ? t(Math.round(config.sync_interval_secs / 3600) === 1 ? 'syncPanel.intervalHoursOne' : 'syncPanel.intervalHoursMany', { count: Math.round(config.sync_interval_secs / 3600) })
                                : t('common.minutesCount', { count: Math.round(config.sync_interval_secs / 60) })
                            }
                        </span>
                    </div>
                </div>
            </div>

            <div className="dashboard-actions">
                <button
                    onClick={onSyncNow}
                    className="px-4 py-2 text-sm rounded-lg bg-blue-500 hover:bg-blue-600 text-white transition-colors flex items-center gap-1.5 disabled:opacity-50 disabled:cursor-not-allowed"
                    disabled={status.type === 'syncing'}
                >
                    <RefreshCw size={16} /> {t('cloud.syncNow')}
                </button>

                {status.type === 'paused' ? (
                    <button onClick={onResume} className="px-4 py-2 text-sm rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors flex items-center gap-1.5 border border-gray-300 dark:border-gray-600">
                        <Play size={16} /> {t('cloud.resume')}
                    </button>
                ) : (
                    <button
                        onClick={onPause}
                        className="px-4 py-2 text-sm rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors flex items-center gap-1.5 border border-gray-300 dark:border-gray-600 disabled:opacity-50 disabled:cursor-not-allowed"
                        disabled={status.type === 'not_configured' || status.type === 'error'}
                    >
                        <Pause size={16} /> {t('cloud.pause')}
                    </button>
                )}

                <button onClick={onDisable} className="px-4 py-2 text-sm rounded-lg text-red-500 hover:bg-red-50 dark:hover:bg-red-500/10 transition-colors flex items-center gap-1.5 border border-red-200 dark:border-red-500/30">
                    <CloudOff size={16} /> {t('cloud.disable')}
                </button>
            </div>
        </div>
    );
};

// Main CloudPanel Component
export const CloudPanel: React.FC<CloudPanelProps> = ({ isOpen, onClose, onOpenServers }) => {
    const t = useTranslation();
    const [config, setConfig] = useState<CloudConfig | null>(null);
    const [status, setStatus] = useState<CloudSyncStatus>({ type: 'not_configured' });
    const [isLoading, setIsLoading] = useState(true);
    const [showSettings, setShowSettings] = useState(false);
    const [showVersionBrowser, setShowVersionBrowser] = useState(false);
    const [showCloudPairs, setShowCloudPairs] = useState(false);
    const [pendingDisableConfirm, setPendingDisableConfirm] = useState(false);
    const [reauthRequired, setReauthRequired] = useState<{ provider: string; message: string } | null>(null);

    // Use modular tray sync hook. Pause/Resume/Disable now go through the
    // dedicated aerocloud commands (pause_aerocloud/resume_aerocloud/disable_aerocloud)
    // so only the start command is still needed here for Sync Now.
    const {
        isRunning: isBackgroundSyncRunning,
        startBackgroundSync,
    } = useTraySync();

    // Load config on mount
    useEffect(() => {
        loadConfig();
    }, []);

    // Listen for status changes: useTauriListener handles late-resolution cleanup
    useTauriListener<CloudSyncStatus>('cloud_status_change', (event) => {
        setStatus(event.payload);
    });

    useTauriListener<SyncResult>('cloud_sync_complete', (event) => {
        const result = event.payload;
        if (result.errors.length > 0) {
            logger.debug(`Sync completed with ${result.errors.length} errors`);
        } else {
            logger.debug(
                `Synced: ↑${result.uploaded} ↓${result.downloaded} files`
            );
        }
    });

    useTauriListener<{ provider: string; reason: string; message: string }>('cloud-reauth-required', (event) => {
        setReauthRequired({ provider: event.payload.provider, message: event.payload.message });
    });

    const loadConfig = async () => {
        setIsLoading(true);
        try {
            const cfg = await invoke<CloudConfig>('get_cloud_config');
            setConfig(cfg);

            if (cfg.enabled) {
                const sts = await invoke<CloudSyncStatus>('get_cloud_status');
                // The status from Rust already has the correct type
                setStatus(sts);
            }
        } catch (error) {
            console.error('Failed to load cloud config:', error);
        } finally {
            setIsLoading(false);
        }
    };

    const handleSetupComplete = (newConfig: CloudConfig) => {
        setConfig(newConfig);
        setStatus({ type: 'idle' });
        // Notify parent that cloud is now active
        emit('cloud-sync-status', { status: 'active', message: t('cloud.aerocloudEnabled') });
        logger.debug('AeroCloud enabled successfully!');
    };

    const handleSyncNow = async () => {
        logger.debug('Starting sync...');
        setStatus({ type: 'syncing', current_file: 'Scanning...', progress: 0 });
        try {
            // Start background sync if not already running
            if (!isBackgroundSyncRunning) {
                await startBackgroundSync();
            }
            const result = await invoke<string>('trigger_cloud_sync');
            logger.debug('Sync result:', result);
            setStatus({ type: 'idle' });
            // Reload config to update last_sync
            loadConfig();
        } catch (error) {
            console.error('Sync failed:', error);
            setStatus({ type: 'error', message: String(error) });
        }
    };

    const handlePause = async () => {
        try {
            const updated = await invoke<CloudConfig>('pause_aerocloud');
            setConfig(updated);
            setStatus({ type: 'paused' });
            logger.debug('Sync paused');
        } catch (error) {
            console.error('Failed to pause sync:', error);
        }
    };

    const handleResume = async () => {
        try {
            const updated = await invoke<CloudConfig>('resume_aerocloud');
            setConfig(updated);
            setStatus({ type: 'idle', last_sync: config?.last_sync || undefined });
            logger.debug('Sync resumed');
        } catch (error) {
            console.error('Failed to resume sync:', error);
        }
    };

    const handleDisable = () => {
        // Disable is destructive: it wipes the AeroCloud configuration and
        // forces the user to run the setup wizard again. Confirm before acting.
        // The `t(...) || '...'` this used to carry protected nothing: `t()`
        // returns the key when it does not resolve, and a key is truthy.
        setPendingDisableConfirm(true);
    };

    const performDisable = async () => {
        setPendingDisableConfirm(false);
        try {
            await invoke('disable_aerocloud');
            setConfig(null);
            setStatus({ type: 'not_configured' });
            logger.debug('AeroCloud disabled');
        } catch (error) {
            logger.error('Failed to disable:', error);
        }
    };

    const handleOpenFolder = async () => {
        if (config?.local_folder) {
            try {
                await invoke('open_in_file_manager', { path: config.local_folder });
            } catch (error) {
                logger.error('Failed to open folder:', error);
            }
        }
    };

    // Don't render if not open
    if (!isOpen) return null;

    if (isLoading) {
        return (
            <div className="fixed inset-0 bg-black/60 z-[9999] flex items-center justify-center backdrop-blur-sm" onClick={onClose} role="dialog" aria-modal="true" aria-label={t('cloud.dialogLoadingLabel')}>
                <div className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl p-8 max-w-2xl w-full mx-4 max-h-[90vh] overflow-auto" onClick={e => e.stopPropagation()}>
                    <div className="flex items-center justify-center gap-3">
                        <Loader2 className="animate-spin" size={32} />
                        <p>{t('common.loading')} {t('cloud.title')}...</p>
                    </div>
                </div>
            </div>
        );
    }

    // Show setup wizard if not configured
    if (!config?.enabled) {
        return (
            <div className="fixed inset-0 bg-black/60 z-[9999] flex items-center justify-center backdrop-blur-sm" onClick={onClose} role="dialog" aria-modal="true" aria-label={t('cloud.dialogSetupLabel')}>
                <div className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl p-6 max-w-xl w-full mx-4 max-h-[90vh] overflow-y-auto" onClick={e => e.stopPropagation()}>
                    <div className="flex items-center justify-between mb-4">
                        <h2 className="text-xl font-semibold flex items-center gap-2"><Cloud className="text-cyan-500" /> {t('cloud.title')} {t('cloud.setup')}</h2>
                        <button onClick={onClose} className="p-2 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg" title={t('common.close')}><X size={20} /></button>
                    </div>
                    <CloudSetupWizard
                        onComplete={handleSetupComplete}
                        onCancel={onClose}
                        onOpenServers={onOpenServers && (() => { onClose(); onOpenServers(); })}
                    />
                </div>
            </div>
        );
    }

    // Show settings panel
    if (showSettings) {
        const syncSecs = config?.sync_interval_secs || 300;
        const isHours = syncSecs >= 3600;
        const syncIntervalValue = isHours ? Math.round(syncSecs / 3600) : Math.round(syncSecs / 60);

        return (
            <div className="fixed inset-0 bg-black/60 z-[9999] flex items-start justify-center backdrop-blur-sm overflow-y-auto py-6" onClick={onClose} role="dialog" aria-modal="true" aria-label={t('cloud.dialogSettingsLabel')}>
                <div className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl max-w-4xl w-full mx-4 my-auto flex flex-col max-h-[90vh]" onClick={e => e.stopPropagation()}>
                    <div className="flex items-center justify-between p-6 pb-4 border-b border-gray-200 dark:border-gray-700">
                        <h2 className="text-xl font-semibold flex items-center gap-2"><Settings className="text-cyan-500" /> {t('cloud.title')} {t('common.settings')}</h2>
                        <button onClick={() => setShowSettings(false)} className="p-2 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg"><X size={20} /></button>
                    </div>

                    <div className="grid grid-cols-1 md:grid-cols-2 gap-6 p-6 overflow-y-auto">
                        {/* LEFT COLUMN: Connection & Location */}
                        <section className="space-y-4">
                            <h3 className="text-xs font-semibold text-gray-500 dark:text-gray-400 uppercase tracking-wide">{t('cloud.sectionConnection')}</h3>

                            <div>
                                <label className="block text-sm font-medium mb-1">{t('cloud.cloudName')}</label>
                                <input
                                    type="text"
                                    value={config?.cloud_name || ''}
                                    onChange={e => setConfig(prev => prev ? { ...prev, cloud_name: e.target.value } : null)}
                                    className="w-full px-3 py-2 bg-white dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                    placeholder={t('cloud.cloudNamePlaceholder')}
                                />
                                <p className="text-xs text-gray-400 mt-1">{t('cloud.cloudNameDesc')}</p>
                            </div>

                            <div>
                                <label className="block text-sm font-medium mb-1">{t('cloud.localFolder')}</label>
                                <input
                                    type="text"
                                    value={config?.local_folder || ''}
                                    readOnly
                                    className="w-full px-3 py-2 bg-gray-100 dark:bg-gray-700 rounded-lg text-sm"
                                />
                            </div>

                            <div>
                                <label className="block text-sm font-medium mb-1">{t('cloud.remoteFolder')}</label>
                                <input
                                    type="text"
                                    value={config?.remote_folder || '/cloud/'}
                                    onChange={e => setConfig(prev => prev ? { ...prev, remote_folder: e.target.value } : null)}
                                    className="w-full px-3 py-2 bg-white dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                    placeholder="/cloud/"
                                />
                            </div>

                            <div>
                                <label className="block text-sm font-medium mb-1">{t('cloud.serverProfile')}</label>
                                <input
                                    type="text"
                                    value={config?.server_profile || ''}
                                    readOnly
                                    className="w-full px-3 py-2 bg-gray-100 dark:bg-gray-700 rounded-lg text-sm"
                                />
                            </div>

                            <div>
                                <label className="block text-sm font-medium mb-1 flex items-center gap-2">
                                    <Link2 size={14} /> {t('cloud.publicUrlBase')}
                                    <span className="text-xs text-gray-400 font-normal">({t('cloud.forSharing')})</span>
                                </label>
                                <input
                                    type="text"
                                    value={config?.public_url_base || ''}
                                    onChange={e => setConfig(prev => prev ? {
                                        ...prev,
                                        public_url_base: e.target.value || null
                                    } : null)}
                                    className="w-full px-3 py-2 bg-white dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                    placeholder="https://cloud.yourdomain.com/"
                                />
                                <p className="text-xs text-gray-400 mt-1">
                                    {t('cloud.publicUrlDesc')}
                                </p>
                            </div>
                        </section>

                        {/* RIGHT COLUMN: Sync & Versioning */}
                        <section className="space-y-4">
                            <h3 className="text-xs font-semibold text-gray-500 dark:text-gray-400 uppercase tracking-wide">{t('cloud.sectionSync')}</h3>

                            <div>
                                <label className="block text-sm font-medium mb-1">{t('cloud.syncInterval')}</label>
                                <div className="flex items-center gap-2">
                                    <input
                                        type="number"
                                        min="1"
                                        max={isHours ? 168 : 1440}
                                        value={syncIntervalValue}
                                        onChange={e => {
                                            const val = Math.max(1, parseInt(e.target.value) || 1);
                                            setConfig(prev => prev ? {
                                                ...prev,
                                                sync_interval_secs: isHours ? val * 3600 : val * 60
                                            } : null);
                                        }}
                                        className="w-20 px-3 py-2 bg-white dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded-lg text-sm text-center"
                                    />
                                    <select
                                        value={isHours ? 'hours' : 'minutes'}
                                        onChange={e => {
                                            setConfig(prev => prev ? {
                                                ...prev,
                                                sync_interval_secs: e.target.value === 'hours' ? syncIntervalValue * 3600 : syncIntervalValue * 60
                                            } : null);
                                        }}
                                        className="px-2 py-2 bg-white dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                    >
                                        <option value="minutes">{t('settings.minutes')}</option>
                                        <option value="hours">{t('cloud.hours')}</option>
                                    </select>
                                </div>
                                <p className="text-xs text-gray-400 mt-1">{t('cloud.syncIntervalDesc')}</p>
                            </div>

                            <div>
                                <label className="block text-sm font-medium mb-1">{t('cloud.watcherDebounce')}</label>
                                <input
                                    type="number"
                                    min="100"
                                    max="60000"
                                    step="100"
                                    value={config?.watcher_debounce_ms ?? 1500}
                                    onChange={e => {
                                        const val = Math.min(60000, Math.max(100, parseInt(e.target.value) || 1500));
                                        setConfig(prev => prev ? { ...prev, watcher_debounce_ms: val } : null);
                                    }}
                                    className="w-24 px-3 py-2 bg-white dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded-lg text-sm text-center"
                                />
                                <p className="text-xs text-gray-400 mt-1">{t('cloud.watcherDebounceDesc')}</p>
                            </div>

                            <div>
                                <label className="block text-sm font-medium mb-1">{t('cloud.watcherCooldown')}</label>
                                <input
                                    type="number"
                                    min="0"
                                    max="3600"
                                    step="5"
                                    value={config?.watcher_cooldown_secs ?? 30}
                                    onChange={e => {
                                        const val = Math.min(3600, Math.max(0, parseInt(e.target.value) || 0));
                                        setConfig(prev => prev ? { ...prev, watcher_cooldown_secs: val } : null);
                                    }}
                                    className="w-24 px-3 py-2 bg-white dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded-lg text-sm text-center"
                                />
                                <p className="text-xs text-gray-400 mt-1">{t('cloud.watcherCooldownDesc')}</p>
                            </div>

                            <div>
                                <label className="block text-sm font-medium mb-2">{t('cloud.syncDirection')}</label>
                                <select
                                    className="w-full px-3 py-2 rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 text-sm"
                                    value={config?.sync_direction || 'bidirectional'}
                                    onChange={(e) => {
                                        setConfig(prev => prev ? { ...prev, sync_direction: e.target.value as CloudConfig['sync_direction'] } : null);
                                    }}
                                >
                                    <option value="bidirectional">{t('cloud.directionBidirectional')}</option>
                                    <option value="local_to_remote">{t('cloud.directionSendOnly')}</option>
                                    <option value="remote_to_local">{t('cloud.directionReceiveOnly')}</option>
                                </select>
                                <p className="text-xs text-gray-400 mt-1">{t('cloud.syncDirectionDesc')}</p>
                                {config?.sync_direction && config.sync_direction !== 'bidirectional' && (
                                    <>
                                        <label className="flex items-center gap-2 mt-2 text-sm">
                                            <input
                                                type="checkbox"
                                                checked={config?.preserve_remote_deletes ?? true}
                                                onChange={(e) => setConfig(prev => prev ? { ...prev, preserve_remote_deletes: e.target.checked } : null)}
                                            />
                                            {t('cloud.preserveRemoteDeletes')}
                                        </label>
                                        <p className="text-xs text-gray-400 mt-1">{t('cloud.preserveRemoteDeletesDesc')}</p>
                                        <label className="flex items-center gap-2 mt-2 text-sm">
                                            <input type="checkbox" checked={config.update_only === true}
                                                onChange={event => setConfig(prev => prev ? { ...prev, update_only: event.target.checked } : null)} />
                                            {t('aerosync.updateOnly')}
                                        </label>
                                        <p className="text-xs text-gray-400 mt-1">{t('cloud.updateOnlyHint')}</p>
                                    </>
                                )}
                            </div>

                            <div>
                                <label className="block text-sm font-medium mb-2">{t('cloud.selectiveSync')}</label>
                                <SelectiveSyncTree
                                    excludedFolders={config?.excluded_folders || []}
                                    onSave={async (folders) => {
                                        try {
                                            await invoke('update_excluded_folders', { excludedFolders: folders });
                                            setConfig(prev => prev ? { ...prev, excluded_folders: folders } : null);
                                        } catch (e) {
                                            console.error('Failed to update excluded folders:', e);
                                        }
                                    }}
                                />
                            </div>

                            <div>
                                <label className="block text-sm font-medium mb-2">{t('cloud.versioningStrategy')}</label>
                                <select
                                    className="w-full px-3 py-2 rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 text-sm"
                                    value={toVersioningSelectValue(config?.versioning_strategy)}
                                    onChange={(e) => {
                                        const nextStrategy = fromVersioningSelectValue(
                                            e.target.value as CloudVersioningSelectValue
                                        );
                                        setConfig(prev => prev ? { ...prev, versioning_strategy: nextStrategy } : null);
                                    }}
                                >
                                    <option value="disabled">{t('cloud.versioningDisabled')}</option>
                                    <option value="trash_can_30">{t('cloud.versioningTrashCan')}</option>
                                    <option value="trash_can_7">{t('cloud.versioningTrashCan7')}</option>
                                    <option value="trash_can_90">{t('cloud.versioningTrashCan90')}</option>
                                    <option value="simple_5">{t('cloud.versioningSimple')}</option>
                                    <option value="staggered">{t('cloud.versioningStaggered')}</option>
                                </select>
                                <p className="text-xs text-gray-400 mt-1">
                                    {t('cloud.versioningDesc')}
                                </p>
                                <button
                                    className="mt-2 text-xs text-blue-500 hover:text-blue-400 flex items-center gap-1"
                                    onClick={() => setShowVersionBrowser(true)}
                                >
                                    <History size={12} /> {t('cloud.browseVersions')}
                                </button>
                            </div>

                            <div className="pt-2 border-t border-gray-200 dark:border-gray-700">
                                <SyncScheduler />
                            </div>

                            <div className="pt-2 border-t border-gray-200 dark:border-gray-700">
                                <WatcherStatus watchPath={config?.local_folder} />
                            </div>
                        </section>
                    </div>

                    {/* Footer: full width */}
                    <div className="flex gap-3 p-6 pt-4 border-t border-gray-200 dark:border-gray-700">
                        <button
                            onClick={async () => {
                                if (config) {
                                    try {
                                        await invoke('save_cloud_config_cmd', { config });
                                        // Sync interval to SyncSchedule so the background worker uses the correct value
                                        try {
                                            const schedule = await invoke<Record<string, unknown>>('get_sync_schedule_cmd');
                                            if (schedule) {
                                                await invoke('save_sync_schedule_cmd', {
                                                    schedule: { ...schedule, interval_secs: config.sync_interval_secs }
                                                });
                                            }
                                        } catch { /* scheduler may not be initialized yet */ }
                                        setShowSettings(false);
                                        logger.debug('Settings saved!');
                                    } catch (e) {
                                        console.error('Failed to save settings:', e);
                                    }
                                }
                            }}
                            className="flex-1 py-2 bg-cyan-500 hover:bg-cyan-600 text-white rounded-lg font-medium"
                        >
                            {t('common.save')} {t('common.settings')}
                        </button>
                        <button
                            onClick={() => setShowSettings(false)}
                            className="flex-1 py-2 bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600 rounded-lg font-medium"
                        >
                            {t('common.cancel')}
                        </button>
                    </div>
                </div>
            </div>
        );
    }

    // Show dashboard
    return (
        <div className="fixed inset-0 bg-black/60 z-[9999] flex items-center justify-center backdrop-blur-sm" onClick={onClose} role="dialog" aria-modal="true" aria-label={t('cloud.dialogDashboardLabel')}>
            <div className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl p-6 max-w-2xl w-full mx-4 max-h-[90vh] overflow-hidden" onClick={e => e.stopPropagation()}>
                <div className="flex items-center justify-between mb-4">
                    <h2 className="text-xl font-semibold flex items-center gap-2"><Cloud className="text-cyan-500" /> {t('cloud.title')}</h2>
                    <button onClick={onClose} className="p-2 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg" title={t('common.close')}><X size={20} /></button>
                </div>
                {reauthRequired && (
                    <div className="mb-4 p-3 rounded-lg bg-red-100 dark:bg-red-900/30 border border-red-300 dark:border-red-700 flex items-center justify-between">
                        <div className="flex items-center gap-2 text-sm text-red-800 dark:text-red-300">
                            <Shield size={16} />
                            <span>{reauthRequired.provider.toUpperCase()}: {t('cloud.reauthRequired')}</span>
                        </div>
                        <button
                            onClick={() => { setReauthRequired(null); setShowSettings(true); }}
                            className="px-3 py-1 text-xs font-medium rounded bg-red-600 text-white hover:bg-red-700"
                        >
                            {t('cloud.reauthorize') || 'Re-authorize'}
                        </button>
                    </div>
                )}
                <CloudDashboard
                    config={config}
                    status={status}
                    onSyncNow={handleSyncNow}
                    onPause={handlePause}
                    onResume={handleResume}
                    onDisable={handleDisable}
                    onOpenFolder={handleOpenFolder}
                    onSettings={() => setShowSettings(true)}
                    onManagePairs={() => setShowCloudPairs(true)}
                />
            </div>
            {showVersionBrowser && (
                <VersionBrowser onClose={() => setShowVersionBrowser(false)} />
            )}
            <CloudPairsEditor
                isOpen={showCloudPairs}
                onClose={() => setShowCloudPairs(false)}
            />
            {pendingDisableConfirm && (
                <ConfirmOverlay
                    message={t('cloud.disableConfirm')}
                    confirmLabel={t('cloud.disable')}
                    onConfirm={() => { void performDisable(); }}
                    onCancel={() => setPendingDisableConfirm(false)}
                    zClass={MODAL_Z.elevatedConfirm}
                />
            )}
        </div>
    );
};

export default CloudPanel;
