// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

/**
 * SettingsAeroCloudTab — AeroCloud configuration panel.
 * Extracted from SettingsPanel.tsx for modularity.
 */

import React, { useState } from 'react';
import { Cloud, MonitorCheck, CheckCircle2, AlertCircle, X } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../../i18n';

interface SettingsAeroCloudTabProps {
  onClose: () => void;
  onOpenCloudPanel?: () => void;
}

export const SettingsAeroCloudTab: React.FC<SettingsAeroCloudTabProps> = ({
  onClose,
  onOpenCloudPanel,
}) => {
  const t = useTranslation();
  const [badgeFeedback, setBadgeFeedback] = useState<{ type: 'success' | 'error'; message: string } | null>(null);

  return (
    <div className="space-y-6">
      <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">AeroCloud</h3>

      {/* Main Card */}
      <div className="p-4 bg-gradient-to-r from-sky-50 to-blue-50 dark:from-sky-900/30 dark:to-blue-900/30 border border-sky-200 dark:border-sky-700 rounded-lg space-y-3">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <div className="w-8 h-8 bg-gradient-to-br from-sky-400 to-blue-500 rounded-lg flex items-center justify-center shadow">
              <Cloud size={16} className="text-white" />
            </div>
            <div>
              <h4 className="font-medium">{t('settings.aerocloudName')}</h4>
              <p className="text-xs text-gray-500">{t('settings.aerocloudDesc')}</p>
            </div>
          </div>
          <span className="text-xs bg-sky-100 dark:bg-sky-800 text-sky-700 dark:text-sky-300 px-2 py-0.5 rounded-full">
            {t('settings.noApiKeysNeeded')}
          </span>
        </div>
        <p className="text-sm text-gray-600 dark:text-gray-400">
          {t('settings.aerocloudInfo')}
        </p>
        <button
          onClick={() => {
            onClose();
            onOpenCloudPanel?.();
          }}
          className="w-full py-2 bg-gradient-to-r from-sky-500 to-blue-600 text-white rounded-lg text-sm font-medium hover:from-sky-600 hover:to-blue-700 transition-all"
        >
          {t('settings.configureAerocloud')} →
        </button>

        {/* File Manager Badge Integration */}
        <div className="pt-3 border-t border-sky-200 dark:border-sky-700 space-y-2">
          <div className="flex items-center gap-2 text-sm font-medium text-sky-700 dark:text-sky-300">
            <MonitorCheck size={14} />
            {t('settings.fileManagerIntegration')}
          </div>
          {navigator.platform.startsWith('Win') ? (
            <p className="text-xs text-gray-500 dark:text-gray-400">
              {t('settings.windowsBadgeDesc')}
            </p>
          ) : (
            <>
              <p className="text-xs text-gray-500 dark:text-gray-400">
                {t('settings.linuxBadgeDesc')}
              </p>
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
                    <button
                      onClick={() => setBadgeFeedback(null)}
                      className="shrink-0 p-0.5 rounded hover:bg-black/10 dark:hover:bg-white/10 transition-colors"
                    >
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
      </div>
    </div>
  );
};
