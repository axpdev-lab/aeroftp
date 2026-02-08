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

const SETTINGS_KEY = 'aeroftp_settings';

export interface AppSettings {
  compactMode: boolean;
  showHiddenFiles: boolean;
  showToastNotifications: boolean;
  confirmBeforeDelete: boolean;
  showStatusBar: boolean;
  defaultLocalPath: string;
  fontSize: 'small' | 'medium' | 'large';
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
}

export const ALL_COLUMNS = ['name', 'size', 'type', 'permissions', 'modified'];

const DEFAULTS: AppSettings = {
  compactMode: false,
  showHiddenFiles: true,
  showToastNotifications: true,
  confirmBeforeDelete: true,
  showStatusBar: true,
  defaultLocalPath: '',
  fontSize: 'medium',
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
};

export const useSettings = () => {
  const [compactMode, setCompactMode] = useState(DEFAULTS.compactMode);
  const [showHiddenFiles, setShowHiddenFiles] = useState(DEFAULTS.showHiddenFiles);
  const [showToastNotifications, setShowToastNotifications] = useState(DEFAULTS.showToastNotifications);
  const [confirmBeforeDelete, setConfirmBeforeDelete] = useState(DEFAULTS.confirmBeforeDelete);
  const [showStatusBar, setShowStatusBar] = useState(DEFAULTS.showStatusBar);
  const [defaultLocalPath, setDefaultLocalPath] = useState(DEFAULTS.defaultLocalPath);
  const [fontSize, setFontSize] = useState<'small' | 'medium' | 'large'>(DEFAULTS.fontSize);
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
  const [showSettingsPanel, setShowSettingsPanel] = useState(false);

  const applySettings = useCallback((parsed: Record<string, unknown>) => {
    if (typeof parsed.compactMode === 'boolean') setCompactMode(parsed.compactMode);
    if (typeof parsed.showHiddenFiles === 'boolean') setShowHiddenFiles(parsed.showHiddenFiles);
    if (typeof parsed.showToastNotifications === 'boolean') setShowToastNotifications(parsed.showToastNotifications);
    if (typeof parsed.confirmBeforeDelete === 'boolean') setConfirmBeforeDelete(parsed.confirmBeforeDelete);
    if (typeof parsed.showStatusBar === 'boolean') setShowStatusBar(parsed.showStatusBar);
    if (typeof parsed.defaultLocalPath === 'string') setDefaultLocalPath(parsed.defaultLocalPath);
    if (parsed.fontSize && ['small', 'medium', 'large'].includes(parsed.fontSize as string)) {
      setFontSize(parsed.fontSize as 'small' | 'medium' | 'large');
    }
    if (parsed.doubleClickAction && ['preview', 'download'].includes(parsed.doubleClickAction as string)) {
      setDoubleClickAction(parsed.doubleClickAction as 'preview' | 'download');
    }
    if (typeof parsed.rememberLastFolder === 'boolean') setRememberLastFolder(parsed.rememberLastFolder);
    if (typeof parsed.debugMode === 'boolean') setDebugMode(parsed.debugMode);
    if (Array.isArray(parsed.visibleColumns)) setVisibleColumns(parsed.visibleColumns.filter((c: unknown) => typeof c === 'string' && ALL_COLUMNS.includes(c as string)));
    if (typeof parsed.sortFoldersFirst === 'boolean') setSortFoldersFirst(parsed.sortFoldersFirst);
    if (typeof parsed.showFileExtensions === 'boolean') setShowFileExtensions(parsed.showFileExtensions);
  }, []);

  // Load settings on mount + listen for changes
  useEffect(() => {
    try {
      const savedSettings = localStorage.getItem(SETTINGS_KEY);
      if (savedSettings) {
        const parsed = JSON.parse(savedSettings);
        applySettings(parsed);

        // System menu visibility
        const showMenu = typeof parsed.showSystemMenu === 'boolean' ? parsed.showSystemMenu : false;
        setSystemMenuVisible(showMenu);
        invoke('toggle_menu_bar', { visible: showMenu });
      } else {
        // No settings saved, apply defaults for system menu
        invoke('toggle_menu_bar', { visible: false });
      }
    } catch (e) {
      console.error('Failed to init settings', e);
    }

    const handleSettingsChange = () => {
      try {
        const saved = localStorage.getItem(SETTINGS_KEY);
        if (saved) {
          applySettings(JSON.parse(saved));
        }
      } catch { /* ignore */ }
    };

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
    showSettingsPanel,

    // Setters
    setCompactMode,
    setShowHiddenFiles,
    setShowToastNotifications,
    setConfirmBeforeDelete,
    setShowStatusBar,
    setDefaultLocalPath,
    setFontSize,
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
    setShowSettingsPanel,

    // Constants
    SETTINGS_KEY,
  };
};

export default useSettings;
