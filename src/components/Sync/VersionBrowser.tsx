/**
 * VersionBrowser — Browse and restore archived file versions from .aeroversions/
 */
import React, { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { History, RotateCcw, Trash2, HardDrive, Loader2 } from 'lucide-react';
import { useTranslation } from '../../i18n';

interface VersionEntry {
    archive_path: string;
    original_relative: string;
    archived_at: string;
    size: number;
}

interface CleanupStats {
    deleted_count: number;
    freed_bytes: number;
}

interface VersionBrowserProps {
    /** File to show versions for (relative path), or null for overview */
    filePath?: string;
}

function formatSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

export const VersionBrowser: React.FC<VersionBrowserProps> = ({ filePath }) => {
    const t = useTranslation();
    const [versions, setVersions] = useState<VersionEntry[]>([]);
    const [loading, setLoading] = useState(false);
    const [diskUsage, setDiskUsage] = useState<number>(0);
    const [cleanupResult, setCleanupResult] = useState<CleanupStats | null>(null);

    const loadVersions = useCallback(async () => {
        if (!filePath) return;
        setLoading(true);
        try {
            const result = await invoke<VersionEntry[]>('list_file_versions', {
                relativePath: filePath,
            });
            setVersions(result);
        } catch (e) {
            console.error('Failed to load versions:', e);
        } finally {
            setLoading(false);
        }
    }, [filePath]);

    const loadDiskUsage = useCallback(async () => {
        try {
            const usage = await invoke<number>('versions_disk_usage');
            setDiskUsage(usage);
        } catch {
            // ignore
        }
    }, []);

    useEffect(() => {
        loadVersions();
        loadDiskUsage();
    }, [loadVersions, loadDiskUsage]);

    const handleRestore = async (version: VersionEntry) => {
        try {
            await invoke('restore_file_version', {
                archivePath: version.archive_path,
                originalRelative: version.original_relative,
            });
            loadVersions();
        } catch (e) {
            console.error('Failed to restore version:', e);
        }
    };

    const handleCleanup = async () => {
        try {
            const stats = await invoke<CleanupStats>('cleanup_versions');
            setCleanupResult(stats);
            loadDiskUsage();
            loadVersions();
        } catch (e) {
            console.error('Failed to cleanup versions:', e);
        }
    };

    return (
        <div className="space-y-3">
            <div className="flex items-center justify-between">
                <div className="flex items-center gap-2 text-sm font-medium">
                    <History size={16} className="text-cyan-500" />
                    {t('cloud.versionHistory') || 'Version History'}
                </div>
                <div className="flex items-center gap-2 text-xs text-gray-400">
                    <HardDrive size={12} />
                    {formatSize(diskUsage)}
                </div>
            </div>

            {filePath && (
                <div className="text-xs text-gray-400 bg-gray-900/30 px-2 py-1 rounded">
                    {filePath}
                </div>
            )}

            {loading ? (
                <div className="flex items-center justify-center py-4 text-gray-400">
                    <Loader2 size={16} className="animate-spin mr-2" />
                    {t('common.loading') || 'Loading...'}
                </div>
            ) : versions.length === 0 ? (
                <p className="text-xs text-gray-500 text-center py-4">
                    {t('cloud.noVersions') || 'No archived versions found'}
                </p>
            ) : (
                <div className="max-h-[200px] overflow-y-auto space-y-1">
                    {versions.map((v, i) => (
                        <div key={i} className="flex items-center justify-between px-2 py-1.5 rounded hover:bg-white/5 text-xs">
                            <div className="flex-1 min-w-0">
                                <span className="text-gray-300">{v.archived_at}</span>
                                <span className="text-gray-500 ml-2">{formatSize(v.size)}</span>
                            </div>
                            <button
                                onClick={() => handleRestore(v)}
                                className="ml-2 p-1 hover:bg-cyan-500/20 rounded text-cyan-400"
                                title={t('cloud.restoreVersion') || 'Restore this version'}
                            >
                                <RotateCcw size={13} />
                            </button>
                        </div>
                    ))}
                </div>
            )}

            <div className="flex justify-end pt-1 border-t border-gray-700">
                <button
                    onClick={handleCleanup}
                    className="flex items-center gap-1 px-2 py-1 text-xs text-gray-400 hover:text-red-400 hover:bg-red-500/10 rounded transition-colors"
                >
                    <Trash2 size={12} />
                    {t('cloud.cleanupVersions') || 'Cleanup old versions'}
                </button>
            </div>

            {cleanupResult && cleanupResult.deleted_count > 0 && (
                <div className="text-xs text-green-400 bg-green-500/10 px-2 py-1 rounded">
                    {t('cloud.cleanupDone') || 'Cleaned up'}: {cleanupResult.deleted_count} files, {formatSize(cleanupResult.freed_bytes)} freed
                </div>
            )}
        </div>
    );
};
