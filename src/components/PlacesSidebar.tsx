import * as React from 'react';
import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import {
  Home, Monitor, FileText, Image, Music, Download, Video,
  Trash2, Folder, HardDrive, Usb, Disc, Globe,
  LayoutList, FolderTree as FolderTreeIcon, ChevronDown, ChevronRight,
  Plus, X, Power, Loader2, Clock, Play,
  type LucideIcon,
} from 'lucide-react';
import { UserDirectory, VolumeInfo, UnmountedPartition, SidebarMode, LabelCount } from '../types/aerofile';
import { FolderTree } from './FolderTree';
import { formatBytes } from '../utils/formatters';

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const SIDEBAR_MODE_KEY = 'aerofile_sidebar_mode';
const CUSTOM_LOCATIONS_KEY = 'aerofile_custom_locations';
const VOLUME_POLL_FALLBACK_MS = 30000; // Fallback polling for macOS/Windows (watcher handles Linux)

/** Map icon name strings (from Rust) to Lucide components */
const iconMap: Record<string, LucideIcon> = {
  Home,
  Monitor,
  FileText,
  Image,
  Music,
  Download,
  Video,
  Trash2,
  Folder,
  HardDrive,
  Usb,
  Disc,
  Globe,
};

/** Map volume_type to Lucide icon */
const volumeIcon: Record<string, LucideIcon> = {
  internal: HardDrive,
  removable: Usb,
  network: Globe,
  optical: Disc,
};

// ---------------------------------------------------------------------------
// Props
// ---------------------------------------------------------------------------

interface PlacesSidebarProps {
  currentPath: string;
  onNavigate: (path: string) => void;
  t: (key: string, params?: Record<string, string | number>) => string;
  recentPaths?: string[];
  onClearRecent?: () => void;
  onRemoveRecent?: (path: string) => void;
  isTrashView?: boolean;
  onNavigateTrash?: () => void;
  // Tags
  labelCounts?: LabelCount[];
  activeTagFilter?: number | null;
  onTagFilter?: (labelId: number | null) => void;
}

// ---------------------------------------------------------------------------
// Sub-component: sidebar item row
// ---------------------------------------------------------------------------

interface SidebarItemProps {
  icon: React.ReactNode;
  label: string;
  path: string;
  currentPath: string;
  tooltip?: string;
  onNavigate: (path: string) => void;
  onContextMenu?: (e: React.MouseEvent) => void;
}

const SidebarItem: React.FC<SidebarItemProps> = React.memo(({
  icon, label, path, currentPath, tooltip, onNavigate, onContextMenu,
}) => {
  const isActive = currentPath === path;
  return (
    <button
      aria-current={isActive ? 'page' : undefined}
      className={`flex items-center gap-2 px-3 py-1.5 text-sm cursor-pointer rounded-md mx-1 w-[calc(100%-8px)] text-left transition-colors duration-100 ${
        isActive
          ? 'bg-blue-100 text-blue-600 dark:bg-blue-600/20 dark:text-blue-400'
          : 'text-gray-700 hover:bg-gray-100 dark:text-gray-300 dark:hover:bg-gray-700/50'
      }`}
      onClick={() => onNavigate(path)}
      onContextMenu={onContextMenu}
      title={tooltip ?? path}
    >
      {icon}
      <span className="truncate">{label}</span>
    </button>
  );
});

SidebarItem.displayName = 'SidebarItem';

// ---------------------------------------------------------------------------
// Sub-component: disk usage bar
// ---------------------------------------------------------------------------

const DiskUsageBar: React.FC<{ usedPercent: number }> = React.memo(({ usedPercent }) => {
  const color =
    usedPercent >= 90 ? 'bg-red-500' :
    usedPercent >= 70 ? 'bg-yellow-500' :
    'bg-green-500';

  return (
    <div className="w-full h-1 rounded-full bg-gray-200 dark:bg-gray-700 mt-0.5">
      <div
        className={`h-full rounded-full ${color} transition-all duration-300`}
        style={{ width: `${Math.min(usedPercent, 100)}%` }}
      />
    </div>
  );
});

DiskUsageBar.displayName = 'DiskUsageBar';

// ---------------------------------------------------------------------------
// Context menu for custom locations
// ---------------------------------------------------------------------------

interface RemoveMenuState {
  visible: boolean;
  x: number;
  y: number;
  index: number;
}

// ---------------------------------------------------------------------------
// PlacesSidebar (main component)
// ---------------------------------------------------------------------------

export const PlacesSidebar: React.FC<PlacesSidebarProps> = ({
  currentPath,
  onNavigate,
  t,
  recentPaths = [],
  onClearRecent,
  onRemoveRecent,
  isTrashView = false,
  onNavigateTrash,
  labelCounts = [],
  activeTagFilter = null,
  onTagFilter,
}) => {
  // -----------------------------------------------------------------------
  // State
  // -----------------------------------------------------------------------

  const [sidebarMode, setSidebarMode] = useState<SidebarMode>(() => {
    const stored = localStorage.getItem(SIDEBAR_MODE_KEY);
    return stored === 'tree' ? 'tree' : 'places';
  });

  const [userDirs, setUserDirs] = useState<UserDirectory[]>([]);
  const [customLocations, setCustomLocations] = useState<string[]>(() => {
    try {
      const stored = localStorage.getItem(CUSTOM_LOCATIONS_KEY);
      return stored ? JSON.parse(stored) as string[] : [];
    } catch {
      return [];
    }
  });

  const [showVolumes, setShowVolumes] = useState(false);
  const [volumes, setVolumes] = useState<VolumeInfo[]>([]);
  const [unmountedPartitions, setUnmountedPartitions] = useState<UnmountedPartition[]>([]);
  const [volumesLoading, setVolumesLoading] = useState(false);
  const [ejectingMount, setEjectingMount] = useState<string | null>(null);
  const [mountingDevice, setMountingDevice] = useState<string | null>(null);

  // Context menu for removing custom locations
  const [removeMenu, setRemoveMenu] = useState<RemoveMenuState>({
    visible: false, x: 0, y: 0, index: -1,
  });
  const removeMenuRef = useRef<HTMLDivElement>(null);
  const volumeIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const mountedRef = useRef(true);

  // -----------------------------------------------------------------------
  // Persist sidebar mode
  // -----------------------------------------------------------------------

  useEffect(() => {
    localStorage.setItem(SIDEBAR_MODE_KEY, sidebarMode);
  }, [sidebarMode]);

  // -----------------------------------------------------------------------
  // Persist custom locations
  // -----------------------------------------------------------------------

  useEffect(() => {
    localStorage.setItem(CUSTOM_LOCATIONS_KEY, JSON.stringify(customLocations));
  }, [customLocations]);

  // -----------------------------------------------------------------------
  // Fetch user directories on mount + global cleanup on unmount
  // -----------------------------------------------------------------------

  useEffect(() => {
    mountedRef.current = true;
    const load = async () => {
      try {
        const dirs = await invoke<UserDirectory[]>('get_user_directories');
        if (mountedRef.current) setUserDirs(dirs);
      } catch {
        // Backend command not available yet — silently ignore
      }
    };
    load();
    return () => {
      mountedRef.current = false;
      // Defense-in-depth: clear volume polling interval on unmount
      if (volumeIntervalRef.current) {
        clearInterval(volumeIntervalRef.current);
        volumeIntervalRef.current = null;
      }
    };
  }, []);

  // -----------------------------------------------------------------------
  // Fetch volumes when expanded + change-detection polling (#113)
  // -----------------------------------------------------------------------

  const fetchVolumes = useCallback(async () => {
    try {
      const [vols, unmounted] = await Promise.all([
        invoke<VolumeInfo[]>('list_mounted_volumes'),
        invoke<UnmountedPartition[]>('list_unmounted_partitions').catch(() => [] as UnmountedPartition[]),
      ]);
      if (mountedRef.current) {
        setVolumes(vols);
        setUnmountedPartitions(unmounted);
      }
    } catch {
      // Backend command not available yet
    }
  }, []);

  useEffect(() => {
    if (!showVolumes) return;

    setVolumesLoading(true);
    fetchVolumes().finally(() => {
      if (mountedRef.current) setVolumesLoading(false);
    });

    // Event-driven: backend mount watcher (poll + inotify on Linux) emits
    // 'volumes-changed' immediately when /proc/mounts or GVFS dir changes.
    // Fallback polling at 30s for macOS/Windows where no watcher exists.
    let unlisten: UnlistenFn | null = null;

    listen<void>('volumes-changed', () => {
      if (mountedRef.current) fetchVolumes();
    }).then((fn) => { unlisten = fn; });

    // Fallback poll for non-Linux platforms (watcher is Linux-only)
    volumeIntervalRef.current = setInterval(async () => {
      try {
        const changed = await invoke<boolean>('volumes_changed');
        if (changed && mountedRef.current) fetchVolumes();
      } catch {
        if (mountedRef.current) fetchVolumes();
      }
    }, VOLUME_POLL_FALLBACK_MS);

    return () => {
      if (unlisten) unlisten();
      if (volumeIntervalRef.current) {
        clearInterval(volumeIntervalRef.current);
        volumeIntervalRef.current = null;
      }
    };
  }, [showVolumes, fetchVolumes]);

  // -----------------------------------------------------------------------
  // Eject volume
  // -----------------------------------------------------------------------

  const handleEject = useCallback(async (mountPoint: string, e: React.MouseEvent) => {
    e.stopPropagation();
    setEjectingMount(mountPoint);
    try {
      await invoke('eject_volume', { mountPoint });
      // Refresh volumes after eject
      await fetchVolumes();
    } catch {
      // Eject failed — volume may be busy
    } finally {
      if (mountedRef.current) setEjectingMount(null);
    }
  }, [fetchVolumes]);

  // -----------------------------------------------------------------------
  // Mount unmounted partition
  // -----------------------------------------------------------------------

  const handleMount = useCallback(async (device: string, e: React.MouseEvent) => {
    e.stopPropagation();
    setMountingDevice(device);
    try {
      const mountPoint = await invoke<string>('mount_partition', { device });
      await fetchVolumes();
      if (mountPoint) onNavigate(mountPoint);
    } catch {
      // Mount failed — permission denied or busy
    } finally {
      if (mountedRef.current) setMountingDevice(null);
    }
  }, [fetchVolumes, onNavigate]);

  // -----------------------------------------------------------------------
  // Custom location management
  // -----------------------------------------------------------------------

  const addCustomLocation = useCallback((path: string) => {
    setCustomLocations((prev) => {
      if (prev.includes(path)) return prev;
      return [...prev, path];
    });
  }, []);

  const removeCustomLocation = useCallback((index: number) => {
    setCustomLocations((prev) => prev.filter((_, i) => i !== index));
  }, []);

  // -----------------------------------------------------------------------
  // Remove context menu handlers
  // -----------------------------------------------------------------------

  const handleCustomLocationContextMenu = useCallback((e: React.MouseEvent, index: number) => {
    e.preventDefault();
    e.stopPropagation();
    setRemoveMenu({ visible: true, x: e.clientX, y: e.clientY, index });
  }, []);

  const closeRemoveMenu = useCallback(() => {
    setRemoveMenu((prev) => ({ ...prev, visible: false }));
  }, []);

  useEffect(() => {
    if (!removeMenu.visible) return;
    const handleClick = (e: MouseEvent) => {
      if (removeMenuRef.current && !removeMenuRef.current.contains(e.target as Node)) {
        closeRemoveMenu();
      }
    };
    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closeRemoveMenu();
    };
    document.addEventListener('mousedown', handleClick);
    document.addEventListener('keydown', handleEscape);
    return () => {
      document.removeEventListener('mousedown', handleClick);
      document.removeEventListener('keydown', handleEscape);
    };
  }, [removeMenu.visible, closeRemoveMenu]);

  // -----------------------------------------------------------------------
  // Trash — uses the cross-platform trash view mechanism via onNavigateTrash
  // No hardcoded path; the backend handles platform-specific trash locations.
  // -----------------------------------------------------------------------

  // -----------------------------------------------------------------------
  // Render helpers
  // -----------------------------------------------------------------------

  const renderIcon = useCallback((iconName: string, className?: string): React.ReactNode => {
    const IconComp = iconMap[iconName] ?? Folder;
    return <IconComp size={16} className={className ?? 'opacity-70 flex-shrink-0'} />;
  }, []);

  const renderVolumeIcon = useCallback((volumeType: string): React.ReactNode => {
    const IconComp = volumeIcon[volumeType] ?? HardDrive;
    return <IconComp size={16} className="opacity-70 flex-shrink-0" />;
  }, []);

  const basename = useCallback((path: string) => {
    const parts = path.replace(/\/+$/, '').split('/');
    return parts[parts.length - 1] || path;
  }, []);

  // -----------------------------------------------------------------------
  // Render: Places mode content
  // -----------------------------------------------------------------------

  const renderPlacesContent = () => (
    <>
      {/* User Directories */}
      <div className="py-1">
        {userDirs.map((dir) => (
          <SidebarItem
            key={dir.key}
            icon={renderIcon(dir.icon)}
            label={t(`sidebar.${dir.key}`)}
            path={dir.path}
            currentPath={currentPath}
            onNavigate={onNavigate}
          />
        ))}
      </div>

      {/* Trash */}
      <div className="py-0.5">
        <button
          aria-current={isTrashView ? 'page' : undefined}
          onClick={() => onNavigateTrash ? onNavigateTrash() : undefined}
          className={`flex items-center gap-2 px-3 py-1.5 text-sm cursor-pointer rounded-md mx-1 w-[calc(100%-8px)] text-left transition-colors duration-100 ${
            isTrashView
              ? 'bg-blue-100 text-blue-600 dark:bg-blue-600/20 dark:text-blue-400'
              : 'text-gray-700 hover:bg-gray-100 dark:text-gray-300 dark:hover:bg-gray-700/50'
          }`}
          title={t('sidebar.trash')}
        >
          <Trash2 size={16} className="opacity-70 flex-shrink-0" />
          <span className="truncate">{t('sidebar.trash')}</span>
        </button>
      </div>

      {/* Separator */}
      <div className="border-b border-gray-200 dark:border-gray-700 my-1 mx-2" />

      {/* Custom Locations */}
      {customLocations.length > 0 && (
        <div className="py-1">
          {customLocations.map((loc, index) => (
            <SidebarItem
              key={loc}
              icon={<Folder size={16} className="opacity-70 flex-shrink-0" />}
              label={basename(loc)}
              path={loc}
              currentPath={currentPath}
              tooltip={loc}
              onNavigate={onNavigate}
              onContextMenu={(e) => handleCustomLocationContextMenu(e, index)}
            />
          ))}
        </div>
      )}

      {/* Separator (only if custom locations present) */}
      {customLocations.length > 0 && (
        <div className="border-b border-gray-200 dark:border-gray-700 my-1 mx-2" />
      )}

      {/* Recent Locations */}
      {recentPaths.length > 0 && (
        <>
          <div className="flex items-center justify-between px-2 pr-4 pt-3 pb-1">
            <span className="text-[10px] font-semibold uppercase tracking-wider text-gray-500">
              {t('sidebar.recent')}
            </span>
            {onClearRecent && (
              <button
                onClick={onClearRecent}
                className="text-[10px] text-gray-500 hover:text-red-500 dark:hover:text-red-400 transition-colors"
                title={t('sidebar.clear_recent')}
              >
                <X size={12} />
              </button>
            )}
          </div>
          {recentPaths.slice(0, 10).map((recentPath) => {
            const folderName = recentPath.split('/').filter(Boolean).pop() || recentPath;
            const isActive = currentPath === recentPath;
            return (
              <div
                key={recentPath}
                className="group relative flex items-center"
              >
                <button
                  onClick={() => onNavigate(recentPath)}
                  className={`w-full flex items-center gap-2 px-2 py-1 rounded text-sm transition-colors ${
                    isActive
                      ? 'bg-blue-100 text-blue-600 dark:bg-blue-600/20 dark:text-blue-400'
                      : 'text-gray-700 hover:bg-gray-100 dark:text-gray-300 dark:hover:bg-gray-700/50'
                  }`}
                  title={recentPath}
                >
                  <Clock size={14} className="text-gray-500 shrink-0" />
                  <span className="truncate pr-4">{folderName}</span>
                </button>
                {onRemoveRecent && (
                  <button
                    onClick={(e) => { e.stopPropagation(); onRemoveRecent(recentPath); }}
                    className="absolute right-4 p-0.5 rounded opacity-0 pointer-events-none group-hover:opacity-100 group-hover:pointer-events-auto text-gray-500 hover:text-red-500 hover:bg-red-50 dark:hover:text-red-400 dark:hover:bg-gray-700/50 transition-all"
                    title={t('common.delete')}
                  >
                    <X size={12} />
                  </button>
                )}
              </div>
            );
          })}
          <div className="border-b border-gray-200 dark:border-gray-700 my-1 mx-2" />
        </>
      )}

      {/* Other Locations toggle */}
      <div className="py-1">
        <button
          aria-expanded={showVolumes}
          className={`flex items-center gap-2 px-3 py-1.5 text-sm cursor-pointer rounded-md mx-1 w-[calc(100%-8px)] text-left transition-colors duration-100 ${
            showVolumes
              ? 'text-blue-600 dark:text-blue-400'
              : 'text-gray-400 hover:text-gray-600 hover:bg-gray-100 dark:text-gray-400 dark:hover:text-gray-300 dark:hover:bg-gray-700/50'
          }`}
          onClick={() => setShowVolumes((prev) => !prev)}
        >
          {showVolumes ? (
            <ChevronDown size={14} className="flex-shrink-0" />
          ) : (
            <ChevronRight size={14} className="flex-shrink-0" />
          )}
          <Plus size={14} className="flex-shrink-0 opacity-70" />
          <span className="truncate">{t('sidebar.other_locations')}</span>
        </button>
      </div>

      {/* Volumes list */}
      {showVolumes && (
        <div className="py-1 px-1">
          {volumesLoading && volumes.length === 0 && (
            <div className="flex items-center justify-center py-3 text-gray-500 text-xs">
              <Loader2 className="w-3.5 h-3.5 animate-spin mr-1.5" />
              {t('common.loading')}
            </div>
          )}
          {volumes.map((vol) => {
            const usedBytes = vol.total_bytes - vol.free_bytes;
            const usedPercent = vol.total_bytes > 0
              ? Math.round((usedBytes / vol.total_bytes) * 100)
              : 0;
            const isEjecting = ejectingMount === vol.mount_point;

            return (
              <div
                key={vol.mount_point}
                className={`flex flex-col gap-0.5 px-2 py-1.5 rounded-md cursor-pointer transition-colors duration-100 ${
                  currentPath === vol.mount_point
                    ? 'bg-blue-100 dark:bg-blue-600/20'
                    : 'hover:bg-gray-100 dark:hover:bg-gray-700/50'
                }`}
                onClick={() => onNavigate(vol.mount_point)}
              >
                <div className="flex items-center gap-2">
                  {renderVolumeIcon(vol.volume_type)}
                  <div className="flex-1 min-w-0">
                    <div className={`text-sm truncate ${
                      currentPath === vol.mount_point ? 'text-blue-600 dark:text-blue-400' : 'text-gray-700 dark:text-gray-300'
                    }`}>
                      {vol.name || vol.mount_point}
                    </div>
                    <div className="text-[10px] text-gray-500">
                      {formatBytes(vol.free_bytes)} / {formatBytes(vol.total_bytes)}
                    </div>
                  </div>
                  {vol.is_ejectable && (
                    <button
                      aria-label={`${t('sidebar.eject')} ${vol.name}`}
                      className="p-0.5 rounded hover:bg-gray-100 text-gray-400 hover:text-gray-600 dark:hover:bg-gray-600/50 dark:hover:text-gray-200 flex-shrink-0 transition-colors"
                      onClick={(e) => handleEject(vol.mount_point, e)}
                      title={t('sidebar.eject')}
                      disabled={isEjecting}
                    >
                      {isEjecting ? (
                        <Loader2 size={14} className="animate-spin" />
                      ) : (
                        <Power size={14} />
                      )}
                    </button>
                  )}
                </div>
                <DiskUsageBar usedPercent={usedPercent} />
              </div>
            );
          })}
          {/* Unmounted partitions */}
          {unmountedPartitions.map((part) => {
            const isMounting = mountingDevice === part.device;
            return (
              <div
                key={part.device}
                className="flex flex-col gap-0.5 px-2 py-1.5 rounded-md transition-colors duration-100 hover:bg-gray-100 dark:hover:bg-gray-700/50"
              >
                <div className="flex items-center gap-2">
                  <HardDrive size={16} className="opacity-40 flex-shrink-0" />
                  <div className="flex-1 min-w-0">
                    <div className="text-sm truncate text-gray-500 dark:text-gray-500">
                      {part.name}
                    </div>
                    <div className="text-[10px] text-gray-400 dark:text-gray-600">
                      {formatBytes(part.size_bytes)} &middot; {part.fs_type}
                    </div>
                  </div>
                  <button
                    aria-label={`${t('sidebar.mount')} ${part.name}`}
                    className="p-0.5 rounded hover:bg-gray-100 text-gray-500 hover:text-gray-600 dark:hover:bg-gray-600/50 dark:hover:text-gray-200 flex-shrink-0 transition-colors"
                    onClick={(e) => handleMount(part.device, e)}
                    title={t('sidebar.mount')}
                    disabled={isMounting}
                  >
                    {isMounting ? (
                      <Loader2 size={14} className="animate-spin" />
                    ) : (
                      <Play size={14} />
                    )}
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* Tags */}
      {labelCounts.length > 0 && onTagFilter && (
        <div className="py-1 border-t border-gray-200 dark:border-gray-700/50">
          <div className="px-3 py-1">
            <span className="text-[10px] font-semibold uppercase tracking-wider text-gray-400 dark:text-gray-500">
              {t('tags.tags')}
            </span>
          </div>
          {labelCounts.map(lc => (
            <button
              key={lc.id}
              className={`flex items-center gap-2 px-3 py-1 text-sm cursor-pointer rounded-md mx-1 w-[calc(100%-8px)] text-left transition-colors duration-100 ${
                activeTagFilter === lc.id
                  ? 'bg-blue-100 text-blue-600 dark:bg-blue-600/20 dark:text-blue-400'
                  : 'text-gray-700 hover:bg-gray-100 dark:text-gray-300 dark:hover:bg-gray-700/50'
              }`}
              onClick={() => onTagFilter(activeTagFilter === lc.id ? null : lc.id)}
              title={`${lc.name} (${lc.count})`}
            >
              <span className="w-3 h-3 rounded-full shrink-0" style={{ backgroundColor: lc.color }} />
              <span className="truncate flex-1">{lc.name}</span>
              <span className="text-[10px] text-gray-400 dark:text-gray-500 shrink-0">{lc.count}</span>
            </button>
          ))}
        </div>
      )}
    </>
  );

  // -----------------------------------------------------------------------
  // Render: main
  // -----------------------------------------------------------------------

  return (
    <div role="navigation" aria-label="Places sidebar" className="w-[200px] h-full bg-white/80 border-r border-gray-200 dark:bg-gray-900/50 dark:border-gray-700 flex flex-col overflow-hidden select-none">
      {/* Header with mode toggle */}
      <div className="flex items-center justify-between px-3 py-2 border-b border-gray-200 dark:border-gray-700/50">
        <span className="text-xs font-semibold uppercase tracking-wider text-gray-500 dark:text-gray-400">
          {sidebarMode === 'places' ? t('sidebar.places') : t('sidebar.folders')}
        </span>
        <div className="flex items-center gap-0.5" role="group" aria-label="Sidebar mode">
          <button
            aria-pressed={sidebarMode === 'places'}
            className={`p-1 rounded transition-colors ${
              sidebarMode === 'places'
                ? 'bg-blue-100 text-blue-600 dark:bg-gray-700 dark:text-blue-400'
                : 'text-gray-400 hover:text-gray-600 hover:bg-gray-100 dark:text-gray-500 dark:hover:text-gray-300 dark:hover:bg-gray-700/50'
            }`}
            onClick={() => setSidebarMode('places')}
            title={t('sidebar.places')}
          >
            <LayoutList size={14} />
          </button>
          <button
            aria-pressed={sidebarMode === 'tree'}
            className={`p-1 rounded transition-colors ${
              sidebarMode === 'tree'
                ? 'bg-blue-100 text-blue-600 dark:bg-gray-700 dark:text-blue-400'
                : 'text-gray-400 hover:text-gray-600 hover:bg-gray-100 dark:text-gray-500 dark:hover:text-gray-300 dark:hover:bg-gray-700/50'
            }`}
            onClick={() => setSidebarMode('tree')}
            title={t('sidebar.folders')}
          >
            <FolderTreeIcon size={14} />
          </button>
        </div>
      </div>

      {/* Scrollable content area */}
      <div className="flex-1 overflow-y-auto scrollbar-thin scrollbar-thumb-gray-300 dark:scrollbar-thumb-gray-700 scrollbar-track-transparent">
        {sidebarMode === 'places'
          ? renderPlacesContent()
          : (
            <FolderTree
              currentPath={currentPath}
              onNavigate={onNavigate}
              onAddToSidebar={addCustomLocation}
              t={t}
            />
          )
        }
      </div>

      {/* Context menu for removing custom locations */}
      {removeMenu.visible && (
        <div
          ref={removeMenuRef}
          role="menu"
          className="fixed z-50 bg-white/95 border-gray-200 dark:bg-gray-800/95 backdrop-blur-lg rounded-lg shadow-2xl border dark:border-gray-700/50 py-1 min-w-[180px]"
          style={{ left: removeMenu.x, top: removeMenu.y }}
        >
          <button
            role="menuitem"
            className="w-full px-3 py-1.5 text-left text-[13px] flex items-center gap-2 text-red-500 hover:bg-red-100 dark:text-red-400 dark:hover:bg-red-900/40"
            onClick={() => {
              removeCustomLocation(removeMenu.index);
              closeRemoveMenu();
            }}
          >
            <X size={14} className="opacity-70" />
            <span>{t('sidebar.remove_from_sidebar')}</span>
          </button>
        </div>
      )}
    </div>
  );
};

export default PlacesSidebar;
