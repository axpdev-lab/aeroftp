// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, it, expect } from 'vitest';
import { overlayAnchor } from './cryptUnlockTarget';

describe('overlayAnchor', () => {
  it('spells the whole remote as "/", never as the empty "current folder"', () => {
    expect(overlayAnchor(undefined)).toBe('/');
    expect(overlayAnchor(null)).toBe('/');
    expect(overlayAnchor('')).toBe('/');
    expect(overlayAnchor('  ')).toBe('/');
    expect(overlayAnchor('/')).toBe('/');
  });

  it('keeps a subfolder absolute and without trailing slashes', () => {
    expect(overlayAnchor('/home/user/Vault')).toBe('/home/user/Vault');
    expect(overlayAnchor('home/user/Vault/')).toBe('/home/user/Vault');
  });
});
