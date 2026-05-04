// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

export type OpenWithDefaultRoute =
  | { kind: 'aerovault' }
  | { kind: 'aeroftp-profile' }
  | { kind: 'aeroftp-keystore' }
  | { kind: 'terminal'; command: string }
  | { kind: 'system' };

const SCRIPT_EXTENSIONS = new Set(['.ps1', '.sh', '.py']);

function getDefaultPlatform(): string {
  return typeof navigator === 'undefined' ? '' : navigator.platform;
}

export function isSafeLocalOpenPath(path: string): boolean {
  const value = path.trim();
  if (!value) return false;
  if (value.includes('\0') || value.includes('\r') || value.includes('\n')) return false;
  if (/^file:/i.test(value)) return false;
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(value)) return false;
  return true;
}

function lowerPath(path: string): string {
  return path.trim().toLowerCase();
}

function extensionOf(path: string): string {
  const lower = lowerPath(path);
  const slash = Math.max(lower.lastIndexOf('/'), lower.lastIndexOf('\\'));
  const name = lower.slice(slash + 1);
  const dot = name.lastIndexOf('.');
  return dot >= 0 ? name.slice(dot) : '';
}

function quoteForPosixShell(path: string): string {
  return `'${path.replace(/'/g, `'\\''`)}'`;
}

function quoteForPowerShell(path: string): string {
  return `'${path.replace(/'/g, `''`)}'`;
}

export function shellQuoteLocalPath(path: string, platform = getDefaultPlatform()): string {
  return platform.toLowerCase().startsWith('win')
    ? quoteForPowerShell(path)
    : quoteForPosixShell(path);
}

export function buildTerminalCommandForPath(path: string, platform = getDefaultPlatform()): string | null {
  if (!isSafeLocalOpenPath(path)) return null;

  const ext = extensionOf(path);
  const quoted = shellQuoteLocalPath(path, platform);

  if (ext === '.ps1') return `pwsh -File ${quoted}`;
  if (ext === '.sh') return `bash ${quoted}`;
  if (ext === '.py') return `python3 ${quoted}`;
  return null;
}

export function getOpenWithDefaultRoute(path: string, isDir = false, platform = getDefaultPlatform()): OpenWithDefaultRoute {
  if (!isSafeLocalOpenPath(path)) {
    throw new Error('Unsafe local path');
  }

  if (!isDir) {
    const lower = lowerPath(path);
    if (lower.endsWith('.aerovault')) return { kind: 'aerovault' };
    if (lower.endsWith('.aeroftp-keystore')) return { kind: 'aeroftp-keystore' };
    if (lower.endsWith('.aeroftp')) return { kind: 'aeroftp-profile' };

    const ext = extensionOf(path);
    if (SCRIPT_EXTENSIONS.has(ext)) {
      const command = buildTerminalCommandForPath(path, platform);
      if (command) return { kind: 'terminal', command };
    }
  }

  return { kind: 'system' };
}
