// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { FileText } from 'lucide-react';
import { getThumbnail, keyFor, putThumbnail } from '../utils/thumbnailCache';

interface Props {
  path: string;
  name: string;
  size?: number;
  className?: string;
  /** `signatureOf(size, modified)`; without it this thumbnail is not cached. */
  signature?: string | null;
  /** Separates one remote session's paths from another's. */
  cacheScope?: string;
  /**
   * Rendered when the provider's own thumbnail endpoint fails or returns
   * nothing. A provider that advertises `provider_supports_thumbnails` can still
   * refuse an individual file, and this used to end at a generic document icon —
   * so a whole Icons view could look empty of previews while the file itself was
   * perfectly readable. Callers pass an `ImageThumbnail` here so the second
   * route is tried before giving up.
   */
  fallback?: React.ReactNode;
}

// Re-exported so existing callers keep working; the cache itself is now the one
// in `utils/thumbnailCache`, shared with ImageThumbnail and bounded in bytes.
export { clearThumbnailCache } from '../utils/thumbnailCache';

export function ProviderThumbnail({ path, name, size = 48, className, signature, cacheScope, fallback }: Props) {
  const cacheKey = keyFor(cacheScope ?? 'provider', path, signature);
  const [src, setSrc] = useState<string | null>(getThumbnail(cacheKey) ?? null);
  const [failed, setFailed] = useState(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  useEffect(() => {
    const cached = getThumbnail(cacheKey);
    if (cached) {
      setSrc(cached);
      setFailed(false);
      return;
    }

    let cancelled = false;
    (async () => {
      try {
        const base64 = await invoke<string>('provider_get_thumbnail', { path });
        if (!cancelled && mounted.current) {
          putThumbnail(cacheKey, base64);
          setSrc(base64);
        }
      } catch {
        if (!cancelled && mounted.current) {
          setFailed(true);
        }
      }
    })();

    return () => { cancelled = true; };
  }, [path, cacheKey]);

  if (failed || !src) {
    if (fallback) return <>{fallback}</>;
    return (
      <div className={`flex items-center justify-center ${className || ''}`} style={{ width: size, height: size }}>
        <FileText size={size * 0.6} className="text-gray-400" />
      </div>
    );
  }

  const imgSrc = src.startsWith('data:') ? src : `data:image/jpeg;base64,${src}`;

  return (
    <img
      src={imgSrc}
      alt={name}
      className={`object-cover rounded ${className || ''}`}
      style={{ width: size, height: size }}
      onError={() => setFailed(true)}
    />
  );
}
