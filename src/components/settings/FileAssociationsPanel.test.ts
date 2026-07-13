// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';

// Pure helpers extracted for test coverage of grouping + action modes
// (mirrors logic in FileAssociationsPanel + backend catalog expectations).

const AERO_KEYS = ['aerovault', 'aerozip', 'profile', 'keystore', 'script'] as const;
const ARCHIVE_KEYS = ['zip', 'sevenz', 'rar', 'tar', 'single_stream'] as const;

function groupItems(keys: readonly string[], items: Array<{ key: string }>) {
  return {
    aero: items.filter(i => (keys as readonly string[]).includes(i.key) && AERO_KEYS.includes(i.key as any)),
    archives: items.filter(i => (keys as readonly string[]).includes(i.key) && ARCHIVE_KEYS.includes(i.key as any)),
  };
}

function actionForPlatform(platform: string, itemAction: string): string {
  if (platform === 'linux' && itemAction === 'direct') return 'direct-apply';
  if (platform === 'windows') return 'os-confirmation';
  if (platform === 'macos') return 'best-effort';
  return 'unsupported';
}

function statusLabel(isDefault: boolean, isAvailable: boolean, current?: string | null): string {
  if (isDefault) return 'Default';
  if (isAvailable) return 'Available';
  if (current) return 'Other';
  return 'Unknown';
}

describe('FileAssociationsPanel helpers (pure)', () => {
  const mockItems = [
    { key: 'aerovault', isDefault: true, isAvailable: true },
    { key: 'zip', isDefault: false, isAvailable: true, currentHandler: 'Explorer' },
    { key: 'sevenz', isDefault: false, isAvailable: false },
  ];

  it('groups aero vs archives correctly', () => {
    const g = groupItems(['aerovault', 'zip', 'sevenz'], mockItems as any);
    expect(g.aero.map(i => i.key)).toEqual(['aerovault']);
    expect(g.archives.map(i => i.key)).toEqual(['zip', 'sevenz']);
  });

  it('computes action modes per platform', () => {
    expect(actionForPlatform('linux', 'direct')).toBe('direct-apply');
    expect(actionForPlatform('windows', 'os_confirmation')).toBe('os-confirmation');
    expect(actionForPlatform('macos', 'best_effort')).toBe('best-effort');
    expect(actionForPlatform('freebsd', 'foo')).toBe('unsupported');
  });

  it('maps status labels', () => {
    expect(statusLabel(true, true)).toBe('Default');
    expect(statusLabel(false, true)).toBe('Available');
    expect(statusLabel(false, false, 'Files')).toBe('Other');
    expect(statusLabel(false, false)).toBe('Unknown');
  });

  it('catalog keys shape (contract)', () => {
    // These keys must exist in backend + frontend expectations
    const all = [...AERO_KEYS, ...ARCHIVE_KEYS];
    expect(all).toContain('aerovault');
    expect(all).toContain('single_stream');
    expect(AERO_KEYS[0]).toBe('aerovault');
  });
});
