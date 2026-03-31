/**
 * SelectiveSyncTree — Remote folder tree with checkboxes for selective sync.
 * Users can exclude/include remote folders from AeroCloud sync.
 */
import React, { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { FolderOpen, ChevronRight, ChevronDown, Loader2, RefreshCw } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { Checkbox } from '../ui/Checkbox';

interface RemoteFolderEntry {
    path: string;
    name: string;
    depth: number;
    excluded: boolean;
}

interface SelectiveSyncTreeProps {
    /** Current excluded folders from config */
    excludedFolders: string[];
    /** Callback when user saves changes */
    onSave: (excluded: string[]) => void;
    /** Optional max depth for tree scan */
    maxDepth?: number;
}

export const SelectiveSyncTree: React.FC<SelectiveSyncTreeProps> = ({
    excludedFolders: initialExcluded,
    onSave,
    maxDepth = 3,
}) => {
    const t = useTranslation();
    const [folders, setFolders] = useState<RemoteFolderEntry[]>([]);
    const [excluded, setExcluded] = useState<Set<string>>(new Set(initialExcluded));
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
    const [dirty, setDirty] = useState(false);

    const loadTree = useCallback(async () => {
        setLoading(true);
        setError(null);
        try {
            const result = await invoke<RemoteFolderEntry[]>('list_remote_folders_tree', {
                maxDepth,
            });
            setFolders(result);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    }, [maxDepth]);

    useEffect(() => {
        loadTree();
    }, [loadTree]);

    const toggleExclude = (path: string) => {
        setExcluded(prev => {
            const next = new Set(prev);
            if (next.has(path)) {
                next.delete(path);
            } else {
                next.add(path);
            }
            return next;
        });
        setDirty(true);
    };

    const toggleCollapse = (path: string) => {
        setCollapsed(prev => {
            const next = new Set(prev);
            if (next.has(path)) {
                next.delete(path);
            } else {
                next.add(path);
            }
            return next;
        });
    };

    const handleSave = () => {
        onSave(Array.from(excluded));
        setDirty(false);
    };

    // Filter visible folders based on collapsed state
    const visibleFolders = folders.filter(f => {
        // Check if any parent is collapsed
        const parts = f.path.split('/');
        for (let i = 1; i < parts.length; i++) {
            const parentPath = parts.slice(0, i).join('/');
            if (collapsed.has(parentPath)) {
                return false;
            }
        }
        return true;
    });

    // Check if a folder has children
    const hasChildren = (path: string) =>
        folders.some(f => f.path.startsWith(path + '/') && f.path !== path);

    if (loading) {
        return (
            <div className="flex items-center justify-center py-8 text-gray-400">
                <Loader2 size={20} className="animate-spin mr-2" />
                {t('cloud.scanningFolders') || 'Scanning remote folders...'}
            </div>
        );
    }

    if (error) {
        return (
            <div className="text-center py-4">
                <p className="text-red-400 text-sm mb-2">{error}</p>
                <button onClick={loadTree} className="text-xs text-cyan-400 hover:underline flex items-center gap-1 mx-auto">
                    <RefreshCw size={12} /> {t('common.retry') || 'Retry'}
                </button>
            </div>
        );
    }

    return (
        <div className="space-y-2">
            <div className="text-xs text-gray-400 mb-2">
                {t('cloud.selectiveSyncDesc') || 'Uncheck folders to exclude them from sync.'}
            </div>

            <div className="max-h-[300px] overflow-y-auto border border-gray-700 rounded-lg p-2 space-y-0.5">
                {visibleFolders.length === 0 ? (
                    <p className="text-gray-500 text-xs text-center py-4">
                        {t('cloud.noRemoteFolders') || 'No remote folders found'}
                    </p>
                ) : (
                    visibleFolders.map(folder => {
                        const isExcluded = excluded.has(folder.path);
                        const isParentExcluded = Array.from(excluded).some(
                            ef => folder.path.startsWith(ef + '/') && folder.path !== ef
                        );
                        const children = hasChildren(folder.path);
                        const isCollapsed = collapsed.has(folder.path);

                        return (
                            <div
                                key={folder.path}
                                className="flex items-center gap-1.5 py-0.5 hover:bg-white/5 rounded px-1"
                                style={{ paddingLeft: `${folder.depth * 16 + 4}px` }}
                            >
                                {children ? (
                                    <button
                                        onClick={() => toggleCollapse(folder.path)}
                                        className="p-0.5 hover:bg-white/10 rounded flex-shrink-0"
                                    >
                                        {isCollapsed ? <ChevronRight size={12} /> : <ChevronDown size={12} />}
                                    </button>
                                ) : (
                                    <span className="w-4" />
                                )}
                                <div className={`flex items-center gap-1.5 text-xs flex-1 ${
                                    isParentExcluded ? 'opacity-40' : isExcluded ? 'opacity-60 line-through' : ''
                                }`}>
                                    <Checkbox
                                        checked={!isExcluded && !isParentExcluded}
                                        onChange={() => toggleExclude(folder.path)}
                                        disabled={isParentExcluded}
                                    />
                                    <FolderOpen size={13} className="text-yellow-500 flex-shrink-0" />
                                    <span className="truncate">{folder.name}</span>
                                </div>
                            </div>
                        );
                    })
                )}
            </div>

            {dirty && (
                <div className="flex justify-end pt-1">
                    <button
                        onClick={handleSave}
                        className="px-3 py-1.5 text-xs font-medium rounded-lg bg-cyan-600 text-white hover:bg-cyan-700 transition-colors"
                    >
                        {t('common.apply') || 'Apply'}
                    </button>
                </div>
            )}
        </div>
    );
};
