// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Image-preview transparency background (discussion #270).
 *
 * Transparent images (PNG/SVG/WebP/GIF) used to render on a fixed dark
 * panel, so transparency read as "dark" while other viewers show it as
 * white. This module lets the user pick the colour shown behind a
 * transparent image: a checkerboard (recommended, makes transparency
 * obvious), a flat dark or light, or any custom RGB colour.
 *
 * The value lives in localStorage so both the Settings > Appearance picker
 * and the ImageViewer read the same source. A custom `image-preview-bg-changed`
 * event (plus the cross-tab `storage` event) drives a live update without a
 * reload. Stored value is one of the preset ids or a `#rrggbb` hex string.
 */
import { useEffect, useState } from 'react';
import type { CSSProperties } from 'react';

export const IMAGE_PREVIEW_BG_KEY = 'aeroftp_image_preview_bg';
export const IMAGE_PREVIEW_BG_EVENT = 'image-preview-bg-changed';

/** Default keeps the historical dark panel so existing installs look unchanged. */
export const DEFAULT_IMAGE_PREVIEW_BG = 'dark';

/** Tailwind `gray-900`, the original hardcoded ImageViewer background. */
const DARK_HEX = '#111827';
const LIGHT_HEX = '#ffffff';

export const IMAGE_PREVIEW_BG_PRESETS = [
  { id: 'checkerboard', nameKey: 'settings.imagePreviewBgCheckerboard' },
  { id: 'dark', nameKey: 'settings.imagePreviewBgDark' },
  { id: 'light', nameKey: 'settings.imagePreviewBgLight' },
] as const;

export function readImagePreviewBg(): string {
  try {
    return localStorage.getItem(IMAGE_PREVIEW_BG_KEY) || DEFAULT_IMAGE_PREVIEW_BG;
  } catch {
    return DEFAULT_IMAGE_PREVIEW_BG;
  }
}

export function writeImagePreviewBg(value: string): void {
  try {
    localStorage.setItem(IMAGE_PREVIEW_BG_KEY, value);
    window.dispatchEvent(new CustomEvent(IMAGE_PREVIEW_BG_EVENT, { detail: value }));
  } catch {
    // localStorage unavailable: ignore, the in-memory default still applies
  }
}

/** Hex string currently representing the picker value (presets resolved to hex). */
export function resolveImagePreviewBgHex(value: string): string {
  if (value === 'dark') return DARK_HEX;
  if (value === 'light') return LIGHT_HEX;
  if (value === 'checkerboard') return LIGHT_HEX;
  return /^#[0-9a-fA-F]{6}$/.test(value) ? value : DARK_HEX;
}

/** Inline style applied behind a transparent image. */
export function resolveImagePreviewBgStyle(value: string): CSSProperties {
  if (value === 'checkerboard') {
    return {
      backgroundColor: '#ffffff',
      backgroundImage:
        'linear-gradient(45deg, #c8c8c8 25%, transparent 25%), linear-gradient(-45deg, #c8c8c8 25%, transparent 25%), linear-gradient(45deg, transparent 75%, #c8c8c8 75%), linear-gradient(-45deg, transparent 75%, #c8c8c8 75%)',
      backgroundSize: '20px 20px',
      backgroundPosition: '0 0, 0 10px, 10px -10px, -10px 0',
    };
  }
  return { backgroundColor: resolveImagePreviewBgHex(value) };
}

/** Reactive hook: returns the inline style and the raw value, updating live. */
export function useImagePreviewBg(): { value: string; style: CSSProperties } {
  const [value, setValue] = useState<string>(readImagePreviewBg);
  useEffect(() => {
    const onChange = () => setValue(readImagePreviewBg());
    window.addEventListener(IMAGE_PREVIEW_BG_EVENT, onChange);
    window.addEventListener('storage', onChange);
    return () => {
      window.removeEventListener(IMAGE_PREVIEW_BG_EVENT, onChange);
      window.removeEventListener('storage', onChange);
    };
  }, []);
  return { value, style: resolveImagePreviewBgStyle(value) };
}
