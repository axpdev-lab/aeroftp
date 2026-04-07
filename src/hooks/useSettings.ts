// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

/**
 * useSettings Hook
 * Extracted from App.tsx during modularization (v1.3.1)
 *
 * Manages all application settings persisted in localStorage under 'aeroftp_settings'.
 * Provides live reload via 'storage' and custom 'aeroftp-settings-changed' events.
 *
 * Used by: App.tsx (main consumer), SettingsPanel (writes to localStorage)
 * Dependencies: invoke('toggle_menu_bar') for native menu bar visibility
 *
 * Returns: All settings as individual state values + their setters + SETTINGS_KEY constant
 */

import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { secureGetWithFallback, secureStoreAndClean } from '../utils/secureStorage';

const SETTINGS_KEY = 'aeroftp_settings';
const SETTINGS_VAULT_KEY = 'app_settings';

export const MIN_APP_FONT_SIZE = 10;
export const MAX_APP_FONT_SIZE = 22;
export const DEFAULT_APP_FONT_FAMILY = "'Inter', system-ui, sans-serif";

const LEGACY_FONT_SIZE_MAP: Record<string, number> = {
  small: 13,
  medium: 16,
  large: 18,
};

export const clampAppFontSize = (value: unknown): number => {
  const normalized = typeof value === 'string'
    ? LEGACY_FONT_SIZE_MAP[value] ?? Number(value)
    : Number(value);

  if (!Number.isFinite(normalized)) {
    return LEGACY_FONT_SIZE_MAP.medium;
  }

  return Math.min(MAX_APP_FONT_SIZE, Math.max(MIN_APP_FONT_SIZE, Math.round(normalized)));
};

export const normalizeAppFontFamily = (value: unknown): string => {
  return typeof value === 'string' && value.trim() ? value.trim() : DEFAULT_APP_FONT_FAMILY;
};

export interface AppSettings {
  compactMode: boolean;
  showHiddenFiles: boolean;
  showToastNotifications: boolean;
  confirmBeforeDelete: boolean;
  showStatusBar: boolean;
  defaultLocalPath: string;
  fontSize: number;
  fontFamily: string;
  doubleClickAction: 'preview' | 'download';
  rememberLastFolder: boolean;
  systemMenuVisible: boolean;
  showMenuBar: boolean;
  showActivityLog: boolean;
  showConnectionScreen: boolean;
  debugMode: boolean;
  visibleColumns: string[];
  sortFoldersFirst: boolean;
  showFileExtensions: boolean;
  timeoutSeconds: number;
  maxConcurrentTransfers: number;
  retryCount: number;
  fileExistsAction: 'ask' | 'overwrite' | 'skip' | 'rename' | 'resume' | 'overwrite_if_newer' | 'overwrite_if_different' | 'skip_if_identical';
  swapPanels: boolean;
  lastLocalPath?: string;
  showSystemMenu?: boolean;
}

export const ALL_COLUMNS = ['name', 'size', 'type', 'permissions', 'modified'];

const DEFAULTS: AppSettings = {
  compactMode: false,
  showHiddenFiles: true,
  showToastNotifications: false,
  confirmBeforeDelete: true,
  showStatusBar: true,
  defaultLocalPath: '',
  fontSize: 16,
  fontFamily: DEFAULT_APP_FONT_FAMILY,
  doubleClickAction: 'preview',
  rememberLastFolder: true,
  systemMenuVisible: false,
  showMenuBar: true,
  showActivityLog: false,
  showConnectionScreen: true,
  debugMode: false,
  visibleColumns: ALL_COLUMNS,
  sortFoldersFirst: true,
  showFileExtensions: true,
  timeoutSeconds: 30,
  maxConcurrentTransfers: 5,
  retryCount: 3,
  fileExistsAction: 'ask',
  swapPanels: false,
};

export const useSettings = () => {
  const [compactMode, setCompactMode] = useState(DEFAULTS.compactMode);
  const [showHiddenFiles, setShowHiddenFiles] = useState(DEFAULTS.showHiddenFiles);
  const [showToastNotifications, setShowToastNotifications] = useState(DEFAULTS.showToastNotifications);
  const [confirmBeforeDelete, setConfirmBeforeDelete] = useState(DEFAULTS.confirmBeforeDelete);
  const [showStatusBar, setShowStatusBar] = useState(DEFAULTS.showStatusBar);
  const [defaultLocalPath, setDefaultLocalPath] = useState(DEFAULTS.defaultLocalPath);
  const [fontSize, setFontSize] = useState<number>(DEFAULTS.fontSize);
  const [fontFamily, setFontFamily] = useState(DEFAULTS.fontFamily);
  const [doubleClickAction, setDoubleClickAction] = useState<'preview' | 'download'>(DEFAULTS.doubleClickAction);
  const [rememberLastFolder, setRememberLastFolder] = useState(DEFAULTS.rememberLastFolder);
  const [systemMenuVisible, setSystemMenuVisible] = useState(DEFAULTS.systemMenuVisible);
  const [showMenuBar, setShowMenuBar] = useState(DEFAULTS.showMenuBar);
  const [showActivityLog, setShowActivityLog] = useState(DEFAULTS.showActivityLog);
  const [showConnectionScreen, setShowConnectionScreen] = useState(DEFAULTS.showConnectionScreen);
  const [debugMode, setDebugMode] = useState(DEFAULTS.debugMode);
  const [visibleColumns, setVisibleColumns] = useState<string[]>(DEFAULTS.visibleColumns);
  const [sortFoldersFirst, setSortFoldersFirst] = useState(DEFAULTS.sortFoldersFirst);
  const [showFileExtensions, setShowFileExtensions] = useState(DEFAULTS.showFileExtensions);
  const [timeoutSeconds, setTimeoutSeconds] = useState(DEFAULTS.timeoutSeconds);
  const [maxConcurrentTransfers, setMaxConcurrentTransfers] = useState(DEFAULTS.maxConcurrentTransfers);
  const [retryCount, setRetryCount] = useState(DEFAULTS.retryCount);
  const [fileExistsAction, setFileExistsAction] = useState<AppSettings['fileExistsAction']>(DEFAULTS.fileExistsAction);
  const [swapPanels, setSwapPanels] = useState(DEFAULTS.swapPanels);
  const [showSettingsPanel, setShowSettingsPanel] = useState(false);

  const applySettings = useCallback((parsed: Record<string, unknown>) => {
    if (typeof parsed.compactMode === 'boolean') setCompactMode(parsed.compactMode);
    if (typeof parsed.showHiddenFiles === 'boolean') setShowHiddenFiles(parsed.showHiddenFiles);
    if (typeof parsed.showToastNotifications === 'boolean') setShowToastNotifications(parsed.showToastNotifications);
    if (typeof parsed.confirmBeforeDelete === 'boolean') setConfirmBeforeDelete(parsed.confirmBeforeDelete);
    if (typeof parsed.showStatusBar === 'boolean') setShowStatusBar(parsed.showStatusBar);
    if (typeof parsed.defaultLocalPath === 'string') setDefaultLocalPath(parsed.defaultLocalPath);
    if (typeof parsed.fontSize === 'number' || typeof parsed.fontSize === 'string') {
      setFontSize(clampAppFontSize(parsed.fontSize));
    }
    if ('fontFamily' in parsed) setFontFamily(normalizeAppFontFamily(parsed.fontFamily));
    if (parsed.doubleClickAction && ['preview', 'download'].includes(parsed.doubleClickAction as string)) {
      setDoubleClickAction(parsed.doubleClickAction as 'preview' | 'download');
    }
    if (typeof parsed.rememberLastFolder === 'boolean') setRememberLastFolder(parsed.rememberLastFolder);
    if (typeof parsed.debugMode === 'boolean') setDebugMode(parsed.debugMode);
    if (Array.isArray(parsed.visibleColumns)) setVisibleColumns(parsed.visibleColumns.filter((c: unknown) => typeof c === 'string' && ALL_COLUMNS.includes(c as string)));
    if (typeof parsed.sortFoldersFirst === 'boolean') setSortFoldersFirst(parsed.sortFoldersFirst);
    if (typeof parsed.showFileExtensions === 'boolean') setShowFileExtensions(parsed.showFileExtensions);
    if (typeof parsed.timeoutSeconds === 'number') setTimeoutSeconds(parsed.timeoutSeconds);
    if (typeof parsed.maxConcurrentTransfers === 'number') setMaxConcurrentTransfers(parsed.maxConcurrentTransfers);
    if (typeof parsed.retryCount === 'number') setRetryCount(parsed.retryCount);
    if (
      typeof parsed.fileExistsAction === 'string' &&
      ['ask', 'overwrite', 'skip', 'rename', 'resume', 'overwrite_if_newer', 'overwrite_if_different', 'skip_if_identical'].includes(parsed.fileExistsAction)
    ) {
      setFileExistsAction(parsed.fileExistsAction as AppSettings['fileExistsAction']);
    }
    if (typeof parsed.swapPanels === 'boolean') setSwapPanels(parsed.swapPanels);
  }, []);

  // Load settings on mount + listen for changes
  useEffect(() => {
    const loadSettings = async () => {
      try {
        const parsed = await secureGetWithFallback<Record<string, unknown>>(SETTINGS_VAULT_KEY, SETTINGS_KEY);
        if (parsed) {
          applySettings(parsed);

          // System menu visibility
          const showMenu = typeof parsed.showSystemMenu === 'boolean' ? parsed.showSystemMenu : false;
          setSystemMenuVisible(showMenu);
          invoke('toggle_menu_bar', { visible: showMenu });

          // One-way idempotent migration to vault (no-op if already in vault)
          secureStoreAndClean(SETTINGS_VAULT_KEY, SETTINGS_KEY, parsed).catch(() => {});
        } else {
          // No settings saved, apply defaults for system menu
          invoke('toggle_menu_bar', { visible: false });
        }
      } catch (e) {
        console.error('Failed to init settings', e);
      }
    };

    const handleSettingsChange = (e: Event) => {
      // Use inline payload from CustomEvent for immediate sync (no async vault read)
      const detail = (e as CustomEvent)?.detail as Record<string, unknown> | undefined;
      if (detail) {
        applySettings(detail);
        const showMenu = typeof detail.showSystemMenu === 'boolean' ? detail.showSystemMenu : false;
        setSystemMenuVisible(showMenu);
        return;
      }
      // Fallback: re-read from vault (for storage events or legacy callers)
      void (async () => {
        try {
          const parsed = await secureGetWithFallback<Record<string, unknown>>(SETTINGS_VAULT_KEY, SETTINGS_KEY);
          if (parsed) {
            applySettings(parsed);
            const showMenu = typeof parsed.showSystemMenu === 'boolean' ? parsed.showSystemMenu : false;
            setSystemMenuVisible(showMenu);
          }
        } catch { /* ignore */ }
      })();
    };

    void loadSettings();

    window.addEventListener('storage', handleSettingsChange);
    window.addEventListener('aeroftp-settings-changed', handleSettingsChange);
    return () => {
      window.removeEventListener('storage', handleSettingsChange);
      window.removeEventListener('aeroftp-settings-changed', handleSettingsChange);
    };
  }, [applySettings]);

  return {
    // Settings state
    compactMode,
    showHiddenFiles,
    showToastNotifications,
    confirmBeforeDelete,
    showStatusBar,
    defaultLocalPath,
    fontSize,
    fontFamily,
    doubleClickAction,
    rememberLastFolder,
    systemMenuVisible,
    showMenuBar,
    showActivityLog,
    showConnectionScreen,
    debugMode,
    visibleColumns,
    sortFoldersFirst,
    showFileExtensions,
    timeoutSeconds,
    maxConcurrentTransfers,
    retryCount,
    fileExistsAction,
    swapPanels,
    showSettingsPanel,

    // Setters
    setCompactMode,
    setShowHiddenFiles,
    setShowToastNotifications,
    setConfirmBeforeDelete,
    setShowStatusBar,
    setDefaultLocalPath,
    setFontSize,
    setFontFamily,
    setDoubleClickAction,
    setRememberLastFolder,
    setSystemMenuVisible,
    setShowMenuBar,
    setShowActivityLog,
    setShowConnectionScreen,
    setDebugMode,
    setVisibleColumns,
    setSortFoldersFirst,
    setShowFileExtensions,
    setTimeoutSeconds,
    setMaxConcurrentTransfers,
    setRetryCount,
    setFileExistsAction,
    setSwapPanels,
    setShowSettingsPanel,

    // Constants
    SETTINGS_KEY,
    SETTINGS_VAULT_KEY,
  };
};

export default useSettings;
