import * as React from 'react';
import { useState, useEffect, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { save } from '@tauri-apps/plugin-dialog';
import { Archive, File, Folder, Lock, Download, Eye, X, ChevronRight, ChevronDown, EyeOff, Loader2 } from 'lucide-react';
import { ArchiveEntry, ArchiveType } from '../types';
import { useTranslation } from '../i18n';
import { formatSize } from '../utils/formatters';

interface ArchiveBrowserProps {
    archivePath: string;
    archiveType: ArchiveType;
    isEncrypted: boolean;
    onClose: () => void;
}

interface TreeNode {
    name: string;
    fullPath: string;
    isDir: boolean;
    size: number;
    compressedSize: number;
    isEncrypted: boolean;
    modified: string | null;
    children: Map<string, TreeNode>;
}

function buildTree(entries: ArchiveEntry[]): TreeNode {
    const root: TreeNode = {
        name: '', fullPath: '', isDir: true, size: 0,
        compressedSize: 0, isEncrypted: false, modified: null,
        children: new Map(),
    };

    for (const entry of entries) {
        const parts = entry.name.split(/[\\/]/).filter(Boolean);
        let current = root;

        for (let i = 0; i < parts.length; i++) {
            const part = parts[i];
            const isLast = i === parts.length - 1;

            if (!current.children.has(part)) {
                current.children.set(part, {
                    name: part,
                    fullPath: parts.slice(0, i + 1).join('/') + ((!isLast || entry.isDir) ? '/' : ''),
                    isDir: !isLast || entry.isDir,
                    size: isLast ? entry.size : 0,
                    compressedSize: isLast ? entry.compressedSize : 0,
                    isEncrypted: isLast ? entry.isEncrypted : false,
                    modified: isLast ? entry.modified : null,
                    children: new Map(),
                });
            }

            current = current.children.get(part)!;
        }
    }

    return root;
}

const TreeRow: React.FC<{
    node: TreeNode;
    depth: number;
    onExtract: (path: string) => void;
    onPreview: (path: string) => void;
}> = ({ node, depth, onExtract, onPreview }) => {
    const [expanded, setExpanded] = useState(depth < 1);
    const children = Array.from(node.children.values()).sort((a, b) => {
        if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
        return a.name.localeCompare(b.name);
    });

    return (
        <>
            <tr className="hover:bg-gray-700/30 text-sm">
                <td className="py-1 px-2" style={{ paddingLeft: `${depth * 20 + 8}px` }}>
                    <div className="flex items-center gap-1.5">
                        {node.isDir ? (
                            <button onClick={() => setExpanded(!expanded)} className="p-0.5 hover:bg-gray-600 rounded">
                                {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                            </button>
                        ) : <span className="w-5" />}
                        {node.isDir ? <Folder size={14} className="text-yellow-400 shrink-0" /> : <File size={14} className="text-gray-400 shrink-0" />}
                        {node.isEncrypted && <Lock size={12} className="text-orange-400 shrink-0" />}
                        <span className="truncate">{node.name}</span>
                    </div>
                </td>
                <td className="py-1 px-2 text-right text-gray-400 whitespace-nowrap">{node.isDir ? '' : formatSize(node.size)}</td>
                <td className="py-1 px-2 text-right text-gray-400 whitespace-nowrap">{node.compressedSize > 0 ? formatSize(node.compressedSize) : ''}</td>
                <td className="py-1 px-2 text-right">
                    {!node.isDir && (
                        <div className="flex gap-1 justify-end">
                            <button onClick={() => onPreview(node.fullPath)} className="p-1 hover:bg-gray-600 rounded" title="Preview">
                                <Eye size={14} />
                            </button>
                            <button onClick={() => onExtract(node.fullPath)} className="p-1 hover:bg-gray-600 rounded" title="Extract">
                                <Download size={14} />
                            </button>
                        </div>
                    )}
                </td>
            </tr>
            {expanded && children.map(child => (
                <TreeRow key={child.fullPath} node={child} depth={depth + 1} onExtract={onExtract} onPreview={onPreview} />
            ))}
        </>
    );
};

export const ArchiveBrowser: React.FC<ArchiveBrowserProps> = ({ archivePath, archiveType, isEncrypted, onClose }) => {
    const t = useTranslation();
    const [entries, setEntries] = useState<ArchiveEntry[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [password, setPassword] = useState('');
    const [showPassword, setShowPassword] = useState(false);
    const [needsPassword, setNeedsPassword] = useState(isEncrypted);
    const [extracting, setExtracting] = useState<string | null>(null);

    const tree = useMemo(() => buildTree(entries), [entries]);
    const archiveName = archivePath.split('/').pop() || archivePath.split('\\').pop() || archivePath;

    const loadEntries = async (pwd?: string) => {
        setLoading(true);
        setError(null);
        try {
            let result: ArchiveEntry[];
            const args = pwd ? { archivePath, password: pwd } : { archivePath };

            switch (archiveType) {
                case 'zip':
                    result = await invoke('list_zip', args);
                    break;
                case '7z':
                    result = await invoke('list_7z', args);
                    break;
                case 'tar':
                    result = await invoke('list_tar', { archivePath });
                    break;
                case 'rar':
                    result = await invoke('list_rar', { archivePath });
                    break;
                default:
                    throw new Error(`Unsupported archive type: ${archiveType}`);
            }

            setEntries(result);
            setNeedsPassword(false);
        } catch (e) {
            const msg = String(e);
            if (msg.includes('password') || msg.includes('Password') || msg.includes('encrypted')) {
                setNeedsPassword(true);
            } else {
                setError(msg);
            }
        } finally {
            setLoading(false);
        }
    };

    useEffect(() => {
        if (!isEncrypted) {
            loadEntries();
        } else {
            setLoading(false);
        }
    }, [archivePath]);

    const handlePasswordSubmit = () => {
        if (password.trim()) {
            loadEntries(password);
        }
    };

    const handleExtract = async (entryName: string) => {
        const savePath = await save({
            defaultPath: entryName.split('/').pop() || entryName,
        });
        if (!savePath) return;

        setExtracting(entryName);
        try {
            const cmd = `extract_${archiveType}_entry`;
            const args: Record<string, unknown> = {
                archivePath,
                entryName,
                outputPath: savePath,
            };
            if (password) args.password = password;
            await invoke(cmd, args);
        } catch (e) {
            setError(String(e));
        } finally {
            setExtracting(null);
        }
    };

    const handlePreview = async (entryName: string) => {
        // Extract to temp and open with system preview
        setExtracting(entryName);
        try {
            const { tempDir: getTempDir } = await import('@tauri-apps/api/path');
            const tempDirPath = await getTempDir();
            const fileName = entryName.split(/[\\/]/).pop() || 'preview';
            const tempPath = `${tempDirPath}aeroftp_preview_${Date.now()}_${fileName}`;
            const cmd = `extract_${archiveType}_entry`;
            const args: Record<string, unknown> = {
                archivePath,
                entryName,
                outputPath: tempPath,
            };
            if (password) args.password = password;
            await invoke(cmd, args);
            // The file is extracted — frontend can use UniversalPreview to display it
            // For now we just extract it; integration with preview system is handled by the parent
        } catch (e) {
            setError(String(e));
        } finally {
            setExtracting(null);
        }
    };

    const fileCount = entries.filter(e => !e.isDir).length;
    const totalSize = entries.reduce((sum, e) => sum + e.size, 0);

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
            <div className="bg-gray-800 rounded-lg shadow-2xl border border-gray-700 w-[700px] max-h-[80vh] flex flex-col">
                {/* Header */}
                <div className="flex items-center justify-between px-4 py-3 border-b border-gray-700">
                    <div className="flex items-center gap-2">
                        <Archive size={18} className="text-blue-400" />
                        <span className="font-medium truncate max-w-[400px]">{archiveName}</span>
                        <span className="text-xs text-gray-400 uppercase">{archiveType}</span>
                    </div>
                    <button onClick={onClose} className="p-1 hover:bg-gray-700 rounded" title={t('common.close')}>
                        <X size={18} />
                    </button>
                </div>

                {/* Password prompt */}
                {needsPassword && (
                    <div className="p-4 border-b border-gray-700">
                        <div className="flex items-center gap-2 mb-2">
                            <Lock size={16} className="text-orange-400" />
                            <span className="text-sm">{t('archive.passwordRequired') || 'This archive is password protected'}</span>
                        </div>
                        <div className="flex gap-2">
                            <div className="relative flex-1">
                                <input
                                    type={showPassword ? 'text' : 'password'}
                                    value={password}
                                    onChange={e => setPassword(e.target.value)}
                                    onKeyDown={e => e.key === 'Enter' && handlePasswordSubmit()}
                                    placeholder={t('archive.enterPassword') || 'Enter password...'}
                                    className="w-full bg-gray-900 border border-gray-600 rounded px-3 py-1.5 text-sm pr-8"
                                />
                                <button
                                    onClick={() => setShowPassword(!showPassword)}
                                    className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400"
                                >
                                    {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                                </button>
                            </div>
                            <button
                                onClick={handlePasswordSubmit}
                                className="px-3 py-1.5 bg-blue-600 hover:bg-blue-500 rounded text-sm"
                            >
                                {t('archive.unlock') || 'Unlock'}
                            </button>
                        </div>
                    </div>
                )}

                {/* Error */}
                {error && (
                    <div className="px-4 py-2 bg-red-900/30 text-red-400 text-sm border-b border-gray-700">
                        {error}
                    </div>
                )}

                {/* Loading */}
                {loading && (
                    <div className="flex-1 flex items-center justify-center py-12">
                        <Loader2 size={24} className="animate-spin text-blue-400" />
                    </div>
                )}

                {/* Tree view */}
                {!loading && !needsPassword && entries.length > 0 && (
                    <div className="flex-1 overflow-auto">
                        <table className="w-full">
                            <thead className="text-xs text-gray-400 border-b border-gray-700 sticky top-0 bg-gray-800">
                                <tr>
                                    <th className="py-2 px-2 text-left">{t('archive.name') || 'Name'}</th>
                                    <th className="py-2 px-2 text-right w-24">{t('archive.size') || 'Size'}</th>
                                    <th className="py-2 px-2 text-right w-24">{t('archive.compressed') || 'Compressed'}</th>
                                    <th className="py-2 px-2 text-right w-20">{t('archive.actions') || 'Actions'}</th>
                                </tr>
                            </thead>
                            <tbody>
                                {Array.from(tree.children.values())
                                    .sort((a, b) => {
                                        if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
                                        return a.name.localeCompare(b.name);
                                    })
                                    .map(node => (
                                        <TreeRow key={node.fullPath} node={node} depth={0} onExtract={handleExtract} onPreview={handlePreview} />
                                    ))}
                            </tbody>
                        </table>
                    </div>
                )}

                {/* Empty state */}
                {!loading && !needsPassword && entries.length === 0 && !error && (
                    <div className="flex-1 flex items-center justify-center py-12 text-gray-400">
                        {t('archive.empty') || 'Archive is empty'}
                    </div>
                )}

                {/* Extracting indicator */}
                {extracting && (
                    <div className="px-4 py-2 bg-blue-900/30 text-blue-400 text-sm border-t border-gray-700 flex items-center gap-2">
                        <Loader2 size={14} className="animate-spin" />
                        {t('archive.extracting') || 'Extracting'} {extracting.split('/').pop()}...
                    </div>
                )}

                {/* Footer */}
                <div className="px-4 py-2 border-t border-gray-700 text-xs text-gray-400 flex justify-between">
                    <span>{fileCount} {t('archive.files') || 'files'}, {formatSize(totalSize)}</span>
                </div>
            </div>
        </div>
    );
};
