import * as React from 'react';
import { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { X, ChevronLeft, ChevronRight, FileText, Folder, Image, Music, Film, FileCode, File, ExternalLink } from 'lucide-react';
import { formatBytes } from '../utils/formatters';
import { getMimeType } from '../components/Preview/utils/fileTypes';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface LocalFile {
  name: string;
  path: string;
  size: number | null;
  is_dir: boolean;
  modified: string | null;
}

interface QuickLookOverlayProps {
  file: LocalFile;
  allFiles: LocalFile[];
  currentIndex: number;
  currentPath: string;
  onClose: () => void;
  onNavigate: (index: number) => void;
  t: (key: string, params?: Record<string, string | number>) => string;
}

// ---------------------------------------------------------------------------
// File type detection
// ---------------------------------------------------------------------------

const IMAGE_EXTS = /\.(jpg|jpeg|png|gif|svg|webp|bmp|ico|tiff?)$/i;
const VIDEO_EXTS = /\.(mp4|webm|mov|avi|mkv|flv|wmv|m4v)$/i;
const AUDIO_EXTS = /\.(mp3|wav|ogg|flac|aac|wma|m4a|opus)$/i;
const CODE_EXTS = /\.(js|jsx|ts|tsx|py|rs|go|java|c|cpp|h|hpp|cs|rb|php|swift|kt|scala|sh|bash|zsh|fish|ps1|bat|cmd|sql|graphql|yaml|yml|toml|ini|conf|env|dockerfile|makefile|cmake|gradle)$/i;
const TEXT_EXTS = /\.(txt|md|markdown|rst|log|csv|tsv|xml|html|htm|css|scss|sass|less|json|jsonc|json5)$/i;

type PreviewType = 'image' | 'video' | 'audio' | 'code' | 'text' | 'markdown' | 'folder' | 'unknown';

function getPreviewType(file: LocalFile): PreviewType {
  if (file.is_dir) return 'folder';
  const name = file.name;
  if (IMAGE_EXTS.test(name)) return 'image';
  if (VIDEO_EXTS.test(name)) return 'video';
  if (AUDIO_EXTS.test(name)) return 'audio';
  if (name.endsWith('.md') || name.endsWith('.markdown')) return 'markdown';
  if (CODE_EXTS.test(name)) return 'code';
  if (TEXT_EXTS.test(name)) return 'text';
  return 'unknown';
}

function getFileIcon(type: PreviewType) {
  switch (type) {
    case 'image': return <Image size={64} className="text-green-400" />;
    case 'video': return <Film size={64} className="text-purple-400" />;
    case 'audio': return <Music size={64} className="text-pink-400" />;
    case 'code': return <FileCode size={64} className="text-blue-400" />;
    case 'text': case 'markdown': return <FileText size={64} className="text-gray-400" />;
    case 'folder': return <Folder size={64} className="text-yellow-400" />;
    default: return <File size={64} className="text-gray-500" />;
  }
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export const QuickLookOverlay: React.FC<QuickLookOverlayProps> = ({
  file,
  allFiles,
  currentIndex,
  currentPath,
  onClose,
  onNavigate,
  t,
}) => {
  const [content, setContent] = useState<string | null>(null);
  const [blobUrl, setBlobUrl] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const prevBlobRef = useRef<string | null>(null);

  const previewType = getPreviewType(file);
  const nonDirFiles = allFiles.filter(f => !f.is_dir);
  const totalFiles = nonDirFiles.length;
  const fileIndex = nonDirFiles.findIndex(f => f.name === file.name) + 1;

  // Cleanup blob URLs on unmount to prevent memory leaks from
  // large base64 media content (video/audio) held in object URLs.
  useEffect(() => {
    return () => {
      if (prevBlobRef.current) {
        URL.revokeObjectURL(prevBlobRef.current);
        prevBlobRef.current = null;
      }
    };
  }, []);

  // Load content when file changes — uses per-load cancellation flag
  // to prevent race conditions with overlapping async loads.
  useEffect(() => {
    let cancelled = false;
    setContent(null);
    setError(null);
    setLoading(true);

    // Revoke previous blob
    if (prevBlobRef.current) {
      URL.revokeObjectURL(prevBlobRef.current);
      prevBlobRef.current = null;
      setBlobUrl(null);
    }

    const filePath = file.path || `${currentPath}/${file.name}`;

    const loadContent = async () => {
      try {
        if (previewType === 'folder' || previewType === 'unknown') {
          if (!cancelled) setLoading(false);
          return;
        }

        if (previewType === 'image') {
          const base64 = await invoke<string>('read_file_base64', { path: filePath, maxSizeMb: 20 });
          if (cancelled) return;
          const ext = file.name.split('.').pop()?.toLowerCase() || 'png';
          const mimeMap: Record<string, string> = {
            jpg: 'image/jpeg', jpeg: 'image/jpeg', png: 'image/png',
            gif: 'image/gif', svg: 'image/svg+xml', webp: 'image/webp',
            bmp: 'image/bmp', ico: 'image/x-icon', tif: 'image/tiff', tiff: 'image/tiff',
          };
          // SECURITY: SVG files are rendered via <img src="data:image/svg+xml;base64,...">
          // which is safe — browsers do NOT execute <script> tags or event handlers
          // embedded in SVGs when loaded through <img> tags. This is a fundamental
          // browser security restriction (same-origin policy for img elements).
          // NEVER use <object>, <embed>, or <iframe src="data:..."> for SVG rendering,
          // as those DO execute embedded scripts.
          setContent(`data:${mimeMap[ext] || 'image/png'};base64,${base64}`);
        } else if (previewType === 'video' || previewType === 'audio') {
          const base64 = await invoke<string>('read_local_file_base64', { path: filePath, maxSizeMb: 20 });
          if (cancelled) return;
          const mime = getMimeType(file.name);
          const binary = atob(base64);
          const bytes = new Uint8Array(binary.length);
          for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
          const blob = new Blob([bytes], { type: mime });
          const url = URL.createObjectURL(blob);
          prevBlobRef.current = url;
          setBlobUrl(url);
        } else {
          // text, code, markdown
          const text = await invoke<string>('read_local_file', { path: filePath, maxSizeMb: 5 });
          if (cancelled) return;
          // Limit to 50KB for display
          setContent(text.length > 51200 ? text.substring(0, 51200) + '\n\n... (truncated)' : text);
        }
      } catch (err) {
        if (!cancelled) setError(String(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    };

    loadContent();
    return () => { cancelled = true; };
  }, [file.name, file.path, currentPath, previewType]);

  // Keyboard navigation
  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
      } else if (e.key === ' ') {
        // Don't close on spacebar during media playback
        const currentPreviewType = getPreviewType(file);
        if (currentPreviewType === 'video' || currentPreviewType === 'audio') return;
        e.preventDefault();
        onClose();
      } else if (e.key === 'ArrowRight' || e.key === 'ArrowDown') {
        e.preventDefault();
        // Skip directories
        let next = currentIndex + 1;
        while (next < allFiles.length && allFiles[next].is_dir) next++;
        if (next < allFiles.length) onNavigate(next);
      } else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') {
        e.preventDefault();
        // Skip directories
        let prev = currentIndex - 1;
        while (prev >= 0 && allFiles[prev].is_dir) prev--;
        if (prev >= 0) onNavigate(prev);
      }
    };
    window.addEventListener('keydown', handleKey);
    return () => window.removeEventListener('keydown', handleKey);
  }, [currentIndex, allFiles, file, onClose, onNavigate]);

  const formatDate = (dateStr: string | null): string => {
    if (!dateStr) return '';
    try { return new Date(dateStr).toLocaleString(); } catch { return dateStr; }
  };

  return (
    <div
      className="fixed inset-0 z-[60] flex items-center justify-center bg-black/70 backdrop-blur-sm"
      onClick={onClose}
      role="dialog"
      aria-label={`Quick Look: ${file.name}`}
      aria-modal="true"
    >
      <div
        className="bg-gray-900 rounded-xl shadow-2xl border border-gray-700/50 w-[80vw] max-w-4xl h-[80vh] max-h-[800px] flex flex-col overflow-hidden animate-scale-in"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-2.5 border-b border-gray-700/50 bg-gray-800/80 shrink-0">
          <div className="flex items-center gap-2 min-w-0">
            <span className="text-sm font-medium text-gray-200 truncate">{file.name}</span>
            {!file.is_dir && (
              <span className="text-xs text-gray-500">{fileIndex} of {totalFiles}</span>
            )}
          </div>
          <div className="flex items-center gap-1">
            <button
              onClick={(e) => {
                e.stopPropagation();
                let prev = currentIndex - 1;
                while (prev >= 0 && allFiles[prev].is_dir) prev--;
                if (prev >= 0) onNavigate(prev);
              }}
              disabled={(() => { let p = currentIndex - 1; while (p >= 0 && allFiles[p].is_dir) p--; return p < 0; })()}
              className="p-1.5 rounded hover:bg-gray-700 disabled:opacity-30 text-gray-400 hover:text-gray-200 transition-colors"
              aria-label="Previous file"
            >
              <ChevronLeft size={16} />
            </button>
            <button
              onClick={(e) => {
                e.stopPropagation();
                let next = currentIndex + 1;
                while (next < allFiles.length && allFiles[next].is_dir) next++;
                if (next < allFiles.length) onNavigate(next);
              }}
              disabled={(() => { let n = currentIndex + 1; while (n < allFiles.length && allFiles[n].is_dir) n++; return n >= allFiles.length; })()}
              className="p-1.5 rounded hover:bg-gray-700 disabled:opacity-30 text-gray-400 hover:text-gray-200 transition-colors"
              aria-label="Next file"
            >
              <ChevronRight size={16} />
            </button>
            <button
              onClick={(e) => {
                e.stopPropagation();
                const filePath = file.path || `${currentPath}/${file.name}`;
                invoke('open_in_file_manager', { path: filePath }).catch(() => {});
              }}
              className="p-1.5 rounded hover:bg-gray-700 text-gray-400 hover:text-gray-200 transition-colors ml-2"
              aria-label={t('contextMenu.openInFileManager')}
              title={t('contextMenu.openInFileManager')}
            >
              <ExternalLink size={16} />
            </button>
            <button
              onClick={onClose}
              className="p-1.5 rounded hover:bg-gray-700 text-gray-400 hover:text-gray-200 transition-colors"
              aria-label="Close preview"
            >
              <X size={16} />
            </button>
          </div>
        </div>

        {/* Content */}
        <div className="flex-1 min-h-0 flex items-center justify-center p-4 overflow-auto">
          {loading ? (
            <div className="flex flex-col items-center gap-3 text-gray-400">
              <div className="w-8 h-8 border-2 border-gray-600 border-t-blue-500 rounded-full animate-spin" />
              <span className="text-sm">{t('common.loading')}</span>
            </div>
          ) : error ? (
            <div className="flex flex-col items-center gap-3 text-red-400">
              {getFileIcon(previewType)}
              <span className="text-sm">{error}</span>
            </div>
          ) : previewType === 'image' && content ? (
            <img src={content} alt={file.name} className="max-w-full max-h-full object-contain rounded" />
          ) : previewType === 'video' && blobUrl ? (
            <video src={blobUrl} controls autoPlay className="max-w-full max-h-full rounded" />
          ) : previewType === 'audio' && blobUrl ? (
            <div className="flex flex-col items-center gap-6">
              {getFileIcon('audio')}
              <audio src={blobUrl} controls autoPlay className="w-80" />
            </div>
          ) : (previewType === 'code' || previewType === 'text' || previewType === 'markdown') && content !== null ? (
            <pre className="w-full h-full overflow-auto text-sm text-gray-300 font-mono bg-gray-950 rounded-lg p-4 whitespace-pre-wrap break-words">
              {content}
            </pre>
          ) : (
            <div className="flex flex-col items-center gap-3">
              {getFileIcon(previewType)}
              <span className="text-gray-400 text-sm">
                {file.is_dir ? t('common.folder') : t('quickLook.noPreview')}
              </span>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center gap-4 px-4 py-2 border-t border-gray-700/50 bg-gray-800/80 text-xs text-gray-400 shrink-0">
          {!file.is_dir && file.size != null && (
            <span>{formatBytes(file.size)}</span>
          )}
          <span>{getMimeType(file.name)}</span>
          {file.modified && <span>{formatDate(file.modified)}</span>}
        </div>
      </div>
    </div>
  );
};

export default QuickLookOverlay;
