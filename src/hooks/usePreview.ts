// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

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
import DOMPurify from 'dompurify';

/** Max file size (in bytes) for base64 media preview: 25 MB */
const MAX_PREVIEW_SIZE_BYTES = 25 * 1024 * 1024;

/**
 * Sanitize SVG content before it is rendered (as an <img>/blob data URL).
 * A regex "sanitizer" is trivially evaded by nested or oddly-cased tags,
 * entity tricks, and unquoted attributes; DOMPurify parses the SVG namespace
 * and removes <script>, <foreignObject>, event handlers, and javascript:
 * hrefs reliably. This keeps the defense correct if a consumer ever renders
 * the SVG inline instead of through <img>. (audit B-F03)
 */
function sanitizeSvg(svgContent: string): string {
  return DOMPurify.sanitize(svgContent, {
    USE_PROFILES: { svg: true, svgFilters: true },
  });
}

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
  // Gallery: same-folder, same-kind siblings so the arrows / ← → keys can slide
  // between the images in the open folder (#128). Empty / -1 = nothing to page.
  const [galleryFiles, setGalleryFiles] = useState<(RemoteFile | LocalFile)[]>([]);
  const [galleryIndex, setGalleryIndex] = useState(-1);
  const [galleryIsRemote, setGalleryIsRemote] = useState(false);

  // View mode
  const [viewMode, setViewMode] = useState<'list' | 'grid' | 'large'>('list');

  // Track current blob URL for cleanup on replacement
  const currentBlobUrlRef = useRef<string | null>(null);

  // Cleanup blob URL on unmount
  useEffect(() => {
    return () => {
      if (currentBlobUrlRef.current) {
        URL.revokeObjectURL(currentBlobUrlRef.current);
        currentBlobUrlRef.current = null;
      }
    };
  }, []);

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
          const base64: string = await invoke('read_file_base64', { path: previewFile.path, maxSizeMb: 20 });
          const ext = previewFile.name.split('.').pop()?.toLowerCase() || '';
          const mimeTypes: Record<string, string> = {
            jpg: 'image/jpeg', jpeg: 'image/jpeg', png: 'image/png',
            gif: 'image/gif', svg: 'image/svg+xml', webp: 'image/webp', bmp: 'image/bmp'
          };
          const mime = mimeTypes[ext] || 'image/png';
          let dataUrl: string;
          if (ext === 'svg') {
            // H27: Sanitize SVG to remove XSS vectors before preview
            const rawSvg = atob(base64);
            const cleanSvg = sanitizeSvg(rawSvg);
            dataUrl = `data:${mime};base64,${btoa(cleanSvg)}`;
          } else {
            dataUrl = `data:${mime};base64,${base64}`;
          }
          setPreviewImageBase64(dataUrl);
          // Extract image dimensions
          const img = new window.Image();
          img.onload = () => setPreviewImageDimensions({ width: img.naturalWidth, height: img.naturalHeight });
          img.onerror = () => setPreviewImageDimensions(null);
          img.src = dataUrl;
        } catch (error) {
          logger.error('Failed to load preview:', error);
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

  // Open the AeroTools editor from a file already shown in Universal Preview.
  // Lets any plain-text file (not just code) be edited: the read-only preview
  // surfaces an Edit button that routes here.
  const openDevToolsFromData = useCallback(async (pf: PreviewFileData) => {
    try {
      const content = pf.isRemote
        ? await invoke<string>('preview_remote_file', { path: pf.path })
        : await invoke<string>('read_local_file', { path: pf.path });
      setDevToolsPreviewFile({
        name: pf.name,
        path: pf.path,
        content,
        mimeType: 'text/plain',
        size: pf.size || 0,
        isRemote: pf.isRemote,
      });
      setDevToolsOpen(true);
    } catch (error) {
      notify.error('Preview Failed', String(error));
    }
  }, [notify]);

  // Open Universal Preview Modal (for media files)
  const loadAndShow = useCallback(async (file: RemoteFile | LocalFile, isRemote: boolean) => {
    const filePath = isRemote ? (file as RemoteFile).path : (file as LocalFile).path;
    const category = getPreviewCategory(file.name);
    const ext = file.name.split('.').pop()?.toLowerCase() || '';
    const fileSize = file.size || 0;
    const sizeMB = (fileSize / (1024 * 1024)).toFixed(1);

    // Header metadata shown regardless of load outcome (skeleton / error / ready).
    const baseFile: PreviewFileData = {
      name: file.name,
      path: filePath,
      size: fileSize,
      isRemote,
      modified: file.modified || undefined,
    };

    // Starting a new preview: free the previous blob URL.
    if (currentBlobUrlRef.current) {
      URL.revokeObjectURL(currentBlobUrlRef.current);
      currentBlobUrlRef.current = null;
    }

    // H29: reject binary preview over 25 MB (memory amplification). Show it in
    // the modal, not just a transient toast that hides in the activity log (#128).
    const needsBinaryPreview = category !== 'text' && category !== 'markdown' && category !== 'code';
    if (needsBinaryPreview && fileSize > MAX_PREVIEW_SIZE_BYTES) {
      setUniversalPreviewFile({ ...baseFile, error: `File too large for preview (${sizeMB} MB). Maximum is 25 MB.` });
      setUniversalPreviewOpen(true);
      return;
    }

    // Open the modal immediately with a skeleton, then fetch the bytes in the
    // background and swap them in when ready (#128).
    setUniversalPreviewFile({ ...baseFile, loading: true });
    setUniversalPreviewOpen(true);

    const mimeMap: Record<string, string> = {
      jpg: 'image/jpeg', jpeg: 'image/jpeg', png: 'image/png',
      gif: 'image/gif', webp: 'image/webp', svg: 'image/svg+xml',
      bmp: 'image/bmp', ico: 'image/x-icon',
      mp3: 'audio/mpeg', wav: 'audio/wav', ogg: 'audio/ogg',
      flac: 'audio/flac', aac: 'audio/aac', m4a: 'audio/mp4',
      mp4: 'video/mp4', webm: 'video/webm', mkv: 'video/x-matroska',
      avi: 'video/x-msvideo', mov: 'video/quicktime', ogv: 'video/ogg',
    };

    /** Helper: decode base64 to ArrayBuffer for Blob construction */
    const base64ToArrayBuffer = (b64: string): ArrayBuffer => {
      const byteCharacters = atob(b64);
      const byteArray = new Uint8Array(byteCharacters.length);
      for (let i = 0; i < byteCharacters.length; i++) {
        byteArray[i] = byteCharacters.charCodeAt(i);
      }
      return byteArray.buffer as ArrayBuffer;
    };

    try {
      let blobUrl: string | undefined;
      let content: string | undefined;

      if (!isRemote) {
        if (category === 'text' || category === 'markdown' || category === 'code') {
          content = await invoke<string>('read_local_file', { path: filePath });
        } else if (category === 'audio' || category === 'video') {
          const base64 = await invoke<string>('read_local_file_base64', { path: filePath });
          const mimeType = mimeMap[ext] || (category === 'audio' ? 'audio/mpeg' : 'video/mp4');
          const byteArray = base64ToArrayBuffer(base64);
          const blob = new Blob([byteArray], { type: mimeType });
          blobUrl = URL.createObjectURL(blob);
        } else {
          const base64 = await invoke<string>('read_local_file_base64', { path: filePath });
          const mimeType = mimeMap[ext] || 'application/octet-stream';
          // H27: Sanitize SVG content before creating blob
          if (ext === 'svg') {
            const rawSvg = atob(base64);
            const cleanSvg = sanitizeSvg(rawSvg);
            const blob = new Blob([cleanSvg], { type: mimeType });
            blobUrl = URL.createObjectURL(blob);
          } else {
            const byteArray = base64ToArrayBuffer(base64);
            const blob = new Blob([byteArray], { type: mimeType });
            blobUrl = URL.createObjectURL(blob);
          }
        }
      } else {
        if (category === 'text' || category === 'markdown' || category === 'code') {
          content = await invoke<string>('preview_remote_file', { path: filePath });
        } else if (category === 'image') {
          // #128 item B: pass the UI's preview cap so the backend uses the same
          // 25 MB limit (it previously hard-capped at 10 MB, rejecting full-res
          // photos the UI had already accepted).
          const base64 = await invoke<string>('ftp_read_file_base64', { path: filePath, maxSizeMb: MAX_PREVIEW_SIZE_BYTES / (1024 * 1024) });
          // H27: Sanitize SVG content from remote sources
          if (ext === 'svg') {
            const rawSvg = atob(base64);
            const cleanSvg = sanitizeSvg(rawSvg);
            blobUrl = `data:${mimeMap[ext] || 'image/svg+xml'};base64,${btoa(cleanSvg)}`;
          } else {
            blobUrl = `data:${mimeMap[ext] || 'image/png'};base64,${base64}`;
          }
        }
      }

      currentBlobUrlRef.current = blobUrl?.startsWith('blob:') ? blobUrl : null;
      setUniversalPreviewFile({ ...baseFile, mimeType: mimeMap[ext], content, blobUrl, loading: false });
    } catch (error) {
      // Surface the failure inside the modal, not only as a toast that hides in
      // the activity log (#128).
      setUniversalPreviewFile({ ...baseFile, error: String(error), loading: false });
    }
  }, []);

  // Open a preview and, when the folder holds other files of the SAME kind,
  // build a gallery so the on-screen arrows, the toolbar buttons and the ← →
  // keys can slide between them (#128). `siblings` is the panel's displayed list.
  const openUniversalPreview = useCallback(async (
    file: RemoteFile | LocalFile,
    isRemote: boolean,
    siblings?: (RemoteFile | LocalFile)[],
  ) => {
    const pathOf = (f: RemoteFile | LocalFile) => (f as { path: string }).path;
    const openedCategory = getPreviewCategory(file.name);
    const gallery = (siblings ?? []).filter((f) => {
      const entry = f as { is_dir?: boolean; name: string };
      return !entry.is_dir && getPreviewCategory(entry.name) === openedCategory;
    });
    const idx = gallery.findIndex((f) => pathOf(f) === pathOf(file));
    if (gallery.length > 1 && idx >= 0) {
      setGalleryFiles(gallery);
      setGalleryIndex(idx);
      setGalleryIsRemote(isRemote);
    } else {
      setGalleryFiles([]);
      setGalleryIndex(-1);
    }
    await loadAndShow(file, isRemote);
  }, [loadAndShow]);

  const hasPreviewPrevious = galleryIndex > 0;
  const hasPreviewNext = galleryIndex >= 0 && galleryIndex < galleryFiles.length - 1;

  const previewPrevious = useCallback(() => {
    if (galleryIndex <= 0) return;
    const ni = galleryIndex - 1;
    setGalleryIndex(ni);
    void loadAndShow(galleryFiles[ni], galleryIsRemote);
  }, [galleryIndex, galleryFiles, galleryIsRemote, loadAndShow]);

  const previewNext = useCallback(() => {
    if (galleryIndex < 0 || galleryIndex >= galleryFiles.length - 1) return;
    const ni = galleryIndex + 1;
    setGalleryIndex(ni);
    void loadAndShow(galleryFiles[ni], galleryIsRemote);
  }, [galleryIndex, galleryFiles, galleryIsRemote, loadAndShow]);

  // Close Universal Preview (cleanup blob URL)
  const closeUniversalPreview = useCallback(() => {
    if (currentBlobUrlRef.current) {
      URL.revokeObjectURL(currentBlobUrlRef.current);
      currentBlobUrlRef.current = null;
    }
    setUniversalPreviewOpen(false);
    setUniversalPreviewFile(null);
    setGalleryFiles([]);
    setGalleryIndex(-1);
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
    openDevToolsFromData,

    // Universal preview
    universalPreviewOpen,
    universalPreviewFile,
    openUniversalPreview,
    closeUniversalPreview,
    previewNext,
    previewPrevious,
    hasPreviewNext,
    hasPreviewPrevious,

    // View mode
    viewMode,
    setViewMode,
  };
};

export default usePreview;
