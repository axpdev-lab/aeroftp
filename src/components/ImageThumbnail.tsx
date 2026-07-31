// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * ImageThumbnail - Lazy-loads image thumbnails for file grid view
 */

import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getThumbnail, keyFor, putThumbnail } from '../utils/thumbnailCache';

interface ImageThumbnailProps {
    path: string;
    name: string;
    fallbackIcon: React.ReactNode;
    isRemote?: boolean;
    className?: string;
    /**
     * The file's identity in time, from `signatureOf(size, modified)`. Supplied,
     * the thumbnail is cached and survives a view switch or a walk out of the
     * directory and back; omitted, it is fetched on every mount, which is the
     * old behaviour and the safe one when a changed file cannot be told apart.
     */
    signature?: string | null;
    /**
     * What the path is relative to: the local disk, one remote session. Keeps two
     * sources that use the same path from sharing a cache entry.
     */
    cacheScope?: string;
}

export const ImageThumbnail: React.FC<ImageThumbnailProps> = ({
    path,
    name,
    fallbackIcon,
    isRemote = false,
    className,
    signature,
    cacheScope,
}) => {
    const cacheKey = keyFor(cacheScope ?? (isRemote ? 'remote' : 'local'), path, signature);
    // Seeded from the cache so a cached thumbnail paints on the first frame,
    // instead of flashing the fallback icon and swapping a moment later.
    const [src, setSrc] = useState<string | null>(() => getThumbnail(cacheKey) ?? null);
    const [error, setError] = useState(false);

    useEffect(() => {
        const cached = getThumbnail(cacheKey);
        if (cached) {
            setSrc(cached);
            setError(false);
            return;
        }

        let cancelled = false;
        const loadImage = async () => {
            try {
                const command = isRemote ? 'ftp_read_file_base64' : 'read_file_base64';
                const base64: string = await invoke(command, { path, maxSizeMb: 5 });
                if (cancelled) return;
                const ext = name.split('.').pop()?.toLowerCase() || '';
                const mimeTypes: Record<string, string> = {
                    jpg: 'image/jpeg', jpeg: 'image/jpeg', png: 'image/png',
                    gif: 'image/gif', svg: 'image/svg+xml', webp: 'image/webp',
                    bmp: 'image/bmp', ico: 'image/x-icon'
                };
                const mime = mimeTypes[ext] || 'image/png';
                const dataUrl = `data:${mime};base64,${base64}`;
                putThumbnail(cacheKey, dataUrl);
                setSrc(dataUrl);
            } catch {
                if (!cancelled) setError(true);
            }
        };
        loadImage();
        return () => { cancelled = true; };
    }, [path, name, isRemote, cacheKey]);

    if (error || !src) {
        return <div className="file-grid-icon">{fallbackIcon}</div>;
    }
    return <img src={src} alt={name} className={className || "file-grid-thumbnail"} />;
};

export default ImageThumbnail;
