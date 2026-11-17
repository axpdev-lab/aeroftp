// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Open URL in default browser using Tauri shell plugin
 * This is required in Tauri 2.0 since target="_blank" doesn't work
 *
 * Security: only http, https and mailto schemes are allowed.
 * Other schemes (file://, javascript:, custom://) are blocked to prevent abuse.
 *
 * Resolves true when the URL was handed to the system or to a new window, and
 * false when the scheme is blocked or both the shell and the window.open
 * fallback failed (window.open returns null or throws). It never rejects, so a
 * caller that ignores the result keeps working as before.
 */
import { open } from '@tauri-apps/plugin-shell';

const ALLOWED_SCHEMES = ['http:', 'https:', 'mailto:'];

export async function openUrl(url: string): Promise<boolean> {
  try {
    const parsed = new URL(url);
    if (!ALLOWED_SCHEMES.includes(parsed.protocol)) {
      throw new Error(`Blocked URL scheme: ${parsed.protocol}`);
    }
    await open(url);
    return true;
  } catch (error) {
    if (error instanceof Error && error.message.startsWith('Blocked URL scheme')) {
      console.warn(error.message);
      return false;
    }
    console.error('Failed to open URL:', error);
    // Fallback: try window.open (won't work in Tauri but useful for dev).
    // It returns null when it opens nothing and can throw (a malformed URL).
    try {
      return window.open(url, '_blank') !== null;
    } catch (fallbackError) {
      console.error('Failed to open URL in a new window:', fallbackError);
      return false;
    }
  }
}

export default openUrl;
