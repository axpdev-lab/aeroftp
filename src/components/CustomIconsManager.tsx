// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet, AI-assisted (see AI-TRANSPARENCY.md)

/**
 * CustomIconsManager
 *
 * Standalone gallery for the user's custom uploaded server icons. Mirrors the
 * persistence layer of IconPickerDialog (`localStorage['aeroftp-custom-icons']`)
 * so a rename / reorder / delete here is immediately visible from the
 * per-profile Choose Icon dialog and vice versa.
 *
 * Shipped from Settings, Appearance, Icons so users can curate the library
 * without going through a profile edit.
 */

import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { Pencil, Trash2, Upload, Check, X, ArrowDownAZ, Clock } from 'lucide-react';
import { useTranslation } from '../i18n';
import { logger } from '../utils/logger';

const CUSTOM_ICONS_KEY = 'aeroftp-custom-icons';
const SORT_KEY = 'aeroftp-custom-icons-sort';

type SortMode = 'recent' | 'alphabetical';

interface CustomIcon {
    id: string;
    name: string;
    dataUrl: string;
    type: 'svg' | 'raster';
    createdAt: string;
}

function loadCustomIcons(): CustomIcon[] {
    try {
        const raw = localStorage.getItem(CUSTOM_ICONS_KEY);
        if (!raw) return [];
        const parsed = JSON.parse(raw);
        return Array.isArray(parsed) ? parsed : [];
    } catch {
        return [];
    }
}

function persistCustomIcons(icons: CustomIcon[]) {
    try {
        localStorage.setItem(CUSTOM_ICONS_KEY, JSON.stringify(icons));
        // Notify other surfaces (IconPickerDialog) that the library changed,
        // so they can re-read on next open without bouncing through this UI.
        window.dispatchEvent(new CustomEvent('aeroftp-custom-icons-changed'));
    } catch (e) {
        logger.warn('custom-icons: failed to persist', e);
    }
}

function loadSort(): SortMode {
    try {
        const v = localStorage.getItem(SORT_KEY);
        return v === 'alphabetical' ? 'alphabetical' : 'recent';
    } catch {
        return 'recent';
    }
}

function persistSort(sort: SortMode) {
    try { localStorage.setItem(SORT_KEY, sort); } catch { /* ignore */ }
}

export const CustomIconsManager: React.FC = () => {
    const t = useTranslation();
    const [icons, setIcons] = useState<CustomIcon[]>(() => loadCustomIcons());
    const [sort, setSort] = useState<SortMode>(() => loadSort());
    const [renameId, setRenameId] = useState<string | null>(null);
    const [renameValue, setRenameValue] = useState('');
    const [dragIdx, setDragIdx] = useState<number | null>(null);
    const [overIdx, setOverIdx] = useState<number | null>(null);
    const [uploading, setUploading] = useState(false);

    // Mirror external changes (the per-profile picker dialog also writes to the
    // same key) so users see the gallery up to date if they roundtrip through
    // a profile while Settings stays open.
    useEffect(() => {
        const refresh = () => setIcons(loadCustomIcons());
        const storageHandler = (e: StorageEvent) => {
            if (e.key === CUSTOM_ICONS_KEY) refresh();
        };
        window.addEventListener('aeroftp-custom-icons-changed', refresh);
        window.addEventListener('storage', storageHandler);
        return () => {
            window.removeEventListener('aeroftp-custom-icons-changed', refresh);
            window.removeEventListener('storage', storageHandler);
        };
    }, []);

    const sortedIcons = useMemo(() => {
        if (sort === 'alphabetical') {
            return [...icons].sort((a, b) =>
                a.name.localeCompare(b.name, undefined, { sensitivity: 'base' })
            );
        }
        // `recent`: keep insertion order (newest first via prepend in upload),
        // tie-break by createdAt for deterministic ordering after edits.
        return [...icons].sort((a, b) => {
            // Stable: preserve current array order. We compare via index as
            // fallback so the array order wins when present.
            const ai = icons.indexOf(a);
            const bi = icons.indexOf(b);
            return ai - bi;
        });
    }, [icons, sort]);

    const updateSort = useCallback((mode: SortMode) => {
        setSort(mode);
        persistSort(mode);
    }, []);

    const startRename = useCallback((icon: CustomIcon) => {
        setRenameId(icon.id);
        setRenameValue(icon.name);
    }, []);

    const commitRename = useCallback(() => {
        if (!renameId) return;
        const name = renameValue.trim();
        if (!name) {
            setRenameId(null);
            return;
        }
        setIcons(prev => {
            const next = prev.map(i => i.id === renameId ? { ...i, name } : i);
            persistCustomIcons(next);
            return next;
        });
        setRenameId(null);
    }, [renameId, renameValue]);

    const cancelRename = useCallback(() => setRenameId(null), []);

    const deleteIcon = useCallback((id: string) => {
        setIcons(prev => {
            const target = prev.find(i => i.id === id);
            const label = target?.name || t('iconPicker.removeIcon');
            if (!window.confirm(t('iconPicker.confirmDelete', { name: label }))) {
                return prev;
            }
            const next = prev.filter(i => i.id !== id);
            persistCustomIcons(next);
            return next;
        });
    }, [t]);

    const handleDragStart = useCallback((idx: number) => (e: React.DragEvent) => {
        // Reorder is only meaningful in `recent` mode where the array order
        // is the source of truth. Disable in alphabetical mode so users do
        // not waste a drag whose result is silently overwritten by sort.
        if (sort !== 'recent') {
            e.preventDefault();
            return;
        }
        setDragIdx(idx);
        e.dataTransfer.effectAllowed = 'move';
        e.dataTransfer.setData('text/plain', idx.toString());
    }, [sort]);

    const handleDragOver = useCallback((idx: number) => (e: React.DragEvent) => {
        e.preventDefault();
        e.dataTransfer.dropEffect = 'move';
        setOverIdx(idx);
    }, []);

    const handleDrop = useCallback((idx: number) => (e: React.DragEvent) => {
        e.preventDefault();
        e.stopPropagation();
        if (dragIdx === null || dragIdx === idx) {
            setDragIdx(null);
            setOverIdx(null);
            return;
        }
        setIcons(prev => {
            if (dragIdx < 0 || dragIdx >= prev.length) return prev;
            const next = [...prev];
            const [moved] = next.splice(dragIdx, 1);
            const target = dragIdx < idx ? idx - 1 : idx;
            next.splice(target, 0, moved);
            persistCustomIcons(next);
            return next;
        });
        setDragIdx(null);
        setOverIdx(null);
    }, [dragIdx]);

    const handleDragEnd = useCallback(() => {
        setDragIdx(null);
        setOverIdx(null);
    }, []);

    const ingestFiles = useCallback(async (files: FileList | null) => {
        if (!files || files.length === 0) return;
        setUploading(true);
        try {
            const additions: CustomIcon[] = [];
            for (const file of Array.from(files)) {
                const ext = (file.name.split('.').pop() || 'png').toLowerCase();
                const baseName = file.name.replace(/\.[^.]+$/, '') || 'Icon';
                const bytes = new Uint8Array(await file.arrayBuffer());
                let dataUrl: string | null = null;
                let type: 'svg' | 'raster' = 'raster';

                if (ext === 'svg') {
                    let svgText: string;
                    try {
                        svgText = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
                    } catch { continue; }
                    if (!svgText.trim().toLowerCase().includes('<svg')) continue;
                    dataUrl = `data:image/svg+xml;base64,${btoa(unescape(encodeURIComponent(svgText)))}`;
                    type = 'svg';
                } else {
                    const mimeMap: Record<string, string> = {
                        jpg: 'image/jpeg', jpeg: 'image/jpeg', png: 'image/png',
                        gif: 'image/gif', webp: 'image/webp', ico: 'image/x-icon',
                    };
                    const mime = mimeMap[ext] || 'image/png';
                    const blob = new Blob([new Uint8Array(bytes)], { type: mime });
                    const url = URL.createObjectURL(blob);
                    dataUrl = await new Promise<string | null>((resolve) => {
                        const img = new window.Image();
                        const timeout = setTimeout(() => { URL.revokeObjectURL(url); resolve(null); }, 10_000);
                        img.onload = () => {
                            clearTimeout(timeout);
                            const canvas = document.createElement('canvas');
                            const size = 128;
                            canvas.width = size; canvas.height = size;
                            const ctx = canvas.getContext('2d');
                            if (!ctx) { URL.revokeObjectURL(url); resolve(null); return; }
                            const scale = Math.min(size / img.width, size / img.height);
                            const w = img.width * scale, h = img.height * scale;
                            ctx.drawImage(img, (size - w) / 2, (size - h) / 2, w, h);
                            URL.revokeObjectURL(url);
                            resolve(canvas.toDataURL('image/png'));
                        };
                        img.onerror = () => { clearTimeout(timeout); URL.revokeObjectURL(url); resolve(null); };
                        img.src = url;
                    });
                }
                if (!dataUrl) continue;
                additions.push({
                    id: `icon_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`,
                    name: baseName,
                    dataUrl,
                    type,
                    createdAt: new Date().toISOString(),
                });
            }
            if (additions.length === 0) return;
            setIcons(prev => {
                const next = [...additions, ...prev];
                persistCustomIcons(next);
                return next;
            });
        } finally {
            setUploading(false);
        }
    }, []);

    const handleUploadInput = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
        ingestFiles(e.target.files);
        // Reset value so re-selecting the same file fires onChange again.
        e.target.value = '';
    }, [ingestFiles]);

    return (
        <div className="space-y-3">
            <div className="flex items-center justify-between gap-2">
                <div className="flex flex-col">
                    <span className="text-sm font-medium text-gray-700 dark:text-gray-200">
                        {t('settings.customIconsLibrary') || 'Custom icons library'}
                    </span>
                    <span className="text-[11px] text-gray-500">
                        {t('settings.customIconsLibraryDesc') || 'Manage uploaded icons reused across profiles. Drag to reorder, double-click the name to rename.'}
                    </span>
                </div>
                <div className="flex items-center gap-2 flex-shrink-0">
                    <div className="inline-flex rounded-md border border-gray-300 dark:border-gray-600 overflow-hidden text-xs">
                        <button
                            type="button"
                            onClick={() => updateSort('recent')}
                            className={`px-2 py-1 flex items-center gap-1 transition-colors ${
                                sort === 'recent'
                                    ? 'bg-blue-500/15 text-blue-600 dark:text-blue-400'
                                    : 'text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-700/50'
                            }`}
                            title={t('iconPicker.sortRecent') || 'Recent first'}
                        >
                            <Clock size={12} />
                            {t('iconPicker.sortRecent') || 'Recent'}
                        </button>
                        <button
                            type="button"
                            onClick={() => updateSort('alphabetical')}
                            className={`px-2 py-1 flex items-center gap-1 transition-colors ${
                                sort === 'alphabetical'
                                    ? 'bg-blue-500/15 text-blue-600 dark:text-blue-400'
                                    : 'text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-700/50'
                            }`}
                            title={t('iconPicker.sortAlphabetical') || 'Sort A-Z'}
                        >
                            <ArrowDownAZ size={12} />
                            {t('iconPicker.sortAlphabetical') || 'A-Z'}
                        </button>
                    </div>
                    <label
                        className={`inline-flex items-center gap-1 px-2 py-1 rounded-md text-xs cursor-pointer transition-colors ${
                            uploading
                                ? 'bg-gray-200 dark:bg-gray-700 text-gray-400 cursor-wait'
                                : 'bg-blue-500 text-white hover:bg-blue-600'
                        }`}
                        title={t('iconPicker.uploadHint') || 'Upload SVG / PNG / JPG / WEBP / ICO'}
                    >
                        <Upload size={12} />
                        {uploading ? '...' : (t('settings.customIconsUpload') || 'Upload')}
                        <input
                            type="file"
                            multiple
                            accept=".svg,.png,.jpg,.jpeg,.gif,.webp,.ico"
                            className="hidden"
                            disabled={uploading}
                            onChange={handleUploadInput}
                        />
                    </label>
                </div>
            </div>

            {sortedIcons.length === 0 ? (
                <div className="rounded-lg border border-dashed border-gray-300 dark:border-gray-600 p-6 text-center text-xs text-gray-500">
                    {t('settings.customIconsEmpty') || 'No custom icons yet. Upload SVG, PNG, JPG, WEBP, or ICO files to build your library.'}
                </div>
            ) : (
                <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 gap-2">
                    {sortedIcons.map((icon, idx) => {
                        const isDragSource = dragIdx === idx;
                        const isDropTarget = overIdx === idx && dragIdx !== null && dragIdx !== idx;
                        const isRenaming = renameId === icon.id;
                        return (
                            <div
                                key={icon.id}
                                draggable={sort === 'recent'}
                                onDragStart={handleDragStart(idx)}
                                onDragOver={handleDragOver(idx)}
                                onDrop={handleDrop(idx)}
                                onDragEnd={handleDragEnd}
                                className={`group relative flex flex-col items-center gap-1 p-2 rounded-lg border transition-colors ${
                                    isDropTarget
                                        ? 'border-blue-500 ring-2 ring-blue-500/20 bg-blue-50 dark:bg-blue-900/20'
                                        : 'border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800/40 hover:border-gray-400 dark:hover:border-gray-500'
                                } ${isDragSource ? 'opacity-50' : ''}`}
                            >
                                <img
                                    src={icon.dataUrl}
                                    alt={icon.name}
                                    className="w-12 h-12 object-contain"
                                    draggable={false}
                                />
                                {isRenaming ? (
                                    <div className="flex items-center gap-1 w-full">
                                        <input
                                            autoFocus
                                            type="text"
                                            value={renameValue}
                                            onChange={(e) => setRenameValue(e.target.value)}
                                            onKeyDown={(e) => {
                                                if (e.key === 'Enter') commitRename();
                                                if (e.key === 'Escape') cancelRename();
                                            }}
                                            onBlur={commitRename}
                                            className="flex-1 min-w-0 text-xs bg-transparent border border-blue-500 rounded px-1 py-0.5 outline-none"
                                        />
                                        <button
                                            onMouseDown={(e) => { e.preventDefault(); commitRename(); }}
                                            className="p-0.5 text-green-500 hover:text-green-400"
                                            title={t('breadcrumb.confirm') || 'Confirm'}
                                        >
                                            <Check size={12} />
                                        </button>
                                        <button
                                            onMouseDown={(e) => { e.preventDefault(); cancelRename(); }}
                                            className="p-0.5 text-gray-500 hover:text-gray-300"
                                            title={t('common.cancel') || 'Cancel'}
                                        >
                                            <X size={12} />
                                        </button>
                                    </div>
                                ) : (
                                    <button
                                        type="button"
                                        onDoubleClick={() => startRename(icon)}
                                        className="text-[11px] text-gray-700 dark:text-gray-300 truncate w-full text-center hover:text-blue-500 dark:hover:text-blue-400 transition-colors"
                                        title={t('settings.customIconsRenameHint') || 'Double-click to rename'}
                                    >
                                        {icon.name}
                                    </button>
                                )}
                                <div className="absolute top-1 right-1 flex gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
                                    <button
                                        type="button"
                                        onClick={() => startRename(icon)}
                                        className="p-1 rounded bg-white/80 dark:bg-gray-700/80 text-gray-600 dark:text-gray-300 hover:text-blue-500"
                                        title={t('common.rename') || 'Rename'}
                                    >
                                        <Pencil size={11} />
                                    </button>
                                    <button
                                        type="button"
                                        onClick={() => deleteIcon(icon.id)}
                                        className="p-1 rounded bg-white/80 dark:bg-gray-700/80 text-gray-600 dark:text-gray-300 hover:text-red-500"
                                        title={t('common.delete') || 'Delete'}
                                    >
                                        <Trash2 size={11} />
                                    </button>
                                </div>
                            </div>
                        );
                    })}
                </div>
            )}
        </div>
    );
};

export default CustomIconsManager;
