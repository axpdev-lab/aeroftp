/**
 * usePreview Hook
 * Extracted from App.tsx during modularization (v1.3.1)
 *
 * Manages three preview systems:
 *   1. Sidebar preview - file info panel with image thumbnail (base64 loaded via invoke)
 *   2. DevTools code editor - Monaco-based source viewer for code files
 *   3. Universal media preview - modal for images, audio, video, PDF
 *
 * Props: notify (for error/info toasts), toast (for removing loading toasts)
 * Returns: All preview state + openDevToolsPreview, openUniversalPreview, closeUniversalPreview
 */

import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { LocalFile, RemoteFile } from '../types';
import { PreviewFile } from '../components/DevTools';
import { PreviewFileData, getPreviewCategory } from '../components/Preview';
import { logger } from '../utils/logger';

interface UsePreviewProps {
  notify: {
    error: (title: string, message?: string) => void;
    info: (title: string, message?: string) => string | null | undefined;
  };
  toast: {
    removeToast: (id: string) => void;
  };
}

export const usePreview = ({ notify, toast }: UsePreviewProps) => {
  // Sidebar preview
  const [showLocalPreview, setShowLocalPreview] = useState(false);
  const [previewFile, setPreviewFile] = useState<LocalFile | null>(null);
  const [previewImageBase64, setPreviewImageBase64] = useState<string | null>(null);
  const [previewImageDimensions, setPreviewImageDimensions] = useState<{ width: number; height: number } | null>(null);

  // DevTools code editor
  const [devToolsOpen, setDevToolsOpen] = useState(false);
  const [devToolsPreviewFile, setDevToolsPreviewFile] = useState<PreviewFile | null>(null);

  // Universal media preview (images, audio, video, pdf)
  const [universalPreviewOpen, setUniversalPreviewOpen] = useState(false);
  const [universalPreviewFile, setUniversalPreviewFile] = useState<PreviewFileData | null>(null);

  // View mode
  const [viewMode, setViewMode] = useState<'list' | 'grid' | 'large'>('list');

  // Track current blob URL for cleanup on replacement
  const currentBlobUrlRef = useRef<string | null>(null);

  // Load preview image as base64
  useEffect(() => {
    const loadPreview = async () => {
      if (!previewFile) {
        setPreviewImageBase64(null);
        setPreviewImageDimensions(null);
        return;
      }
      if (/\.(jpg|jpeg|png|gif|svg|webp|bmp)$/i.test(previewFile.name)) {
        try {
          const base64: string = await invoke('read_file_base64', { path: previewFile.path });
          const ext = previewFile.name.split('.').pop()?.toLowerCase() || '';
          const mimeTypes: Record<string, string> = {
            jpg: 'image/jpeg', jpeg: 'image/jpeg', png: 'image/png',
            gif: 'image/gif', svg: 'image/svg+xml', webp: 'image/webp', bmp: 'image/bmp'
          };
          const mime = mimeTypes[ext] || 'image/png';
          const dataUrl = `data:${mime};base64,${base64}`;
          setPreviewImageBase64(dataUrl);
          // Extract image dimensions
          const img = new window.Image();
          img.onload = () => setPreviewImageDimensions({ width: img.naturalWidth, height: img.naturalHeight });
          img.onerror = () => setPreviewImageDimensions(null);
          img.src = dataUrl;
        } catch (error) {
          console.error('Failed to load preview:', error);
          setPreviewImageBase64(null);
          setPreviewImageDimensions(null);
        }
      } else {
        setPreviewImageBase64(null);
        setPreviewImageDimensions(null);
      }
    };
    loadPreview();
  }, [previewFile]);

  // Open code preview in DevTools
  const openDevToolsPreview = useCallback(async (file: RemoteFile | LocalFile, isRemote: boolean) => {
    try {
      let content = '';
      if (isRemote) {
        const remotePath = (file as RemoteFile).path;
        content = await invoke<string>('preview_remote_file', { path: remotePath });
      } else {
        const localPath = (file as LocalFile).path;
        content = await invoke<string>('read_local_file', { path: localPath });
      }

      setDevToolsPreviewFile({
        name: file.name,
        path: isRemote ? (file as RemoteFile).path : (file as LocalFile).path,
        content,
        mimeType: 'text/plain',
        size: file.size || 0,
        isRemote,
      });
      setDevToolsOpen(true);
    } catch (error) {
      notify.error('Preview Failed', String(error));
    }
  }, [notify]);

  // Open Universal Preview Modal (for media files)
  const openUniversalPreview = useCallback(async (file: RemoteFile | LocalFile, isRemote: boolean) => {
    try {
      const filePath = isRemote ? (file as RemoteFile).path : (file as LocalFile).path;
      let blobUrl: string | undefined;
      let content: string | undefined;

      const category = getPreviewCategory(file.name);
      const ext = file.name.split('.').pop()?.toLowerCase() || '';

      const fileSize = file.size || 0;
      const sizeMB = (fileSize / (1024 * 1024)).toFixed(1);
      const loadingToastId = fileSize > 1024 * 1024
        ? notify.info(`Loading ${file.name}`, `${sizeMB} MB - Please wait...`)
        : null;

      const mimeMap: Record<string, string> = {
        jpg: 'image/jpeg', jpeg: 'image/jpeg', png: 'image/png',
        gif: 'image/gif', webp: 'image/webp', svg: 'image/svg+xml',
        bmp: 'image/bmp', ico: 'image/x-icon',
        mp3: 'audio/mpeg', wav: 'audio/wav', ogg: 'audio/ogg',
        flac: 'audio/flac', aac: 'audio/aac', m4a: 'audio/mp4',
        mp4: 'video/mp4', webm: 'video/webm', mkv: 'video/x-matroska',
        avi: 'video/x-msvideo', mov: 'video/quicktime', ogv: 'video/ogg',
      };

      if (!isRemote) {
        if (category === 'text' || category === 'markdown') {
          content = await invoke<string>('read_local_file', { path: filePath });
        } else if (category === 'audio' || category === 'video') {
          logger.debug(`[Preview] Loading ${category} file as blob...`);
          const base64 = await invoke<string>('read_local_file_base64', { path: filePath });
          const mimeType = mimeMap[ext] || (category === 'audio' ? 'audio/mpeg' : 'video/mp4');
          const byteCharacters = atob(base64);
          const byteNumbers = new Array(byteCharacters.length);
          for (let i = 0; i < byteCharacters.length; i++) {
            byteNumbers[i] = byteCharacters.charCodeAt(i);
          }
          const byteArray = new Uint8Array(byteNumbers);
          const blob = new Blob([byteArray], { type: mimeType });
          blobUrl = URL.createObjectURL(blob);
          logger.debug(`[Preview] Created blob URL for ${category}`);
        } else {
          const base64 = await invoke<string>('read_local_file_base64', { path: filePath });
          const mimeType = mimeMap[ext] || 'application/octet-stream';
          const byteCharacters = atob(base64);
          const byteNumbers = new Array(byteCharacters.length);
          for (let i = 0; i < byteCharacters.length; i++) {
            byteNumbers[i] = byteCharacters.charCodeAt(i);
          }
          const byteArray = new Uint8Array(byteNumbers);
          const blob = new Blob([byteArray], { type: mimeType });
          blobUrl = URL.createObjectURL(blob);
        }
      } else {
        if (category === 'text' || category === 'markdown') {
          content = await invoke<string>('preview_remote_file', { path: filePath });
        } else if (category === 'image') {
          const base64 = await invoke<string>('ftp_read_file_base64', { path: filePath });
          blobUrl = `data:${mimeMap[ext] || 'image/png'};base64,${base64}`;
        }
      }

      if (loadingToastId) {
        toast.removeToast(loadingToastId);
      }

      // Revoke previous blob URL before replacing state (prevent memory leak)
      if (currentBlobUrlRef.current) {
        URL.revokeObjectURL(currentBlobUrlRef.current);
      }
      currentBlobUrlRef.current = blobUrl?.startsWith('blob:') ? blobUrl : null;

      setUniversalPreviewFile({
        name: file.name,
        path: filePath,
        size: file.size || 0,
        isRemote,
        content,
        blobUrl,
        mimeType: mimeMap[ext],
        modified: file.modified || undefined,
      });
      setUniversalPreviewOpen(true);
    } catch (error) {
      notify.error('Preview Failed', String(error));
    }
  }, [notify, toast]);

  // Close Universal Preview (cleanup blob URL)
  const closeUniversalPreview = useCallback(() => {
    if (currentBlobUrlRef.current) {
      URL.revokeObjectURL(currentBlobUrlRef.current);
      currentBlobUrlRef.current = null;
    }
    setUniversalPreviewOpen(false);
    setUniversalPreviewFile(null);
  }, []);

  return {
    // Sidebar preview
    showLocalPreview,
    setShowLocalPreview,
    previewFile,
    setPreviewFile,
    previewImageBase64,
    previewImageDimensions,

    // DevTools
    devToolsOpen,
    setDevToolsOpen,
    devToolsPreviewFile,
    setDevToolsPreviewFile,
    openDevToolsPreview,

    // Universal preview
    universalPreviewOpen,
    universalPreviewFile,
    openUniversalPreview,
    closeUniversalPreview,

    // View mode
    viewMode,
    setViewMode,
  };
};

export default usePreview;
