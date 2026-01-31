import { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { FileText } from 'lucide-react';

// In-memory cache for thumbnails (cleared on navigation)
const thumbnailCache = new Map<string, string>();

interface Props {
  path: string;
  name: string;
  size?: number;
  className?: string;
}

export function clearThumbnailCache() {
  thumbnailCache.clear();
}

export function ProviderThumbnail({ path, name, size = 48, className }: Props) {
  const [src, setSrc] = useState<string | null>(thumbnailCache.get(path) || null);
  const [failed, setFailed] = useState(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  useEffect(() => {
    if (thumbnailCache.has(path)) {
      setSrc(thumbnailCache.get(path)!);
      return;
    }

    let cancelled = false;
    (async () => {
      try {
        const base64 = await invoke<string>('provider_get_thumbnail', { path });
        if (!cancelled && mounted.current) {
          thumbnailCache.set(path, base64);
          setSrc(base64);
        }
      } catch {
        if (!cancelled && mounted.current) {
          setFailed(true);
        }
      }
    })();

    return () => { cancelled = true; };
  }, [path]);

  if (failed || !src) {
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
