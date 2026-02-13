import * as React from 'react';
import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { HardDrive, ChevronRight, Pencil, Check, Folder, AlertTriangle, MoreHorizontal } from 'lucide-react';

interface SubDirectory {
  name: string;
  path: string;
}

interface BreadcrumbBarProps {
  currentPath: string;
  onNavigate: (path: string) => void;
  isCoherent?: boolean;
  /** When set, segments at or above this path are visually locked (sync navigation boundary) */
  minPath?: string;
  t: (key: string) => string;
}

interface PathSegment {
  segment: string;
  fullPath: string;
}

interface ChevronDropdownState {
  segmentIndex: number;
  items: SubDirectory[];
  loading: boolean;
}

function splitPath(path: string): PathSegment[] {
  if (!path) return [{ segment: '/', fullPath: '/' }];

  const normalized = path.replace(/\\/g, '/');

  // Windows drive letter: "C:/Users/name" -> [{segment: "C:", fullPath: "C:/"}, ...]
  const driveMatch = normalized.match(/^([A-Za-z]:)(\/.*)?$/);
  if (driveMatch) {
    const drive = driveMatch[1];
    const rest = driveMatch[2] || '/';
    const segments: PathSegment[] = [{ segment: drive, fullPath: `${drive}/` }];
    const parts = rest.split('/').filter(Boolean);
    let cumulative = `${drive}/`;
    for (const part of parts) {
      cumulative = `${cumulative}${cumulative.endsWith('/') ? '' : '/'}${part}`;
      segments.push({ segment: part, fullPath: cumulative });
    }
    return segments;
  }

  // Unix path: "/home/user/docs" -> [{segment: "/", fullPath: "/"}, ...]
  const segments: PathSegment[] = [{ segment: '/', fullPath: '/' }];
  const parts = normalized.split('/').filter(Boolean);
  let cumulative = '';
  for (const part of parts) {
    cumulative = `${cumulative}/${part}`;
    segments.push({ segment: part, fullPath: cumulative });
  }
  return segments;
}

export const BreadcrumbBar: React.FC<BreadcrumbBarProps> = ({
  currentPath,
  onNavigate,
  isCoherent = true,
  minPath,
  t,
}) => {
  const [isEditing, setIsEditing] = useState(false);
  const [editValue, setEditValue] = useState('');
  const [chevronDropdown, setChevronDropdown] = useState<ChevronDropdownState | null>(null);
  const [overflowDropdownOpen, setOverflowDropdownOpen] = useState(false);
  const [isOverflowing, setIsOverflowing] = useState(false);

  const containerRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const segmentsRef = useRef<HTMLDivElement>(null);
  const chevronDropdownRef = useRef<HTMLDivElement>(null);
  const overflowDropdownRef = useRef<HTMLDivElement>(null);

  const segments = useMemo(() => splitPath(currentPath), [currentPath]);

  // Number of trailing segments to always keep visible
  const VISIBLE_TAIL = 3;

  // Detect overflow using ResizeObserver
  useEffect(() => {
    const container = segmentsRef.current;
    if (!container) return;

    const check = () => {
      setIsOverflowing(container.scrollWidth > container.clientWidth);
    };

    const observer = new ResizeObserver(check);
    observer.observe(container);
    check();

    return () => observer.disconnect();
  }, [segments]);

  // Focus input when entering edit mode
  useEffect(() => {
    if (isEditing && inputRef.current) {
      inputRef.current.focus();
      inputRef.current.select();
    }
  }, [isEditing]);

  // Click-outside handler for chevron dropdown
  useEffect(() => {
    if (!chevronDropdown) return;
    const handleClick = (e: MouseEvent) => {
      if (chevronDropdownRef.current && !chevronDropdownRef.current.contains(e.target as Node)) {
        setChevronDropdown(null);
      }
    };
    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setChevronDropdown(null);
    };
    document.addEventListener('mousedown', handleClick);
    document.addEventListener('keydown', handleEscape);
    return () => {
      document.removeEventListener('mousedown', handleClick);
      document.removeEventListener('keydown', handleEscape);
    };
  }, [chevronDropdown]);

  // Click-outside handler for overflow dropdown
  useEffect(() => {
    if (!overflowDropdownOpen) return;
    const handleClick = (e: MouseEvent) => {
      if (overflowDropdownRef.current && !overflowDropdownRef.current.contains(e.target as Node)) {
        setOverflowDropdownOpen(false);
      }
    };
    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOverflowDropdownOpen(false);
    };
    document.addEventListener('mousedown', handleClick);
    document.addEventListener('keydown', handleEscape);
    return () => {
      document.removeEventListener('mousedown', handleClick);
      document.removeEventListener('keydown', handleEscape);
    };
  }, [overflowDropdownOpen]);

  const enterEditMode = useCallback(() => {
    setEditValue(currentPath);
    setIsEditing(true);
    setChevronDropdown(null);
    setOverflowDropdownOpen(false);
  }, [currentPath]);

  const cancelEdit = useCallback(() => {
    setIsEditing(false);
    setEditValue('');
  }, []);

  // Check if a path is above (proper ancestor of) the minPath boundary
  const isAboveMinPath = useCallback((segmentPath: string): boolean => {
    if (!minPath) return false;
    const norm = (p: string) => p.endsWith('/') && p.length > 1 ? p.slice(0, -1) : p;
    const sp = norm(segmentPath);
    const mp = norm(minPath);
    if (sp === mp) return false; // at boundary, not above
    if (sp === '/') return true; // root is above everything
    return mp.startsWith(sp + '/');
  }, [minPath]);

  const confirmEdit = useCallback(() => {
    const trimmed = editValue.trim();
    if (trimmed && trimmed !== currentPath && !isAboveMinPath(trimmed)) {
      onNavigate(trimmed);
    }
    setIsEditing(false);
    setEditValue('');
  }, [editValue, currentPath, onNavigate, isAboveMinPath]);

  const handleSegmentClick = useCallback((fullPath: string) => {
    if (fullPath !== currentPath && !isAboveMinPath(fullPath)) {
      onNavigate(fullPath);
    }
  }, [currentPath, onNavigate, isAboveMinPath]);

  const handleChevronClick = useCallback(async (e: React.MouseEvent, segmentIndex: number) => {
    e.stopPropagation();

    // If clicking the same chevron, toggle off
    if (chevronDropdown?.segmentIndex === segmentIndex) {
      setChevronDropdown(null);
      return;
    }

    // The chevron after segment[i] lists siblings at the level of segment[i+1],
    // i.e., directories inside segment[i].fullPath
    const parentPath = segments[segmentIndex].fullPath;

    setChevronDropdown({ segmentIndex, items: [], loading: true });

    try {
      const files: Array<{ name: string; path: string; is_dir: boolean }> = await invoke('get_local_files', {
        path: parentPath,
        showHidden: false,
      });

      const dirs: SubDirectory[] = files
        .filter((f) => f.is_dir)
        .map((f) => ({ name: f.name, path: f.path }))
        .sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' }));

      setChevronDropdown((prev) =>
        prev?.segmentIndex === segmentIndex ? { segmentIndex, items: dirs, loading: false } : prev
      );
    } catch {
      setChevronDropdown((prev) =>
        prev?.segmentIndex === segmentIndex ? { segmentIndex, items: [], loading: false } : prev
      );
    }
  }, [chevronDropdown, segments]);

  const handleDropdownNavigate = useCallback((path: string) => {
    setChevronDropdown(null);
    setOverflowDropdownOpen(false);
    onNavigate(path);
  }, [onNavigate]);

  // Compute which segments are collapsed vs visible
  const collapsedSegments = useMemo(() => {
    if (!isOverflowing || segments.length <= VISIBLE_TAIL + 1) return [];
    return segments.slice(1, segments.length - VISIBLE_TAIL);
  }, [isOverflowing, segments]);

  const visibleSegments = useMemo(() => {
    if (!isOverflowing || segments.length <= VISIBLE_TAIL + 1) return segments;
    // Root + last VISIBLE_TAIL segments
    return [segments[0], ...segments.slice(segments.length - VISIBLE_TAIL)];
  }, [isOverflowing, segments]);

  // Edit mode
  if (isEditing) {
    return (
      <div
        ref={containerRef}
        className="flex items-center h-8 bg-gray-800/50 rounded-md border border-blue-500 px-1 gap-1 w-full"
      >
        <input
          ref={inputRef}
          type="text"
          value={editValue}
          onChange={(e) => setEditValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') confirmEdit();
            if (e.key === 'Escape') cancelEdit();
          }}
          onBlur={cancelEdit}
          className="w-full bg-transparent text-white text-sm px-2 py-1 outline-none"
          spellCheck={false}
        />
        <button
          onMouseDown={(e) => {
            // Prevent blur from firing before click
            e.preventDefault();
            confirmEdit();
          }}
          className="flex-shrink-0 p-1 rounded hover:bg-gray-700/50 text-green-400 hover:text-green-300 transition-colors"
          title={t('breadcrumb.confirm') || 'Confirm'}
        >
          <Check size={14} />
        </button>
      </div>
    );
  }

  // Breadcrumb mode
  return (
    <nav
      ref={containerRef}
      className="flex items-center h-8 bg-gray-800/50 rounded-md border border-gray-700 px-2 gap-0.5 overflow-hidden w-full relative"
      role="navigation"
      aria-label="Breadcrumb"
    >
      {/* Coherence warning icon */}
      {!isCoherent && (
        <span className="flex-shrink-0 mr-1" title={t('breadcrumb.pathMismatch') || 'Path mismatch'}>
          <AlertTriangle size={14} className="text-amber-400" />
        </span>
      )}

      {/* Segments container */}
      <div ref={segmentsRef} className="flex items-center gap-0.5 overflow-hidden flex-1 min-w-0">
        {/* Root segment (always visible) */}
        <button
          onClick={() => handleSegmentClick(segments[0].fullPath)}
          className={`flex-shrink-0 p-1 rounded transition-colors ${
            isAboveMinPath(segments[0].fullPath)
              ? 'text-gray-600 cursor-not-allowed'
              : segments.length === 1
                ? 'text-white'
                : 'text-gray-400 hover:text-blue-400 hover:bg-gray-700/50'
          }`}
          title={segments[0].fullPath}
          disabled={isAboveMinPath(segments[0].fullPath)}
        >
          <HardDrive size={14} />
        </button>

        {/* Overflow indicator */}
        {collapsedSegments.length > 0 && (
          <>
            <ChevronRight size={14} className="w-3.5 h-3.5 text-gray-500 flex-shrink-0" />
            <div className="relative flex-shrink-0" ref={overflowDropdownRef}>
              <button
                onClick={() => setOverflowDropdownOpen((prev) => !prev)}
                className="text-sm px-1 py-0.5 rounded text-gray-400 hover:text-blue-400 hover:bg-gray-700/50 transition-colors"
                title={t('breadcrumb.showAll') || 'Show collapsed segments'}
              >
                <MoreHorizontal size={14} />
              </button>

              {/* Overflow dropdown */}
              {overflowDropdownOpen && (
                <div className="absolute top-full left-0 mt-1 bg-gray-800 border border-gray-600 rounded-md shadow-lg py-1 max-h-60 overflow-y-auto z-50 min-w-[160px]">
                  {collapsedSegments.map((seg) => {
                    const locked = isAboveMinPath(seg.fullPath);
                    return <button
                      key={seg.fullPath}
                      onClick={() => !locked && handleDropdownNavigate(seg.fullPath)}
                      className={`flex items-center gap-2 px-3 py-1.5 text-sm w-full text-left transition-colors ${
                        locked ? 'text-gray-600 cursor-not-allowed' : 'text-gray-300 hover:bg-gray-700 cursor-pointer'
                      }`}
                      disabled={locked}
                    >
                      <Folder size={14} className={`flex-shrink-0 ${locked ? 'text-gray-600' : 'text-amber-400'}`} />
                      <span className="truncate">{seg.segment}</span>
                    </button>;
                  })}
                </div>
              )}
            </div>
          </>
        )}

        {/* Visible segments (excluding root which is rendered above) */}
        {visibleSegments.slice(1).map((seg, idx) => {
          // Compute the real index in the original segments array
          const realIndex = isOverflowing && segments.length > VISIBLE_TAIL + 1
            ? segments.length - VISIBLE_TAIL + idx
            : idx + 1;
          const isLast = realIndex === segments.length - 1;

          return (
            <React.Fragment key={seg.fullPath}>
              {/* Chevron separator */}
              <div className="relative flex-shrink-0">
                <button
                  onClick={(e) => handleChevronClick(e, realIndex - 1)}
                  className="flex items-center p-0.5 rounded hover:bg-gray-700/30 transition-colors group"
                  title={t('breadcrumb.browseSiblings') || 'Browse sibling directories'}
                >
                  <ChevronRight
                    size={14}
                    className="w-3.5 h-3.5 text-gray-500 flex-shrink-0 group-hover:text-blue-400 transition-colors"
                  />
                </button>

                {/* Chevron dropdown */}
                {chevronDropdown?.segmentIndex === realIndex - 1 && (
                  <div
                    ref={chevronDropdownRef}
                    className="absolute top-full left-0 mt-1 bg-gray-800 border border-gray-600 rounded-md shadow-lg py-1 max-h-60 overflow-y-auto z-50 min-w-[180px]"
                  >
                    {chevronDropdown.loading ? (
                      <div className="flex items-center gap-2 px-3 py-2 text-sm text-gray-500">
                        <span className="animate-pulse">{t('breadcrumb.loading') || 'Loading...'}</span>
                      </div>
                    ) : chevronDropdown.items.length === 0 ? (
                      <div className="px-3 py-2 text-sm text-gray-500 italic">
                        {t('breadcrumb.noSubdirectories') || 'No subdirectories'}
                      </div>
                    ) : (
                      chevronDropdown.items.map((dir) => {
                        const isCurrent = segments[realIndex]?.segment === dir.name;
                        return (
                          <button
                            key={dir.path}
                            onClick={() => handleDropdownNavigate(dir.path)}
                            className={`flex items-center gap-2 px-3 py-1.5 text-sm w-full text-left transition-colors ${
                              isCurrent
                                ? 'text-blue-400 bg-gray-700/50 font-medium'
                                : 'text-gray-300 hover:bg-gray-700 cursor-pointer'
                            }`}
                          >
                            <Folder size={14} className={`flex-shrink-0 ${isCurrent ? 'text-blue-400' : 'text-amber-400'}`} />
                            <span className="truncate">{dir.name}</span>
                          </button>
                        );
                      })
                    )}
                  </div>
                )}
              </div>

              {/* Segment button */}
              {(() => {
                const locked = isAboveMinPath(seg.fullPath);
                return <button
                  onClick={() => handleSegmentClick(seg.fullPath)}
                  className={`text-sm px-1 py-0.5 rounded transition-colors truncate max-w-[150px] ${
                    locked
                      ? 'text-gray-600 cursor-not-allowed'
                      : isLast
                        ? 'text-white font-medium cursor-default'
                        : 'text-gray-300 hover:text-blue-400 hover:underline hover:bg-gray-700/50'
                  }`}
                  title={locked ? `${seg.fullPath} (sync boundary)` : seg.fullPath}
                  disabled={locked}
                  {...(isLast ? { 'aria-current': 'page' as const } : {})}
                >
                  {seg.segment}
                </button>;
              })()}
            </React.Fragment>
          );
        })}
      </div>

      {/* Edit button */}
      <button
        onClick={enterEditMode}
        className="flex-shrink-0 p-1 rounded text-gray-500 hover:text-gray-300 hover:bg-gray-700/50 transition-colors ml-1"
        title={t('breadcrumb.editPath') || 'Edit path'}
      >
        <Pencil size={12} />
      </button>
    </nav>
  );
};

export default BreadcrumbBar;
