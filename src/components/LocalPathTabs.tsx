/**
 * LocalPathTabs — tabbed local directory browsing for AeroFile mode.
 *
 * Pattern follows SessionTabs.tsx: drag-to-reorder, context menu,
 * middle-click close, new tab (+) button, max 12 tabs.
 */

import React, { useState, useRef, useCallback, useEffect } from 'react';
import { X, Plus, Folder } from 'lucide-react';
import type { LocalTab } from '../types/aerofile';
import { useTranslation } from '../i18n';

// ============================================================================
// Props
// ============================================================================

interface LocalPathTabsProps {
  tabs: LocalTab[];
  activeTabId: string | null;
  onTabClick: (tabId: string) => void;
  onTabClose: (tabId: string) => void;
  onNewTab: () => void;
  onReorder: (tabs: LocalTab[]) => void;
  maxTabs?: number;
}

// ============================================================================
// Component
// ============================================================================

export const LocalPathTabs: React.FC<LocalPathTabsProps> = ({
  tabs,
  activeTabId,
  onTabClick,
  onTabClose,
  onNewTab,
  onReorder,
  maxTabs = 12,
}) => {
  const t = useTranslation();

  // Drag-to-reorder state
  const [dragIdx, setDragIdx] = useState<number | null>(null);
  const [overIdx, setOverIdx] = useState<number | null>(null);
  const dragNodeRef = useRef<HTMLDivElement | null>(null);

  // Context menu state
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; tabId: string } | null>(null);
  const contextMenuRef = useRef<HTMLDivElement | null>(null);

  // Close context menu on outside click
  useEffect(() => {
    if (!contextMenu) return;
    const handleClick = (e: MouseEvent) => {
      if (contextMenuRef.current && !contextMenuRef.current.contains(e.target as Node)) {
        setContextMenu(null);
      }
    };
    document.addEventListener('mousedown', handleClick);
    return () => document.removeEventListener('mousedown', handleClick);
  }, [contextMenu]);

  // Drag handlers
  const handleDragStart = useCallback((e: React.DragEvent<HTMLDivElement>, idx: number) => {
    setDragIdx(idx);
    dragNodeRef.current = e.currentTarget;
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('application/x-local-tab', String(idx));
    requestAnimationFrame(() => {
      if (dragNodeRef.current) dragNodeRef.current.style.opacity = '0.4';
    });
  }, []);

  const handleDragOver = useCallback((e: React.DragEvent<HTMLDivElement>, idx: number) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    if (dragIdx === null || idx === dragIdx) return;
    setOverIdx(idx);
  }, [dragIdx]);

  const handleDrop = useCallback((e: React.DragEvent<HTMLDivElement>, idx: number) => {
    e.preventDefault();
    if (dragIdx === null || dragIdx === idx) return;
    const reordered = [...tabs];
    const [moved] = reordered.splice(dragIdx, 1);
    reordered.splice(idx, 0, moved);
    onReorder(reordered);
  }, [dragIdx, tabs, onReorder]);

  const handleDragEnd = useCallback(() => {
    if (dragNodeRef.current) dragNodeRef.current.style.opacity = '1';
    dragNodeRef.current = null;
    setDragIdx(null);
    setOverIdx(null);
  }, []);

  // Context menu actions
  const handleCloseTab = useCallback((tabId: string) => {
    setContextMenu(null);
    onTabClose(tabId);
  }, [onTabClose]);

  const handleCloseOthers = useCallback((tabId: string) => {
    setContextMenu(null);
    tabs.forEach(tab => {
      if (tab.id !== tabId) onTabClose(tab.id);
    });
  }, [tabs, onTabClose]);

  const handleCloseAll = useCallback(() => {
    setContextMenu(null);
    tabs.forEach(tab => onTabClose(tab.id));
  }, [tabs, onTabClose]);

  if (tabs.length === 0) return null;

  return (
    <div className="flex items-center gap-1 px-3 py-1 bg-gray-50 dark:bg-gray-800/50 border-b border-gray-200 dark:border-gray-700 overflow-x-auto">
      {tabs.map((tab, idx) => {
        const isActive = tab.id === activeTabId;
        const isDragTarget = overIdx === idx && dragIdx !== null && dragIdx !== idx;

        return (
          <div
            key={tab.id}
            draggable
            onDragStart={(e) => handleDragStart(e, idx)}
            onDragOver={(e) => handleDragOver(e, idx)}
            onDrop={(e) => handleDrop(e, idx)}
            onDragEnd={handleDragEnd}
            className={`group flex items-center gap-1.5 pl-2.5 pr-1.5 py-1 rounded-md cursor-pointer transition-all min-w-0 max-w-[180px] ${
              isActive
                ? 'bg-white dark:bg-gray-700 shadow-sm border border-gray-200 dark:border-gray-600'
                : 'hover:bg-gray-200 dark:hover:bg-gray-700/50'
            } ${dragIdx === idx ? 'scale-95' : ''} ${isDragTarget ? 'border-l-2 border-blue-500' : ''}`}
            onClick={() => onTabClick(tab.id)}
            onAuxClick={(e) => {
              if (e.button === 1) { // Middle click
                e.preventDefault();
                onTabClose(tab.id);
              }
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              setContextMenu({ x: e.clientX, y: e.clientY, tabId: tab.id });
            }}
            title={tab.path}
          >
            <Folder size={12} className={`shrink-0 ${isActive ? 'text-blue-500' : 'text-gray-400'}`} />
            <span className={`truncate text-xs ${isActive ? 'font-medium text-gray-800 dark:text-gray-200' : 'text-gray-500 dark:text-gray-400'}`}>
              {tab.label || '/'}
            </span>
            {/* Close button */}
            <button
              onClick={(e) => {
                e.stopPropagation();
                onTabClose(tab.id);
              }}
              className="shrink-0 p-0.5 rounded opacity-0 group-hover:opacity-100 hover:bg-gray-300 dark:hover:bg-gray-600 transition-opacity"
              title={t('localTabs.closeTab')}
            >
              <X size={10} />
            </button>
          </div>
        );
      })}

      {/* New tab button */}
      {tabs.length < maxTabs && (
        <button
          onClick={onNewTab}
          className="shrink-0 p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors"
          title={t('localTabs.newTab')}
        >
          <Plus size={13} />
        </button>
      )}

      {/* Context menu */}
      {contextMenu && (
        <div
          ref={contextMenuRef}
          className="fixed z-[9999] bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-xl py-1 min-w-[160px]"
          style={{ left: contextMenu.x, top: contextMenu.y }}
        >
          <button
            className="w-full text-left px-3 py-1.5 text-xs hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-700 dark:text-gray-300"
            onClick={() => handleCloseTab(contextMenu.tabId)}
          >
            {t('localTabs.closeTab')}
          </button>
          {tabs.length > 1 && (
            <button
              className="w-full text-left px-3 py-1.5 text-xs hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-700 dark:text-gray-300"
              onClick={() => handleCloseOthers(contextMenu.tabId)}
            >
              {t('localTabs.closeOthers')}
            </button>
          )}
          <button
            className="w-full text-left px-3 py-1.5 text-xs hover:bg-gray-100 dark:hover:bg-gray-700 text-red-500"
            onClick={handleCloseAll}
          >
            {t('localTabs.closeAll')}
          </button>
        </div>
      )}
    </div>
  );
};

export default LocalPathTabs;
