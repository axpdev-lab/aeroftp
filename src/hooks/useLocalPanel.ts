// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useRef, useState } from 'react';
import type { LocalFile } from '../types';
import type { LocalTab } from '../types/aerofile';

export type LocalPanelId = 'local' | 'local2';

interface UseLocalPanelOptions {
  panelId: LocalPanelId;
}

export interface UseLocalPanelReturn {
  panelId: LocalPanelId;

  files: LocalFile[];
  setFiles: React.Dispatch<React.SetStateAction<LocalFile[]>>;
  currentPath: string;
  setCurrentPath: React.Dispatch<React.SetStateAction<string>>;
  selectedFiles: Set<string>;
  setSelectedFiles: React.Dispatch<React.SetStateAction<Set<string>>>;
  lastSelectedIndex: number | null;
  setLastSelectedIndex: React.Dispatch<React.SetStateAction<number | null>>;

  tabs: LocalTab[];
  setTabs: React.Dispatch<React.SetStateAction<LocalTab[]>>;
  activeTabId: string | null;
  setActiveTabId: React.Dispatch<React.SetStateAction<string | null>>;

  searchFilter: string;
  setSearchFilter: React.Dispatch<React.SetStateAction<string>>;
  showSearchBar: boolean;
  setShowSearchBar: React.Dispatch<React.SetStateAction<boolean>>;
  searchRef: React.RefObject<HTMLInputElement>;

  flattenedFiles: LocalFile[] | null;
  setFlattenedFiles: React.Dispatch<React.SetStateAction<LocalFile[] | null>>;
  flattenScanning: boolean;
  setFlattenScanning: React.Dispatch<React.SetStateAction<boolean>>;
  flattenTruncated: boolean;
  setFlattenTruncated: React.Dispatch<React.SetStateAction<boolean>>;
}

const getStoredTabs = (panelId: LocalPanelId): LocalTab[] => {
  try {
    const stored = localStorage.getItem(panelId === 'local2' ? 'aerofile_local_tabs_2' : 'aerofile_local_tabs');
    return stored ? JSON.parse(stored) : [];
  } catch {
    return [];
  }
};

const getStoredActiveTabId = (panelId: LocalPanelId): string | null => {
  try {
    return localStorage.getItem(panelId === 'local2' ? 'aerofile_active_tab_2' : 'aerofile_active_tab') || null;
  } catch {
    return null;
  }
};

export const useLocalPanel = ({ panelId }: UseLocalPanelOptions): UseLocalPanelReturn => {
  const [files, setFiles] = useState<LocalFile[]>([]);
  const [currentPath, setCurrentPath] = useState('');
  const [selectedFiles, setSelectedFiles] = useState<Set<string>>(new Set());
  const [lastSelectedIndex, setLastSelectedIndex] = useState<number | null>(null);

  const [tabs, setTabs] = useState<LocalTab[]>(() => getStoredTabs(panelId));
  const [activeTabId, setActiveTabId] = useState<string | null>(() => getStoredActiveTabId(panelId));

  const [searchFilter, setSearchFilter] = useState('');
  const [showSearchBar, setShowSearchBar] = useState(false);
  const searchRef = useRef<HTMLInputElement>(null);

  const [flattenedFiles, setFlattenedFiles] = useState<LocalFile[] | null>(null);
  const [flattenScanning, setFlattenScanning] = useState(false);
  const [flattenTruncated, setFlattenTruncated] = useState(false);

  return {
    panelId,
    files,
    setFiles,
    currentPath,
    setCurrentPath,
    selectedFiles,
    setSelectedFiles,
    lastSelectedIndex,
    setLastSelectedIndex,
    tabs,
    setTabs,
    activeTabId,
    setActiveTabId,
    searchFilter,
    setSearchFilter,
    showSearchBar,
    setShowSearchBar,
    searchRef,
    flattenedFiles,
    setFlattenedFiles,
    flattenScanning,
    setFlattenScanning,
    flattenTruncated,
    setFlattenTruncated,
  };
};
