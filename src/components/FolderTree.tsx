import * as React from 'react';
import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ChevronRight, Folder, Loader2, FolderPlus, Copy, PlusCircle } from 'lucide-react';
import { SubDirectory } from '../types/aerofile';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface FolderTreeProps {
  currentPath: string;
  onNavigate: (path: string) => void;
  onAddToSidebar?: (path: string) => void;
  t: (key: string, params?: Record<string, string | number>) => string;
}

interface TreeNodeProps {
  node: SubDirectory;
  level: number;
  currentPath: string;
  expandedPaths: Set<string>;
  childrenCache: Map<string, SubDirectory[]>;
  loadingPaths: Set<string>;
  onNavigate: (path: string) => void;
  onToggleExpand: (path: string) => void;
  onContextMenu: (e: React.MouseEvent, path: string) => void;
}

interface ContextMenuState {
  visible: boolean;
  x: number;
  y: number;
  path: string;
}

// ---------------------------------------------------------------------------
// TreeNode (memoized)
// ---------------------------------------------------------------------------

const TreeNode = React.memo<TreeNodeProps>(({
  node,
  level,
  currentPath,
  expandedPaths,
  childrenCache,
  loadingPaths,
  onNavigate,
  onToggleExpand,
  onContextMenu,
}) => {
  const isExpanded = expandedPaths.has(node.path);
  const isActive = currentPath === node.path;
  const isLoading = loadingPaths.has(node.path);
  const children = childrenCache.get(node.path);

  return (
    <div>
      <div
        className={`flex items-center gap-1 py-0.5 px-1 cursor-pointer text-sm select-none ${
          isActive
            ? 'bg-blue-600/20 text-blue-400'
            : 'text-gray-300 hover:bg-gray-700/50'
        }`}
        style={{ paddingLeft: `${level * 12 + 4}px` }}
        onContextMenu={(e) => onContextMenu(e, node.path)}
      >
        {node.has_children ? (
          <button
            className="flex-shrink-0 p-0 bg-transparent border-none cursor-pointer text-inherit"
            onClick={(e) => { e.stopPropagation(); onToggleExpand(node.path); }}
          >
            {isLoading ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin opacity-50" />
            ) : (
              <ChevronRight
                className={`w-3.5 h-3.5 transition-transform duration-150 ${isExpanded ? 'rotate-90' : ''}`}
              />
            )}
          </button>
        ) : (
          <span className="w-3.5 flex-shrink-0" />
        )}
        <Folder className="w-4 h-4 text-blue-400 flex-shrink-0" />
        <span
          className="truncate"
          onClick={() => onNavigate(node.path)}
          title={node.path}
        >
          {node.name}
        </span>
      </div>

      {isExpanded && children && children.map((child) => (
        <TreeNode
          key={child.path}
          node={child}
          level={level + 1}
          currentPath={currentPath}
          expandedPaths={expandedPaths}
          childrenCache={childrenCache}
          loadingPaths={loadingPaths}
          onNavigate={onNavigate}
          onToggleExpand={onToggleExpand}
          onContextMenu={onContextMenu}
        />
      ))}
    </div>
  );
});

TreeNode.displayName = 'TreeNode';

// ---------------------------------------------------------------------------
// FolderTree (main component)
// ---------------------------------------------------------------------------

export const FolderTree: React.FC<FolderTreeProps> = ({
  currentPath,
  onNavigate,
  onAddToSidebar,
  t,
}) => {
  const [rootEntries, setRootEntries] = useState<SubDirectory[]>([]);
  const [expandedPaths, setExpandedPaths] = useState<Set<string>>(new Set());
  const [childrenCache, setChildrenCache] = useState<Map<string, SubDirectory[]>>(new Map());
  const [loadingPaths, setLoadingPaths] = useState<Set<string>>(new Set());
  const [contextMenu, setContextMenu] = useState<ContextMenuState>({
    visible: false, x: 0, y: 0, path: '',
  });
  const mountedRef = useRef(true);
  const autoExpandedRef = useRef(false);
  const menuRef = useRef<HTMLDivElement>(null);

  // -----------------------------------------------------------------------
  // Fetch children for a given path
  // -----------------------------------------------------------------------

  const fetchChildren = useCallback(async (path: string): Promise<SubDirectory[]> => {
    try {
      const children = await invoke<SubDirectory[]>('list_subdirectories', { path });
      return children.sort((a, b) => a.name.localeCompare(b.name));
    } catch {
      return [];
    }
  }, []);

  // -----------------------------------------------------------------------
  // Load root entries on mount
  // -----------------------------------------------------------------------

  useEffect(() => {
    mountedRef.current = true;
    const loadRoot = async () => {
      const entries = await fetchChildren('/');
      if (mountedRef.current) {
        setRootEntries(entries);
        setChildrenCache((prev) => {
          const next = new Map(prev);
          next.set('/', entries);
          return next;
        });
      }
    };
    loadRoot();
    return () => { mountedRef.current = false; };
  }, [fetchChildren]);

  // -----------------------------------------------------------------------
  // Auto-expand ancestors of currentPath on first load
  // -----------------------------------------------------------------------

  useEffect(() => {
    if (autoExpandedRef.current || rootEntries.length === 0 || !currentPath) return;
    autoExpandedRef.current = true;

    const expandAncestors = async () => {
      // Build list of ancestor paths: /home, /home/user, /home/user/Documents, etc.
      const segments = currentPath.split('/').filter(Boolean);
      const ancestors: string[] = [];
      for (let i = 1; i <= segments.length; i++) {
        ancestors.push('/' + segments.slice(0, i).join('/'));
      }

      const newExpanded = new Set<string>();
      const newCache = new Map<string, SubDirectory[]>(childrenCache);

      for (const ancestor of ancestors) {
        newExpanded.add(ancestor);
        if (!newCache.has(ancestor)) {
          const children = await fetchChildren(ancestor);
          if (!mountedRef.current) return;
          newCache.set(ancestor, children);
        }
      }

      if (mountedRef.current) {
        setExpandedPaths((prev) => new Set([...prev, ...newExpanded]));
        setChildrenCache(newCache);
      }
    };

    expandAncestors();
  }, [currentPath, rootEntries, childrenCache, fetchChildren]);

  // -----------------------------------------------------------------------
  // Toggle expand/collapse a node
  // -----------------------------------------------------------------------

  const handleToggleExpand = useCallback(async (path: string) => {
    setExpandedPaths((prev) => {
      const next = new Set(prev);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });

    // Lazy-load children if not cached
    if (!childrenCache.has(path)) {
      setLoadingPaths((prev) => new Set([...prev, path]));
      const children = await fetchChildren(path);
      if (mountedRef.current) {
        setChildrenCache((prev) => {
          const next = new Map(prev);
          next.set(path, children);
          return next;
        });
        setLoadingPaths((prev) => {
          const next = new Set(prev);
          next.delete(path);
          return next;
        });
      }
    }
  }, [childrenCache, fetchChildren]);

  // -----------------------------------------------------------------------
  // Context menu handlers
  // -----------------------------------------------------------------------

  const handleContextMenu = useCallback((e: React.MouseEvent, path: string) => {
    e.preventDefault();
    e.stopPropagation();
    setContextMenu({ visible: true, x: e.clientX, y: e.clientY, path });
  }, []);

  const closeContextMenu = useCallback(() => {
    setContextMenu((prev) => ({ ...prev, visible: false }));
  }, []);

  // Close on click outside / Escape
  useEffect(() => {
    if (!contextMenu.visible) return;
    const handleClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        closeContextMenu();
      }
    };
    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closeContextMenu();
    };
    document.addEventListener('mousedown', handleClick);
    document.addEventListener('keydown', handleEscape);
    return () => {
      document.removeEventListener('mousedown', handleClick);
      document.removeEventListener('keydown', handleEscape);
    };
  }, [contextMenu.visible, closeContextMenu]);

  const handleCopyPath = useCallback(() => {
    navigator.clipboard.writeText(contextMenu.path);
    closeContextMenu();
  }, [contextMenu.path, closeContextMenu]);

  const handleAddToSidebar = useCallback(() => {
    onAddToSidebar?.(contextMenu.path);
    closeContextMenu();
  }, [contextMenu.path, onAddToSidebar, closeContextMenu]);

  // -----------------------------------------------------------------------
  // Render
  // -----------------------------------------------------------------------

  return (
    <div className="text-gray-300">
      {rootEntries.map((entry) => (
        <TreeNode
          key={entry.path}
          node={entry}
          level={0}
          currentPath={currentPath}
          expandedPaths={expandedPaths}
          childrenCache={childrenCache}
          loadingPaths={loadingPaths}
          onNavigate={onNavigate}
          onToggleExpand={handleToggleExpand}
          onContextMenu={handleContextMenu}
        />
      ))}

      {rootEntries.length === 0 && (
        <div className="flex items-center justify-center py-4 text-gray-500 text-xs">
          <Loader2 className="w-4 h-4 animate-spin mr-2" />
          {t('common.loading')}
        </div>
      )}

      {/* Context menu */}
      {contextMenu.visible && (
        <div
          ref={menuRef}
          className="fixed z-50 bg-gray-800/95 backdrop-blur-lg rounded-lg shadow-2xl border border-gray-700/50 py-1 min-w-[160px]"
          style={{ left: contextMenu.x, top: contextMenu.y }}
        >
          <button
            className="w-full px-3 py-1.5 text-left text-[13px] flex items-center gap-2 text-gray-200 hover:bg-gray-700/80"
            onClick={() => { closeContextMenu(); }}
          >
            <FolderPlus size={14} className="opacity-70" />
            <span>{t('sidebar.new_folder')}</span>
          </button>
          <button
            className="w-full px-3 py-1.5 text-left text-[13px] flex items-center gap-2 text-gray-200 hover:bg-gray-700/80"
            onClick={handleCopyPath}
          >
            <Copy size={14} className="opacity-70" />
            <span>{t('sidebar.copy_path')}</span>
          </button>
          {onAddToSidebar && (
            <button
              className="w-full px-3 py-1.5 text-left text-[13px] flex items-center gap-2 text-gray-200 hover:bg-gray-700/80"
              onClick={handleAddToSidebar}
            >
              <PlusCircle size={14} className="opacity-70" />
              <span>{t('sidebar.add_to_sidebar')}</span>
            </button>
          )}
        </div>
      )}
    </div>
  );
};

export default FolderTree;
