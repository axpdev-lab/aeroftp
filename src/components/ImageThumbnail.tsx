// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * ImageThumbnail - Lazy-loads image thumbnails for file grid view
 */

import React, { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getThumbnail, keyFor, putThumbnail } from '../utils/thumbnailCache';

const MAX_CONCURRENT_THUMBNAIL_READS = 4;
interface ThumbnailJob {
    cancelled: boolean;
    run: () => Promise<void>;
}
const thumbnailQueue: ThumbnailJob[] = [];
let activeThumbnailReads = 0;

function pumpThumbnailQueue(): void {
    while (activeThumbnailReads < MAX_CONCURRENT_THUMBNAIL_READS && thumbnailQueue.length > 0) {
        const job = thumbnailQueue.shift()!;
        if (job.cancelled) continue;
        activeThumbnailReads += 1;
        void job.run().finally(() => {
            activeThumbnailReads -= 1;
            pumpThumbnailQueue();
        });
    }
}

function scheduleThumbnailRead(run: () => Promise<void>): () => void {
    const job: ThumbnailJob = { cancelled: false, run };
    thumbnailQueue.push(job);
    pumpThumbnailQueue();
    return () => { job.cancelled = true; };
}

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
    const placeholderRef = useRef<HTMLDivElement>(null);
    const [mayLoad, setMayLoad] = useState(false);

    useEffect(() => {
        setSrc(getThumbnail(cacheKey) ?? null);
        setError(false);
        setMayLoad(false);
    }, [cacheKey]);

    // A duplicate scan can render thousands of image rows. Observe the cheap
    // placeholder and do not enqueue an IPC read until the row approaches the
    // viewport; the shared queue below then bounds the visible burst as well.
    useEffect(() => {
        if (src || error || mayLoad) return;
        const node = placeholderRef.current;
        if (!node || typeof IntersectionObserver === 'undefined') {
            setMayLoad(true);
            return;
        }
        const observer = new IntersectionObserver(([entry]) => {
            if (entry.isIntersecting) {
                setMayLoad(true);
                observer.disconnect();
            }
        }, { rootMargin: '160px' });
        observer.observe(node);
        return () => observer.disconnect();
    }, [src, error, mayLoad, cacheKey]);

    useEffect(() => {
        const cached = getThumbnail(cacheKey);
        if (cached) {
            setSrc(cached);
            setError(false);
            return;
        }
        if (!mayLoad) return;

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
        const cancelQueued = scheduleThumbnailRead(loadImage);
        return () => {
            cancelled = true;
            cancelQueued();
        };
    }, [path, name, isRemote, cacheKey, mayLoad]);

    if (error || !src) {
        return <div ref={placeholderRef} className="file-grid-icon">{fallbackIcon}</div>;
    }
    // `object-contain`: show the file whole. `cover` crops whatever does not fit
    // the square, which on a wide photo is both edges and on a screenshot is
    // usually the part that identifies it (discussion #347).
    return <img src={src} alt={name} className={className || "file-grid-thumbnail object-contain"} />;
};

export default ImageThumbnail;
