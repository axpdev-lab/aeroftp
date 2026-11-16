// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet (see AI-TRANSPARENCY.md)

import { describe, it, expect, vi } from 'vitest';
import {
  openThirdPartyLicenses,
  getCandidateNoticePaths,
  GITHUB_NOTICES_URL,
  LINUX_NOTICE_PATHS,
} from './thirdPartyLicenses';

describe('thirdPartyLicenses', () => {
  describe('getCandidateNoticePaths', () => {
    it('returns only absolute paths and never relative paths', async () => {
      const mockResolver = vi.fn().mockResolvedValue('/app/resources/_up_/THIRD-PARTY-NOTICES.md');
      const paths = await getCandidateNoticePaths(mockResolver);

      expect(paths.length).toBeGreaterThan(0);
      for (const p of paths) {
        expect(p.startsWith('/') || /^[a-zA-Z]:\\/.test(p)).toBe(true);
        expect(p.startsWith('.')).toBe(false);
        expect(p.startsWith('..')).toBe(false);
      }
    });

    it('tries resolveResource first before Linux system paths', async () => {
      const mockResolver = vi.fn().mockResolvedValue('/opt/aeroftp/resources/_up_/THIRD-PARTY-NOTICES.md');
      const paths = await getCandidateNoticePaths(mockResolver);

      expect(mockResolver).toHaveBeenCalledWith('../THIRD-PARTY-NOTICES.md');
      expect(paths[0]).toBe('/opt/aeroftp/resources/_up_/THIRD-PARTY-NOTICES.md');
      expect(paths.slice(1)).toEqual(LINUX_NOTICE_PATHS);
    });

    it('gracefully handles resource resolution failure and returns Linux system paths', async () => {
      const mockResolver = vi.fn().mockRejectedValue(new Error('Tauri API not available'));
      const paths = await getCandidateNoticePaths(mockResolver);

      expect(paths).toEqual(LINUX_NOTICE_PATHS);
    });
  });

  describe('openThirdPartyLicenses', () => {
    it('opens resolved resource first when available', async () => {
      const mockCandidates = () => Promise.resolve([
        '/bundle/resources/_up_/THIRD-PARTY-NOTICES.md',
        '/usr/share/doc/aeroftp/THIRD-PARTY-NOTICES.md',
      ]);
      const mockOpener = vi.fn().mockResolvedValue(undefined);
      const mockUrlOpener = vi.fn().mockResolvedValue(undefined);

      const res = await openThirdPartyLicenses(mockCandidates, mockOpener, mockUrlOpener);

      expect(res.success).toBe(true);
      expect(res.target).toBe('/bundle/resources/_up_/THIRD-PARTY-NOTICES.md');
      expect(mockOpener).toHaveBeenCalledWith('/bundle/resources/_up_/THIRD-PARTY-NOTICES.md');
      expect(mockUrlOpener).not.toHaveBeenCalled();
    });

    it('falls back through candidates until one succeeds', async () => {
      const mockCandidates = () => Promise.resolve([
        '/bundle/resources/_up_/THIRD-PARTY-NOTICES.md',
        '/usr/share/doc/aeroftp/THIRD-PARTY-NOTICES.md',
      ]);
      const mockOpener = vi.fn().mockImplementation((path: string) => {
        if (path === '/usr/share/doc/aeroftp/THIRD-PARTY-NOTICES.md') {
          return Promise.resolve();
        }
        return Promise.reject(new Error('File not found'));
      });
      const mockUrlOpener = vi.fn().mockResolvedValue(undefined);

      const res = await openThirdPartyLicenses(mockCandidates, mockOpener, mockUrlOpener);

      expect(res.success).toBe(true);
      expect(res.target).toBe('/usr/share/doc/aeroftp/THIRD-PARTY-NOTICES.md');
      expect(mockOpener).toHaveBeenCalledWith('/bundle/resources/_up_/THIRD-PARTY-NOTICES.md');
      expect(mockOpener).toHaveBeenCalledWith('/usr/share/doc/aeroftp/THIRD-PARTY-NOTICES.md');
      expect(mockUrlOpener).not.toHaveBeenCalled();
    });

    it('falls back to GitHub URL when all candidates fail', async () => {
      const mockCandidates = () => Promise.resolve(['/invalid/path/1', '/invalid/path/2']);
      const mockOpener = vi.fn().mockRejectedValue(new Error('Not found'));
      const mockUrlOpener = vi.fn().mockResolvedValue(undefined);

      const res = await openThirdPartyLicenses(mockCandidates, mockOpener, mockUrlOpener);

      expect(res.success).toBe(true);
      expect(res.target).toBe(GITHUB_NOTICES_URL);
      expect(mockUrlOpener).toHaveBeenCalledWith(GITHUB_NOTICES_URL);
    });
  });
});
