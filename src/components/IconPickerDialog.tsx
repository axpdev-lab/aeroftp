import * as React from 'react';
import { useState, useMemo, useEffect, useCallback, useRef } from 'react';
import { flushSync } from 'react-dom';
import { createRoot } from 'react-dom/client';
import { open } from '@tauri-apps/plugin-dialog';
import { readFile } from '@tauri-apps/plugin-fs';
import { Image as ImageIcon, Upload, Trash2, X, Search, PencilLine } from 'lucide-react';
import { PROVIDER_LOGOS } from './ProviderLogos';
import { buildDiscoverCategories, type DiscoverItem } from './IntroHub/discoverData';
import { useTranslation } from '../i18n';
import { logger } from '../utils/logger';

/**
 * Within each catalog category, surface a curated set of "popular" providers
 * before the rest (which fall back to alphabetical). The list reflects the
 * Discover panel's existing priority signals: major cloud / S3 providers,
 * generic protocols, the ones a typical user reaches for first.
 *
 * Lower number = higher position. Anything not in the map sorts after, by name.
 */
const POPULARITY_PRIORITY: Record<string, number> = {
    // Protocols
    'ftp-generic': 0, 'sftp-generic': 1, 'azure-generic': 2, 'hetzner-storage-box': 3,
    // Object storage
    'custom-s3': 0, 'amazon-s3': 1, 'google-cloud-storage': 2, 'cloudflare-r2': 3,
    'minio': 4, 'storj': 5, 'backblaze': 6, 'wasabi': 7, 'idrive-e2': 8,
    'mega-s4': 9, 'digitalocean-spaces': 10,
    // WebDAV
    'custom-webdav': 0, 'nextcloud': 1, 'koofr-webdav': 2, 'felicloud': 3, 'tabdigital': 4, 'seafile': 5,
    // Cloud
    'googledrive': 0, 'dropbox': 1, 'onedrive': 2, 'mega': 3, 'box': 4,
    'pcloud': 5, 'koofr-cloud': 6, 'kdrive': 7, 'filen': 8, 'internxt': 9,
    'jottacloud': 10, 'zohoworkdrive': 11,
    // Developer
    'github': 0, 'gitlab': 1, 'sourceforge': 2,
    // Media
    'immich': 0, 'pixelunion': 1, 'imagekit': 2, 'uploadcare': 3, 'cloudinary': 4,
};

const CUSTOM_ICONS_KEY = 'aeroftp-custom-icons';
const SHIPPED_SORT_KEY = 'aeroftp-icon-picker-sort';

type ShippedSort = 'popularity' | 'alphabetical';

function loadShippedSort(): ShippedSort {
    try {
        const v = localStorage.getItem(SHIPPED_SORT_KEY);
        return v === 'alphabetical' ? 'alphabetical' : 'popularity';
    } catch {
        return 'popularity';
    }
}

function persistShippedSort(sort: ShippedSort) {
    try { localStorage.setItem(SHIPPED_SORT_KEY, sort); } catch { /* ignore */ }
}

interface CustomIcon {
    id: string;
    name: string;
    dataUrl: string;
    type: 'svg' | 'raster';
    createdAt: string;
}

interface IconPickerDialogProps {
    onSelect: (dataUrl: string) => void;
    onClose: () => void;
    /** Icon currently applied to the server being edited (custom upload first,
     *  then auto-detected favicon, then nothing). Surfaces at the top of the
     *  Custom tab so the user always sees what's in use without scrolling
     *  through the library. */
    currentIcon?: string;
    /** Auto-detected server favicon stored on the profile. Used as the fallback
     *  for the "On server" card when the live re-scan is unavailable or fails. */
    detectedFavicon?: string;
    /** Optional live re-scan callback. When provided, the dialog fires it on
     *  open and surfaces the result as "On server": so a favicon that changed
     *  server-side after the original auto-detection is reflected immediately
     *  without forcing the user to reconnect. Returns the data URL (or null if
     *  the server has no detectable favicon). */
    onRescan?: () => Promise<string | null>;
}

type Tab = 'shipped' | 'custom';

/**
 * Convert a React functional logo component into an SVG data URL at click time.
 * Stored as `data:image/svg+xml;base64,...` so every consumer that already
 * renders `customIconUrl` via `<img src>` works without changes.
 *
 * Implementation: render synchronously into a detached DOM node via React 18's
 * `createRoot` + `flushSync`, then read the SVG's `outerHTML`. This avoids
 * pulling in `react-dom/server` (~78KB) and keeps us off code paths that have
 * historically been fragile in the WebKitGTK dev environment.
 *
 * `<img src=svg>` sandboxes any embedded scripts the same way browsers treat
 * other passive images, so the encoded markup never executes against our DOM.
 */
function reactLogoToSvgDataUrl(LogoComp: React.FC<{ size?: number }>): string | null {
    const compName = LogoComp.displayName || LogoComp.name || 'Logo';
    const container = document.createElement('div');
    container.style.position = 'fixed';
    container.style.left = '-9999px';
    container.style.top = '0';
    container.style.pointerEvents = 'none';
    document.body.appendChild(container);
    const root = createRoot(container);
    try {
        try {
            flushSync(() => root.render(React.createElement(LogoComp, { size: 64 })));
        } catch (renderErr) {
            logger.warn(`icon-picker: render failed for ${compName}`, renderErr);
            return null;
        }
        const svg = container.querySelector('svg');
        if (!svg) {
            // Logos that render as <img> (PNG-backed providers like Hetzner,
            // MinIO, Koofr, FileLu, Blomp, OpenDrive...) used to silently fail
            // here, leaving the click as a no-op. Fall back to the image src
            // so the selection still works for the whole shipped catalog.
            const img = container.querySelector('img');
            if (img && img.src) {
                return img.src;
            }
            logger.warn(`icon-picker: no <svg> or <img> found after render for ${compName}`);
            return null;
        }
        if (!svg.hasAttribute('xmlns')) {
            svg.setAttribute('xmlns', 'http://www.w3.org/2000/svg');
        }
        // XMLSerializer is the standardized path for SVG (handles namespaces
        // and self-closing tags correctly). outerHTML works on most browsers
        // but has historically been fragile with SVG content on WebKitGTK.
        let markup: string;
        try {
            markup = new XMLSerializer().serializeToString(svg);
        } catch {
            markup = svg.outerHTML;
        }
        if (!markup || !markup.includes('<svg')) {
            logger.warn(`icon-picker: empty markup for ${compName}`);
            return null;
        }
        return `data:image/svg+xml;base64,${btoa(unescape(encodeURIComponent(markup)))}`;
    } catch (e) {
        logger.warn(`icon-picker: failed to serialize ${compName}`, e);
        return null;
    } finally {
        try { root.unmount(); } catch { /* ignore */ }
        try { container.remove(); } catch { /* ignore */ }
    }
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
        // Mirror the change to any other surface listening for library updates
        // (Settings > Appearance > Custom icons gallery in particular).
        window.dispatchEvent(new CustomEvent('aeroftp-custom-icons-changed'));
    } catch (e) {
        logger.warn('icon-picker: failed to persist custom icons', e);
    }
}

export function IconPickerDialog({ onSelect, onClose, currentIcon, detectedFavicon, onRescan }: IconPickerDialogProps) {
    const t = useTranslation();
    const [tab, setTab] = useState<Tab>('shipped');
    const [search, setSearch] = useState('');
    const [customIcons, setCustomIcons] = useState<CustomIcon[]>(() => loadCustomIcons());
    const [uploading, setUploading] = useState(false);
    const [shippedSort, setShippedSortState] = useState<ShippedSort>(() => loadShippedSort());
    const setShippedSort = useCallback((sort: ShippedSort) => {
        setShippedSortState(sort);
        persistShippedSort(sort);
    }, []);

    // Drag-reorder state for the Custom icons grid. `dragIdx` tracks the
    // source index, `overIdx` the current hover target. Both reset on drop or
    // dragend, including when the drop happens outside the dialog.
    const [dragIdx, setDragIdx] = useState<number | null>(null);
    const [overIdx, setOverIdx] = useState<number | null>(null);

    // Live re-scan state. `liveDetected === undefined` means "not yet scanned"
    // (or no rescan callback provided). `null` = scanned, no favicon found.
    // string = the freshly-fetched favicon as data URL.
    const [liveDetected, setLiveDetected] = useState<string | null | undefined>(undefined);
    const [rescanning, setRescanning] = useState(false);

    // Fire the re-scan once when the dialog mounts. The fresh result trumps the
    // saved `detectedFavicon` (which may be stale if the favicon changed
    // server-side after the original auto-detection cached it on the profile).
    useEffect(() => {
        if (!onRescan) return;
        let cancelled = false;
        setRescanning(true);
        onRescan()
            .then((result) => {
                if (cancelled) return;
                setLiveDetected(result);
                setRescanning(false);
            })
            .catch((e) => {
                if (cancelled) return;
                logger.warn('icon-picker: rescan failed', e);
                // On failure, leave liveDetected as undefined so the UI falls
                // back to the saved `detectedFavicon` instead of showing nothing.
                setRescanning(false);
            });
        return () => { cancelled = true; };
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, []);

    // Resolve which icon goes into the "On server" card. Once we have a live
    // result (string OR null), we trust it over the saved detection. If we
    // never scanned (no onRescan, or the scan errored), we fall back to the
    // saved value.
    const liveScanRan = liveDetected !== undefined;
    const onServerIcon = liveScanRan ? liveDetected : detectedFavicon;
    const onServerLabel = liveScanRan ? t('iconPicker.onServer') : t('iconPicker.autoDetected');
    const onServerDescription = liveScanRan ? t('iconPicker.onServerDescription') : t('iconPicker.autoDetectedDescription');
    // True when the live scan confirmed the in-use icon matches what's on the
    // server right now: drives a small confirmation badge on the "In use" card.
    const inUseMatchesServer = liveScanRan && liveDetected !== null && liveDetected === currentIcon;

    // Reuse the Discover catalog so display names and categories stay in sync
    // with the rest of the app. Only items that resolve to a real logo
    // component are kept: the picker is a visual gallery, not a feature list.
    const shippedByCategory = useMemo(() => {
        const categories = buildDiscoverCategories();
        const result: Array<{
            id: string;
            labelKey: string;
            items: Array<{ key: string; name: string; Logo: React.FC<{ size?: number }> }>;
        }> = [];

        const seenKeys = new Set<string>();
        for (const cat of categories) {
            const items: Array<{ key: string; name: string; Logo: React.FC<{ size?: number }> }> = [];
            for (const item of cat.items as DiscoverItem[]) {
                const key = item.providerId || item.id;
                const Logo = PROVIDER_LOGOS[key] || PROVIDER_LOGOS[item.protocol];
                if (!Logo) continue;
                // Dedupe across categories: same providerId can appear in multiple
                // groupings (filelu = cloud + WebDAV + S3). First occurrence wins.
                const dedupKey = `${key}:${Logo.displayName || Logo.name || key}`;
                if (seenKeys.has(dedupKey)) continue;
                seenKeys.add(dedupKey);
                items.push({ key, name: item.name, Logo });
            }
            if (items.length > 0) {
                if (shippedSort === 'alphabetical') {
                    items.sort((a, b) => a.name.localeCompare(b.name));
                } else {
                    // Curated "popular first", then alphabetical for the long tail.
                    items.sort((a, b) => {
                        const pa = POPULARITY_PRIORITY[a.key] ?? Number.POSITIVE_INFINITY;
                        const pb = POPULARITY_PRIORITY[b.key] ?? Number.POSITIVE_INFINITY;
                        if (pa !== pb) return pa - pb;
                        return a.name.localeCompare(b.name);
                    });
                }
                result.push({ id: cat.id, labelKey: cat.labelKey, items });
            }
        }
        return result;
    }, [shippedSort]);

    const filteredShipped = useMemo(() => {
        if (!search.trim()) return shippedByCategory;
        const q = search.toLowerCase();
        return shippedByCategory
            .map(cat => ({ ...cat, items: cat.items.filter(it => it.name.toLowerCase().includes(q)) }))
            .filter(cat => cat.items.length > 0);
    }, [shippedByCategory, search]);

    const handleSelectShipped = useCallback((Logo: React.FC<{ size?: number }>) => {
        const url = reactLogoToSvgDataUrl(Logo);
        if (url) {
            onSelect(url);
            onClose();
        }
    }, [onSelect, onClose]);

    const handleSelectCustom = useCallback((icon: CustomIcon) => {
        onSelect(icon.dataUrl);
        onClose();
    }, [onSelect, onClose]);

    // Custom icons drag-reorder. Mirrors the pattern in MyServersPanel but
    // simpler: single flat list, no filtering, no sentinels, no auto-scroll
    // (the dialog grid is short enough to fit without virtualization).
    const handleIconDragStart = useCallback((idx: number) => (e: React.DragEvent) => {
        setDragIdx(idx);
        e.dataTransfer.effectAllowed = 'move';
        e.dataTransfer.setData('text/plain', idx.toString());
    }, []);

    const handleIconDragOver = useCallback((idx: number) => (e: React.DragEvent) => {
        e.preventDefault();
        e.dataTransfer.dropEffect = 'move';
        setOverIdx(idx);
    }, []);

    const handleIconDrop = useCallback((idx: number) => (e: React.DragEvent) => {
        e.preventDefault();
        e.stopPropagation();
        if (dragIdx === null || dragIdx === idx) {
            setDragIdx(null);
            setOverIdx(null);
            return;
        }
        setCustomIcons(prev => {
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

    const handleIconDragEnd = useCallback(() => {
        setDragIdx(null);
        setOverIdx(null);
    }, []);

    const handleDeleteCustom = useCallback((id: string) => {
        setCustomIcons(prev => {
            const target = prev.find(i => i.id === id);
            // Confirm before removing: the library is small and easy to lose
            // an upload by accident. window.confirm is intentional here: same
            // surface as the existing profile delete dialog and zero new i18n.
            const label = target?.name || t('iconPicker.removeIcon');
            if (!window.confirm(t('iconPicker.confirmDelete', { name: label }))) {
                return prev;
            }
            const next = prev.filter(i => i.id !== id);
            persistCustomIcons(next);
            return next;
        });
    }, [t]);

    // Shared ingestion path for both file-picker uploads and drag & drop.
    // Accepts a Uint8Array of file bytes plus the original filename so we can
    // derive the extension and human-readable label the same way regardless
    // of how the file was supplied.
    const ingestIconBytes = useCallback(async (bytes: Uint8Array, filename: string) => {
        const ext = filename.split('.').pop()?.toLowerCase() || 'png';
        const baseName = filename.replace(/\.[^.]+$/, '') || 'Icon';

        let dataUrl: string | null = null;
        let type: 'svg' | 'raster' = 'raster';

        if (ext === 'svg') {
            let svgText: string;
            try {
                svgText = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
            } catch { return; }
            if (!svgText.trim().toLowerCase().includes('<svg')) return;
            dataUrl = `data:image/svg+xml;base64,${btoa(unescape(encodeURIComponent(svgText)))}`;
            type = 'svg';
        } else {
            const mimeMap: Record<string, string> = { jpg: 'image/jpeg', jpeg: 'image/jpeg', png: 'image/png', gif: 'image/gif', webp: 'image/webp', ico: 'image/x-icon' };
            const mime = mimeMap[ext] || 'image/png';
            const blob = new Blob([new Uint8Array(bytes)], { type: mime });
            const url = URL.createObjectURL(blob);
            dataUrl = await new Promise<string | null>((resolve) => {
                const img = new window.Image();
                const timeout = setTimeout(() => { URL.revokeObjectURL(url); resolve(null); }, 10000);
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

        if (!dataUrl) return;
        const newIcon: CustomIcon = {
            id: `icon_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`,
            name: baseName,
            dataUrl,
            type,
            createdAt: new Date().toISOString(),
        };
        setCustomIcons(prev => {
            const next = [newIcon, ...prev];
            persistCustomIcons(next);
            return next;
        });
        setTab('custom');
    }, []);

    const [dragActive, setDragActive] = useState(false);
    const handleDrop = useCallback(async (e: React.DragEvent) => {
        e.preventDefault();
        e.stopPropagation();
        setDragActive(false);
        if (uploading) return;
        const file = e.dataTransfer?.files?.[0];
        if (!file) return;
        const allowed = /\.(svg|png|jpe?g|gif|webp|ico)$/i;
        if (!allowed.test(file.name)) return;
        setUploading(true);
        try {
            const arr = new Uint8Array(await file.arrayBuffer());
            await ingestIconBytes(arr, file.name);
        } finally {
            setUploading(false);
        }
    }, [uploading, ingestIconBytes]);

    const handleUpload = useCallback(async () => {
        if (uploading) return;
        setUploading(true);
        try {
            const selected = await open({
                multiple: false,
                filters: [{ name: 'Images', extensions: ['svg', 'png', 'jpg', 'jpeg', 'ico', 'webp', 'gif'] }],
            });
            if (!selected) return;
            const filePath = Array.isArray(selected) ? selected[0] : selected;
            const bytes = await readFile(filePath);
            const filename = filePath.split(/[/\\]/).pop() || 'icon.png';
            await ingestIconBytes(bytes, filename);
        } finally {
            setUploading(false);
        }
    }, [uploading, ingestIconBytes]);

    // Esc closes
    useEffect(() => {
        const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        window.addEventListener('keydown', onKey);
        return () => window.removeEventListener('keydown', onKey);
    }, [onClose]);

    return (
        <div
            className="fixed inset-0 bg-black/50 backdrop-blur-sm flex items-center justify-center z-50"
            role="dialog"
            aria-modal="true"
            aria-label={t('iconPicker.title')}
            onClick={onClose}
        >
            <div
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-[640px] max-w-[92vw] h-[560px] max-h-[90vh] border border-gray-200 dark:border-gray-700 flex flex-col overflow-hidden"
                onClick={(e) => e.stopPropagation()}
            >
                {/* Header */}
                <div className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <ImageIcon size={18} className="text-blue-500" />
                        <h3 className="text-base font-semibold text-gray-900 dark:text-gray-100">
                            {t('iconPicker.title')}
                        </h3>
                    </div>
                    <button
                        onClick={onClose}
                        className="p-1.5 rounded-lg text-gray-400 hover:text-gray-600 dark:hover:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
                        aria-label={t('common.close')}
                    >
                        <X size={16} />
                    </button>
                </div>

                {/* Tabs */}
                <div className="flex items-center gap-1 px-5 pt-3">
                    {(['shipped', 'custom'] as Tab[]).map(id => (
                        <button
                            key={id}
                            onClick={() => setTab(id)}
                            className={`px-3 py-1.5 text-sm font-medium rounded-lg transition-colors ${
                                tab === id
                                    ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400'
                                    : 'text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700/50'
                            }`}
                        >
                            {id === 'shipped' ? t('iconPicker.tabShipped') : t('iconPicker.tabCustom')}
                            {id === 'custom' && customIcons.length > 0 && (
                                <span className="ml-1.5 text-[10px] tabular-nums px-1.5 py-0.5 rounded-full bg-gray-100 dark:bg-gray-700 text-gray-500 dark:text-gray-400">
                                    {customIcons.length}
                                </span>
                            )}
                        </button>
                    ))}
                </div>

                {/* Search + sort toggle (shipped tab only) */}
                {tab === 'shipped' && (
                    <div className="px-5 pt-2 space-y-2">
                        <div className="relative">
                            <Search size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-gray-400" />
                            <input
                                type="text"
                                value={search}
                                onChange={(e) => setSearch(e.target.value)}
                                placeholder={t('iconPicker.search')}
                                className="w-full pl-9 pr-3 py-2 text-sm bg-gray-50 dark:bg-gray-900/50 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-700 dark:text-gray-200 placeholder-gray-400 focus:outline-none focus:ring-2 focus:ring-blue-500/30"
                            />
                        </div>
                        <div className="flex items-center gap-2 text-[11px]">
                            <span className="text-gray-500 dark:text-gray-400">{t('iconPicker.sort')}:</span>
                            <div className="inline-flex items-center rounded-md border border-gray-200 dark:border-gray-700 overflow-hidden">
                                <button
                                    type="button"
                                    onClick={() => setShippedSort('popularity')}
                                    aria-pressed={shippedSort === 'popularity'}
                                    className={`px-2 py-1 transition-colors ${
                                        shippedSort === 'popularity'
                                            ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400 font-medium'
                                            : 'text-gray-600 dark:text-gray-400 hover:bg-gray-50 dark:hover:bg-gray-700/50'
                                    }`}
                                >
                                    {t('iconPicker.sortPopularity')}
                                </button>
                                <button
                                    type="button"
                                    onClick={() => setShippedSort('alphabetical')}
                                    aria-pressed={shippedSort === 'alphabetical'}
                                    className={`px-2 py-1 border-l border-gray-200 dark:border-gray-700 transition-colors ${
                                        shippedSort === 'alphabetical'
                                            ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400 font-medium'
                                            : 'text-gray-600 dark:text-gray-400 hover:bg-gray-50 dark:hover:bg-gray-700/50'
                                    }`}
                                >
                                    {t('iconPicker.sortAlphabetical')}
                                </button>
                            </div>
                        </div>
                    </div>
                )}

                {/* Content */}
                <div className="flex-1 min-h-0 overflow-y-auto px-5 py-4 custom-scroll-area">
                    {tab === 'shipped' ? (
                        filteredShipped.length === 0 ? (
                            <div className="text-center py-12 text-gray-400 dark:text-gray-500">
                                <Search size={32} className="mx-auto mb-3 opacity-50" />
                                <p className="text-sm">{t('iconPicker.noResults')}</p>
                            </div>
                        ) : (
                            filteredShipped.map(cat => (
                                <div key={cat.id} className="mb-5 last:mb-0">
                                    <h4 className="text-[11px] uppercase tracking-wider font-semibold text-gray-400 dark:text-gray-500 mb-2 px-1">
                                        {t(cat.labelKey)}
                                    </h4>
                                    <div className="grid grid-cols-3 sm:grid-cols-4 md:grid-cols-5 gap-2">
                                        {cat.items.map(it => (
                                            <button
                                                key={it.key}
                                                onClick={() => handleSelectShipped(it.Logo)}
                                                className="flex flex-col items-center gap-1.5 p-2 rounded-lg border border-gray-200 dark:border-gray-700 hover:border-blue-400 dark:hover:border-blue-500/60 hover:bg-blue-50 dark:hover:bg-blue-900/20 transition-colors"
                                                title={it.name}
                                            >
                                                <div className="w-9 h-9 flex items-center justify-center">
                                                    <it.Logo size={32} />
                                                </div>
                                                <span className="text-[11px] text-gray-600 dark:text-gray-300 truncate max-w-full">
                                                    {it.name}
                                                </span>
                                            </button>
                                        ))}
                                    </div>
                                </div>
                            ))
                        )
                    ) : (
                        <>
                            <button
                                onClick={handleUpload}
                                disabled={uploading}
                                onDragOver={(e) => { e.preventDefault(); e.stopPropagation(); setDragActive(true); }}
                                onDragEnter={(e) => { e.preventDefault(); e.stopPropagation(); setDragActive(true); }}
                                onDragLeave={(e) => { e.preventDefault(); e.stopPropagation(); setDragActive(false); }}
                                onDrop={handleDrop}
                                className={`flex items-center justify-center gap-2 w-full px-4 py-3 mb-3 border-2 border-dashed rounded-lg text-sm font-medium transition-colors disabled:opacity-50 ${
                                    dragActive
                                        ? 'border-blue-500 bg-blue-50 dark:bg-blue-900/30 text-blue-600 dark:text-blue-300'
                                        : 'border-gray-300 dark:border-gray-600 hover:border-blue-400 dark:hover:border-blue-500/60 hover:bg-blue-50 dark:hover:bg-blue-900/20 text-gray-600 dark:text-gray-300'
                                }`}
                            >
                                <Upload size={16} />
                                {dragActive ? t('iconPicker.dropHere') : t('iconPicker.upload')}
                                <span className="text-[11px] text-gray-400 dark:text-gray-500 font-normal">
                                    SVG, PNG, JPG, WEBP, GIF, ICO
                                </span>
                            </button>

                            {(currentIcon || rescanning || (onServerIcon && onServerIcon !== currentIcon)) && (
                                <div className="mb-3">
                                    <h4 className="text-[11px] uppercase tracking-wider font-semibold text-gray-400 dark:text-gray-500 mb-2 px-1">
                                        {t('iconPicker.currentSection')}
                                    </h4>
                                    <div className="space-y-2">
                                        {currentIcon && (
                                            <button
                                                onClick={() => { onSelect(currentIcon); onClose(); }}
                                                className="flex items-center gap-3 w-full p-2.5 rounded-lg border border-blue-200 dark:border-blue-500/40 bg-blue-50/30 dark:bg-blue-900/15 hover:bg-blue-50 dark:hover:bg-blue-900/30 transition-colors"
                                                title={t('iconPicker.inUseDescription')}
                                            >
                                                <div className="w-9 h-9 shrink-0 flex items-center justify-center rounded bg-white dark:bg-gray-700 border border-gray-200 dark:border-gray-600">
                                                    <img src={currentIcon} alt="" className="max-w-full max-h-full object-contain" />
                                                </div>
                                                <div className="flex-1 min-w-0 text-left">
                                                    <div className="text-sm font-medium text-gray-700 dark:text-gray-200 flex items-center gap-1.5">
                                                        {t('iconPicker.inUse')}
                                                        {inUseMatchesServer && (
                                                            <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-green-100 text-green-700 dark:bg-green-900/40 dark:text-green-300 font-medium">
                                                                {t('iconPicker.matchesServer')}
                                                            </span>
                                                        )}
                                                    </div>
                                                    <div className="text-[11px] text-gray-500 dark:text-gray-400 truncate">
                                                        {t('iconPicker.inUseDescription')}
                                                    </div>
                                                </div>
                                            </button>
                                        )}
                                        {rescanning && (
                                            <div className="flex items-center gap-3 w-full p-2.5 rounded-lg border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/50">
                                                <div className="w-9 h-9 shrink-0 flex items-center justify-center rounded bg-white dark:bg-gray-700 border border-gray-200 dark:border-gray-600 animate-pulse">
                                                    <ImageIcon size={18} className="text-gray-300 dark:text-gray-600" />
                                                </div>
                                                <div className="flex-1 min-w-0 text-left">
                                                    <div className="text-sm font-medium text-gray-500 dark:text-gray-400">
                                                        {t('iconPicker.scanning')}
                                                    </div>
                                                    <div className="text-[11px] text-gray-400 dark:text-gray-500 truncate">
                                                        {t('iconPicker.scanningDescription')}
                                                    </div>
                                                </div>
                                            </div>
                                        )}
                                        {!rescanning && onServerIcon && onServerIcon !== currentIcon && (
                                            <button
                                                onClick={() => { onSelect(onServerIcon); onClose(); }}
                                                className="flex items-center gap-3 w-full p-2.5 rounded-lg border border-emerald-200 dark:border-emerald-500/40 bg-emerald-50/30 dark:bg-emerald-900/15 hover:bg-emerald-50 dark:hover:bg-emerald-900/30 transition-colors"
                                                title={onServerDescription}
                                            >
                                                <div className="w-9 h-9 shrink-0 flex items-center justify-center rounded bg-white dark:bg-gray-700 border border-gray-200 dark:border-gray-600">
                                                    <img src={onServerIcon} alt="" className="max-w-full max-h-full object-contain" />
                                                </div>
                                                <div className="flex-1 min-w-0 text-left">
                                                    <div className="text-sm font-medium text-gray-700 dark:text-gray-200">
                                                        {onServerLabel}
                                                    </div>
                                                    <div className="text-[11px] text-gray-500 dark:text-gray-400 truncate">
                                                        {onServerDescription}
                                                    </div>
                                                </div>
                                            </button>
                                        )}
                                    </div>
                                </div>
                            )}

                            {customIcons.length === 0 ? (
                                // Suppress the "No custom icons yet" placeholder
                                // when the Current/On-server section above is
                                // already showing something: otherwise users
                                // who upgraded from an older release see both
                                // their existing icon AND a contradictory
                                // "library is empty" message at the same time.
                                (currentIcon || onServerIcon) ? null : (
                                    <div className="text-center py-10 text-gray-400 dark:text-gray-500">
                                        <ImageIcon size={32} className="mx-auto mb-3 opacity-50" />
                                        <p className="text-sm mb-1">{t('iconPicker.empty')}</p>
                                        <p className="text-xs">{t('iconPicker.emptyHint')}</p>
                                    </div>
                                )
                            ) : (
                                <div className="grid grid-cols-3 sm:grid-cols-4 md:grid-cols-5 gap-2">
                                    {customIcons.map((icon, idx) => (
                                        <div
                                            key={icon.id}
                                            draggable
                                            onDragStart={handleIconDragStart(idx)}
                                            onDragOver={handleIconDragOver(idx)}
                                            onDrop={handleIconDrop(idx)}
                                            onDragEnd={handleIconDragEnd}
                                            title={t('iconPicker.dragHint')}
                                            className={`group relative flex flex-col items-center gap-1.5 p-2 rounded-lg border transition-colors cursor-grab active:cursor-grabbing ${
                                                dragIdx === idx
                                                    ? 'opacity-50 border-blue-300 dark:border-blue-500/40'
                                                    : overIdx === idx && dragIdx !== null && dragIdx !== idx
                                                        ? 'border-blue-500 bg-blue-50 dark:bg-blue-900/30 ring-2 ring-blue-400/40'
                                                        : 'border-gray-200 dark:border-gray-700 hover:border-blue-400 dark:hover:border-blue-500/60 hover:bg-blue-50 dark:hover:bg-blue-900/20'
                                            }`}
                                        >
                                            <button
                                                onClick={() => handleSelectCustom(icon)}
                                                className="flex flex-col items-center gap-1.5 w-full"
                                                title={icon.name}
                                            >
                                                <div className="w-9 h-9 flex items-center justify-center">
                                                    <img src={icon.dataUrl} alt="" className="max-w-full max-h-full object-contain" />
                                                </div>
                                                <span className="text-[11px] text-gray-600 dark:text-gray-300 truncate max-w-full">
                                                    {icon.name}
                                                </span>
                                            </button>
                                            <button
                                                onClick={(e) => { e.stopPropagation(); handleDeleteCustom(icon.id); }}
                                                className="absolute top-1 right-1 p-1 rounded text-gray-400 hover:text-red-500 hover:bg-red-50 dark:hover:bg-red-900/30 opacity-0 group-hover:opacity-100 transition-opacity"
                                                title={t('iconPicker.removeIcon')}
                                            >
                                                <Trash2 size={12} />
                                            </button>
                                        </div>
                                    ))}
                                </div>
                            )}
                        </>
                    )}
                </div>

                {/* Footer */}
                <div className="flex justify-end gap-2 px-5 py-3 border-t border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/50">
                    <button
                        onClick={onClose}
                        className="px-4 py-2 text-sm text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                    >
                        {t('common.cancel')}
                    </button>
                </div>
            </div>
        </div>
    );
}
