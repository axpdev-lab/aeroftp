/**
 * ImageThumbnail - Lazy-loads image thumbnails for file grid view
 */

import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface ImageThumbnailProps {
    path: string;
    name: string;
    fallbackIcon: React.ReactNode;
    isRemote?: boolean;
    className?: string;
}

export const ImageThumbnail: React.FC<ImageThumbnailProps> = ({
    path,
    name,
    fallbackIcon,
    isRemote = false,
    className
}) => {
    const [src, setSrc] = useState<string | null>(null);
    const [error, setError] = useState(false);

    useEffect(() => {
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
                setSrc(`data:${mime};base64,${base64}`);
            } catch {
                if (!cancelled) setError(true);
            }
        };
        loadImage();
        return () => { cancelled = true; };
    }, [path, name, isRemote]);

    if (error || !src) {
        return <div className="file-grid-icon">{fallbackIcon}</div>;
    }
    return <img src={src} alt={name} className={className || "file-grid-thumbnail"} />;
};

export default ImageThumbnail;
