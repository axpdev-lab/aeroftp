// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Local-path parent navigation helpers (AeroFile / local panels).
 *
 * On Windows, Rust `Path::is_absolute()` treats bare `D:` as drive-relative
 * (false) but `D:/` and `D:\` as absolute (true). Parent computation that
 * joins path segments can produce `D:` from `D:/boot`, which then fails
 * `get_local_files` with "Path must be absolute". Always normalize bare
 * drive letters to `X:/` before navigating.
 */

/** True for Windows drive roots: `C:`, `C:/`, `C:\`, `D:\\`, etc. */
export function isWindowsDriveRoot(path: string): boolean {
  return /^[A-Za-z]:[\\/]?$/.test(path);
}

/**
 * Parent of a local filesystem path for panel "go up" navigation.
 * Linux/macOS roots stay `/`. Windows drive roots stay absolute (`D:/`).
 */
export function parentLocalPath(currentPath: string): string {
  if (!currentPath) return '/';
  const normalized = currentPath.replace(/\\/g, '/').replace(/\/+$/, '');
  if (!normalized || normalized === '/') return '/';
  // Already at Windows drive root after strip of trailing slashes
  if (/^[A-Za-z]:$/.test(normalized)) return `${normalized}/`;
  const idx = normalized.lastIndexOf('/');
  if (idx <= 0) return '/';
  let parent = normalized.slice(0, idx);
  if (/^[A-Za-z]:$/.test(parent)) parent = `${parent}/`;
  return parent || '/';
}
