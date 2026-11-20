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
 * false when it does not parse, the scheme is blocked, or both the shell and
 * the window.open fallback failed (window.open returns null or throws). The
 * fallback runs only for a URL that parsed with an allowed scheme. It never
 * rejects, so a caller that ignores the result keeps working as before.
 */
import { open } from '@tauri-apps/plugin-shell';

const ALLOWED_SCHEMES = ['http:', 'https:', 'mailto:'];

export async function openUrl(url: string): Promise<boolean> {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    // The input is not logged: it may carry a token.
    console.warn('Not a valid URL');
    return false;
  }
  if (!ALLOWED_SCHEMES.includes(parsed.protocol)) {
    console.warn(`Blocked URL scheme: ${parsed.protocol}`);
    return false;
  }
  try {
    await open(url);
    return true;
  } catch {
    // The rejection is not logged: it can carry the launcher's command line,
    // and with it the URL.
    console.error('Failed to open URL');
    // Fallback: try window.open (won't work in Tauri but useful for dev).
    // It returns null when it opens nothing and can throw.
    try {
      return window.open(url, '_blank') !== null;
    } catch {
      console.error('Failed to open URL in a new window');
      return false;
    }
  }
}

export default openUrl;
