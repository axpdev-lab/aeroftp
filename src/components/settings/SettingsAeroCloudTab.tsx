// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

/**
 * SettingsAeroCloudTab — Full AeroCloud configuration panel.
 * Surfaces all settings that were previously only in CloudPanel settings modal,
 * plus new options (conflict strategy, exclude patterns, sync_on_startup).
 */

import React, { useState, useEffect, useCallback } from 'react';
import {
  Cloud, MonitorCheck, CheckCircle2, AlertCircle, X, FolderSync,
  Play, Pause, RotateCcw, Link2, Eye, EyeOff, Settings, Loader2,
} from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { Checkbox } from '../ui/Checkbox';
import { useTranslation } from '../../i18n';
import { SyncScheduler } from '../SyncScheduler';
import { WatcherStatus } from '../WatcherStatus';

interface CloudConfig {
  enabled: boolean;
  cloud_name: string;
  local_folder: string;
  remote_folder: string;
  server_profile: string;
  sync_interval_secs: number;
  sync_on_change: boolean;
  sync_on_startup: boolean;
  exclude_patterns: string[];
  conflict_strategy: string;
  public_url_base: string | null;
  protocol_type: string;
  last_sync: string | null;
}

interface CloudSyncStatus {
  state: string;
  last_sync?: string;
  error?: string;
  files_synced?: number;
}

interface SettingsAeroCloudTabProps {
  onClose: () => void;
  onOpenCloudPanel?: () => void;
}

export const SettingsAeroCloudTab: React.FC<SettingsAeroCloudTabProps> = ({
  onClose,
  onOpenCloudPanel,
}) => {
  const t = useTranslation();
  const [config, setConfig] = useState<CloudConfig | null>(null);
  const [status, setStatus] = useState<CloudSyncStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [badgeFeedback, setBadgeFeedback] = useState<{ type: 'success' | 'error'; message: string } | null>(null);
  const [excludeText, setExcludeText] = useState('');
  const [syncing, setSyncing] = useState(false);

  // Load config and status on mount
  useEffect(() => {
    const load = async () => {
      try {
        const cfg = await invoke<CloudConfig | null>('get_cloud_config');
        setConfig(cfg);
        if (cfg) {
          setExcludeText((cfg.exclude_patterns || []).join('\n'));
        }
        const st = await invoke<CloudSyncStatus>('get_cloud_status').catch(() => null);
        setStatus(st);
      } catch {
        // AeroCloud not configured
      } finally {
        setLoading(false);
      }
    };
    load();
  }, []);

  const handleSave = useCallback(async () => {
    if (!config) return;
    setSaving(true);
    try {
      // Parse exclude patterns from textarea
      const patterns = excludeText.split('\n').map(l => l.trim()).filter(Boolean);
      const updatedConfig = { ...config, exclude_patterns: patterns };
      await invoke('save_cloud_config_cmd', { config: updatedConfig });
      // Sync interval to SyncSchedule
      try {
        const schedule = await invoke<Record<string, unknown>>('get_sync_schedule_cmd');
        if (schedule) {
          await invoke('save_sync_schedule_cmd', {
            schedule: { ...schedule, interval_secs: updatedConfig.sync_interval_secs }
          });
        }
      } catch { /* scheduler may not be initialized */ }
      setConfig(updatedConfig);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      console.error('Failed to save AeroCloud settings:', e);
    } finally {
      setSaving(false);
    }
  }, [config, excludeText]);

  const handleTriggerSync = useCallback(async () => {
    setSyncing(true);
    try {
      await invoke('trigger_cloud_sync');
      const st = await invoke<CloudSyncStatus>('get_cloud_status').catch(() => null);
      setStatus(st);
    } catch (e) {
      console.error('Sync trigger failed:', e);
    } finally {
      setSyncing(false);
    }
  }, []);

  const handleToggleEnabled = useCallback(async (enabled: boolean) => {
    try {
      const result = await invoke<CloudConfig>('enable_aerocloud', { enabled });
      setConfig(result);
    } catch (e) {
      console.error('Failed to toggle AeroCloud:', e);
    }
  }, []);

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <Loader2 size={20} className="animate-spin text-gray-400" />
      </div>
    );
  }

  // Not configured state
  if (!config) {
    return (
      <div className="space-y-6">
        <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">AeroCloud</h3>
        <div className="p-6 bg-gradient-to-r from-sky-50 to-blue-50 dark:from-sky-900/30 dark:to-blue-900/30 border border-sky-200 dark:border-sky-700 rounded-lg text-center space-y-4">
          <Cloud size={40} className="text-sky-500 mx-auto" />
          <h4 className="font-medium text-lg">{t('settings.aerocloudName')}</h4>
          <p className="text-sm text-gray-600 dark:text-gray-400">{t('settings.aerocloudInfo')}</p>
          <button
            onClick={() => { onClose(); onOpenCloudPanel?.(); }}
            className="px-6 py-2 bg-gradient-to-r from-sky-500 to-blue-600 text-white rounded-lg text-sm font-medium hover:from-sky-600 hover:to-blue-700 transition-all"
          >
            {t('settings.configureAerocloud')} →
          </button>
        </div>
      </div>
    );
  }

  const syncStateLabel = status?.state === 'syncing' ? t('cloud.syncing') || 'Syncing...'
    : status?.state === 'paused' ? t('cloud.paused') || 'Paused'
    : status?.state === 'error' ? t('cloud.error') || 'Error'
    : t('cloud.idle') || 'Idle';

  const syncStateColor = status?.state === 'syncing' ? 'text-cyan-500'
    : status?.state === 'error' ? 'text-red-500'
    : status?.state === 'paused' ? 'text-amber-500'
    : 'text-green-500';

  const isHours = config.sync_interval_secs >= 3600;
  const intervalValue = isHours
    ? Math.round(config.sync_interval_secs / 3600)
    : Math.round(config.sync_interval_secs / 60);

  return (
    <div className="space-y-5">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">AeroCloud</h3>
        <div className="flex items-center gap-2">
          {/* Status badge */}
          <span className={`text-xs font-medium ${syncStateColor}`}>{syncStateLabel}</span>
          {/* Enable/Disable toggle */}
          <button
            onClick={() => handleToggleEnabled(!config.enabled)}
            className={`relative w-10 h-5 rounded-full transition-colors ${config.enabled ? 'bg-cyan-500' : 'bg-gray-300 dark:bg-gray-600'}`}
          >
            <span className={`absolute top-0.5 left-0.5 w-4 h-4 bg-white rounded-full transition-transform shadow ${config.enabled ? 'translate-x-5' : ''}`} />
          </button>
        </div>
      </div>

      {/* Connection Info (readonly) */}
      <div className="grid grid-cols-2 gap-3">
        <div>
          <label className="block text-xs font-medium text-gray-500 mb-1">{t('cloud.cloudName') || 'Cloud Name'}</label>
          <input
            type="text"
            value={config.cloud_name}
            onChange={e => setConfig(prev => prev ? { ...prev, cloud_name: e.target.value } : null)}
            className="w-full px-3 py-1.5 text-sm bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
          />
        </div>
        <div>
          <label className="block text-xs font-medium text-gray-500 mb-1">{t('cloud.protocol') || 'Protocol'}</label>
          <input type="text" value={(config.protocol_type || 'ftp').toUpperCase()} readOnly
            className="w-full px-3 py-1.5 text-sm bg-gray-100 dark:bg-gray-800 rounded-lg text-gray-500" />
        </div>
      </div>

      <div>
        <label className="block text-xs font-medium text-gray-500 mb-1">{t('cloud.localFolder') || 'Local Folder'}</label>
        <input type="text" value={config.local_folder} readOnly
          className="w-full px-3 py-1.5 text-sm bg-gray-100 dark:bg-gray-800 rounded-lg text-gray-500" />
      </div>

      <div className="grid grid-cols-2 gap-3">
        <div>
          <label className="block text-xs font-medium text-gray-500 mb-1">{t('cloud.remoteFolder') || 'Remote Folder'}</label>
          <input
            type="text"
            value={config.remote_folder}
            onChange={e => setConfig(prev => prev ? { ...prev, remote_folder: e.target.value } : null)}
            className="w-full px-3 py-1.5 text-sm bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
            placeholder="/cloud/"
          />
        </div>
        <div>
          <label className="block text-xs font-medium text-gray-500 mb-1">{t('cloud.serverProfile') || 'Server Profile'}</label>
          <input type="text" value={config.server_profile} readOnly
            className="w-full px-3 py-1.5 text-sm bg-gray-100 dark:bg-gray-800 rounded-lg text-gray-500" />
        </div>
      </div>

      {/* Sync Settings */}
      <div className="pt-3 border-t border-gray-200 dark:border-gray-700 space-y-3">
        <h4 className="text-xs font-semibold text-gray-500 uppercase tracking-wide">{t('cloud.syncSettings') || 'Sync Settings'}</h4>

        {/* Sync Interval */}
        <div className="flex items-center gap-3">
          <label className="text-sm text-gray-600 dark:text-gray-400 w-32">{t('cloud.syncInterval') || 'Sync Interval'}</label>
          <input
            type="number" min="1"
            value={intervalValue}
            onChange={e => {
              const val = Math.max(1, parseInt(e.target.value) || 1);
              setConfig(prev => prev ? { ...prev, sync_interval_secs: isHours ? val * 3600 : val * 60 } : null);
            }}
            className="w-20 px-2 py-1.5 text-sm bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-center"
          />
          <select
            value={isHours ? 'hours' : 'minutes'}
            onChange={e => {
              setConfig(prev => prev ? {
                ...prev,
                sync_interval_secs: e.target.value === 'hours' ? intervalValue * 3600 : intervalValue * 60
              } : null);
            }}
            className="px-2 py-1.5 text-sm bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
          >
            <option value="minutes">{t('settings.minutes') || 'minutes'}</option>
            <option value="hours">{t('cloud.hours') || 'hours'}</option>
          </select>
        </div>

        {/* Toggles */}
        <div className="space-y-2">
          <Checkbox
            checked={config.sync_on_change}
            onChange={(v) => setConfig(prev => prev ? { ...prev, sync_on_change: v } : null)}
            label={<span className="text-sm">{t('cloud.syncOnChange') || 'Sync on file changes (real-time watcher)'}</span>}
          />
          <Checkbox
            checked={config.sync_on_startup}
            onChange={(v) => setConfig(prev => prev ? { ...prev, sync_on_startup: v } : null)}
            label={<span className="text-sm">{t('cloud.syncOnStartup') || 'Sync on application startup'}</span>}
          />
        </div>

        {/* Conflict Strategy */}
        <div className="flex items-center gap-3">
          <label className="text-sm text-gray-600 dark:text-gray-400 w-32">{t('cloud.conflictStrategy') || 'Conflicts'}</label>
          <select
            value={config.conflict_strategy || 'ask_user'}
            onChange={e => setConfig(prev => prev ? { ...prev, conflict_strategy: e.target.value } : null)}
            className="flex-1 px-2 py-1.5 text-sm bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
          >
            <option value="ask_user">{t('cloud.conflictAskUser') || 'Ask me'}</option>
            <option value="keep_both">{t('cloud.conflictKeepBoth') || 'Keep both (rename)'}</option>
            <option value="prefer_local">{t('cloud.conflictPreferLocal') || 'Prefer local'}</option>
            <option value="prefer_remote">{t('cloud.conflictPreferRemote') || 'Prefer remote'}</option>
            <option value="prefer_newer">{t('cloud.conflictPreferNewer') || 'Prefer newer'}</option>
          </select>
        </div>
      </div>

      {/* Exclude Patterns */}
      <div className="pt-3 border-t border-gray-200 dark:border-gray-700">
        <label className="block text-xs font-semibold text-gray-500 uppercase tracking-wide mb-1">{t('cloud.excludePatterns') || 'Exclude Patterns'}</label>
        <textarea
          value={excludeText}
          onChange={e => setExcludeText(e.target.value)}
          rows={4}
          className="w-full px-3 py-2 text-xs font-mono bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg resize-y"
          placeholder="node_modules&#10;.git&#10;*.tmp"
        />
        <p className="text-xs text-gray-400 mt-1">{t('cloud.excludePatternsDesc') || 'One pattern per line. Supports glob syntax.'}</p>
      </div>

      {/* Public URL for sharing */}
      <div className="pt-3 border-t border-gray-200 dark:border-gray-700">
        <label className="block text-xs font-medium text-gray-500 mb-1 flex items-center gap-2">
          <Link2 size={12} /> {t('cloud.publicUrlBase') || 'Public URL Base'}
          <span className="text-xs text-gray-400 font-normal">({t('cloud.forSharing') || 'for sharing'})</span>
        </label>
        <input
          type="text"
          value={config.public_url_base || ''}
          onChange={e => setConfig(prev => prev ? { ...prev, public_url_base: e.target.value || null } : null)}
          className="w-full px-3 py-1.5 text-sm bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
          placeholder="https://cloud.yourdomain.com/"
        />
      </div>

      {/* Sync Scheduler */}
      <div className="pt-3 border-t border-gray-200 dark:border-gray-700">
        <SyncScheduler disabled={!config.enabled} />
      </div>

      {/* Watcher Status */}
      <div className="pt-3 border-t border-gray-200 dark:border-gray-700">
        <WatcherStatus watchPath={config.local_folder} />
      </div>

      {/* Last Sync + Actions */}
      <div className="pt-3 border-t border-gray-200 dark:border-gray-700">
        <div className="flex items-center justify-between">
          <div className="text-xs text-gray-400">
            {config.last_sync ? (
              <>{t('cloud.lastSync') || 'Last sync'}: {new Date(config.last_sync).toLocaleString()}</>
            ) : (
              <>{t('cloud.neverSynced') || 'Never synced'}</>
            )}
          </div>
          <div className="flex gap-2">
            <button
              onClick={handleTriggerSync}
              disabled={syncing || !config.enabled}
              className="flex items-center gap-1 px-3 py-1.5 text-xs font-medium bg-cyan-500 hover:bg-cyan-600 text-white rounded-lg disabled:opacity-50 transition-colors"
            >
              {syncing ? <Loader2 size={12} className="animate-spin" /> : <RotateCcw size={12} />}
              {t('cloud.syncNow') || 'Sync Now'}
            </button>
            <button
              onClick={() => { onClose(); onOpenCloudPanel?.(); }}
              className="flex items-center gap-1 px-3 py-1.5 text-xs font-medium border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
            >
              {t('cloud.openDashboard') || 'Dashboard'}
            </button>
          </div>
        </div>
      </div>

      {/* File Manager Badge Integration */}
      <div className="pt-3 border-t border-gray-200 dark:border-gray-700 space-y-2">
        <div className="flex items-center gap-2 text-sm font-medium text-sky-700 dark:text-sky-300">
          <MonitorCheck size={14} />
          {t('settings.fileManagerIntegration')}
        </div>
        {navigator.platform.startsWith('Win') ? (
          <p className="text-xs text-gray-500 dark:text-gray-400">{t('settings.windowsBadgeDesc')}</p>
        ) : (
          <>
            <p className="text-xs text-gray-500 dark:text-gray-400">{t('settings.linuxBadgeDesc')}</p>
            <div className="flex gap-2">
              <button
                onClick={async () => {
                  setBadgeFeedback(null);
                  try {
                    const result = await invoke<string>('install_shell_extension_cmd');
                    setBadgeFeedback({ type: 'success', message: result });
                  } catch (e) {
                    setBadgeFeedback({ type: 'error', message: String(e) });
                  }
                  setTimeout(() => setBadgeFeedback(null), 8000);
                }}
                className="flex-1 py-1.5 bg-emerald-500 hover:bg-emerald-600 text-white rounded text-xs font-medium transition-colors flex items-center justify-center gap-1"
              >
                <CheckCircle2 size={12} />
                {t('settings.installBadges')}
              </button>
              <button
                onClick={async () => {
                  setBadgeFeedback(null);
                  try {
                    const result = await invoke<string>('uninstall_shell_extension_cmd');
                    setBadgeFeedback({ type: 'success', message: result });
                  } catch (e) {
                    setBadgeFeedback({ type: 'error', message: String(e) });
                  }
                  setTimeout(() => setBadgeFeedback(null), 8000);
                }}
                className="py-1.5 px-3 bg-gray-200 hover:bg-gray-300 dark:bg-gray-600 dark:hover:bg-gray-500 text-gray-700 dark:text-gray-200 rounded text-xs font-medium transition-colors"
              >
                {t('settings.uninstallBadges')}
              </button>
            </div>
            {badgeFeedback && (
              <div className={`p-2.5 rounded-lg text-xs space-y-2 ${badgeFeedback.type === 'success'
                ? 'bg-emerald-50 dark:bg-emerald-900/30 border border-emerald-200 dark:border-emerald-700 text-emerald-700 dark:text-emerald-300'
                : 'bg-red-50 dark:bg-red-900/30 border border-red-200 dark:border-red-700 text-red-700 dark:text-red-300'
              }`}>
                <div className="flex items-start gap-2">
                  {badgeFeedback.type === 'success'
                    ? <CheckCircle2 size={14} className="shrink-0 mt-0.5" />
                    : <AlertCircle size={14} className="shrink-0 mt-0.5" />
                  }
                  <span className="leading-relaxed flex-1">{badgeFeedback.message}</span>
                  <button onClick={() => setBadgeFeedback(null)} className="shrink-0 p-0.5 rounded hover:bg-black/10 dark:hover:bg-white/10 transition-colors">
                    <X size={12} />
                  </button>
                </div>
                {badgeFeedback.type === 'success' && (
                  <button
                    onClick={async () => {
                      try {
                        const result = await invoke<string>('restart_file_manager_cmd');
                        setBadgeFeedback({ type: 'success', message: result });
                        setTimeout(() => setBadgeFeedback(null), 5000);
                      } catch (e) {
                        setBadgeFeedback({ type: 'error', message: String(e) });
                      }
                    }}
                    className="w-full py-1.5 bg-sky-500 hover:bg-sky-600 text-white rounded text-xs font-medium transition-colors flex items-center justify-center gap-1.5"
                  >
                    <MonitorCheck size={12} />
                    {t('settings.restartFileManager')}
                  </button>
                )}
              </div>
            )}
          </>
        )}
      </div>

      {/* Save Button */}
      <div className="pt-3 border-t border-gray-200 dark:border-gray-700">
        <button
          onClick={handleSave}
          disabled={saving}
          className="w-full py-2 bg-cyan-500 hover:bg-cyan-600 text-white rounded-lg text-sm font-medium disabled:opacity-50 transition-colors flex items-center justify-center gap-2"
        >
          {saving ? <Loader2 size={14} className="animate-spin" /> : saved ? <CheckCircle2 size={14} /> : null}
          {saved ? (t('common.saved') || 'Saved') : (t('common.save') || 'Save')}
        </button>
      </div>
    </div>
  );
};
