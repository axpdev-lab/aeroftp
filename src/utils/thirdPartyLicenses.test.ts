// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet (see AI-TRANSPARENCY.md)

import { describe, it, expect, vi, afterEach } from 'vitest';

const shellOpen = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/plugin-shell', () => ({ open: shellOpen }));
import packageJson from '../../package.json';
import {
  openThirdPartyLicenses,
  getCandidateNoticePaths,
  GITHUB_NOTICES_URL,
  LINUX_NOTICE_PATHS,
} from './thirdPartyLicenses';
import { openUrl } from './openUrl';

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

    it('checks executableDir for portable installations when present', async () => {
      const mockResolver = vi.fn().mockRejectedValue(new Error('Resource not found'));
      const mockExeDir = vi.fn().mockResolvedValue('/opt/portable-app');
      const paths = await getCandidateNoticePaths(mockResolver, mockExeDir);

      expect(paths).toContain('/opt/portable-app/THIRD-PARTY-NOTICES.md');
      expect(mockExeDir).toHaveBeenCalled();
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

    describe('the default URL opener', () => {
      afterEach(() => {
        vi.restoreAllMocks();
        vi.unstubAllGlobals();
        shellOpen.mockReset();
      });
      const noLocalFile = () => Promise.resolve(['/invalid/path']);
      const failingOpener = () => Promise.reject(new Error('Not found'));

      it('reports failure when the shell fails and window.open opens nothing', async () => {
        shellOpen.mockRejectedValue(new Error('shell unavailable'));
        const windowOpen = vi.fn().mockReturnValue(null);
        vi.stubGlobal('window', { open: windowOpen });
        vi.spyOn(console, 'error').mockImplementation(() => {});

        const res = await openThirdPartyLicenses(noLocalFile, failingOpener);

        expect(windowOpen).toHaveBeenCalledWith(GITHUB_NOTICES_URL, '_blank');
        expect(res).toEqual({ success: false, target: GITHUB_NOTICES_URL });
      });

      it('reports failure, without rejecting, when the shell fails and window.open throws', async () => {
        shellOpen.mockRejectedValue(new Error('shell unavailable'));
        vi.stubGlobal('window', { open: vi.fn(() => { throw new SyntaxError('Invalid URL'); }) });
        vi.spyOn(console, 'error').mockImplementation(() => {});

        await expect(openUrl(GITHUB_NOTICES_URL)).resolves.toBe(false);
        expect(await openThirdPartyLicenses(noLocalFile, failingOpener)).toEqual({ success: false, target: GITHUB_NOTICES_URL });
      });

      it('reports success when the shell fails and window.open opens a window', async () => {
        shellOpen.mockRejectedValue(new Error('shell unavailable'));
        vi.stubGlobal('window', { open: vi.fn().mockReturnValue({}) });
        vi.spyOn(console, 'error').mockImplementation(() => {});

        const res = await openThirdPartyLicenses(noLocalFile, failingOpener);

        expect(res).toEqual({ success: true, target: GITHUB_NOTICES_URL });
      });

      it('reports success when the shell opens the URL', async () => {
        shellOpen.mockResolvedValue(undefined);
        const windowOpen = vi.fn();
        vi.stubGlobal('window', { open: windowOpen });

        const res = await openThirdPartyLicenses(noLocalFile, failingOpener);

        expect(shellOpen).toHaveBeenCalledWith(GITHUB_NOTICES_URL);
        expect(windowOpen).not.toHaveBeenCalled();
        expect(res).toEqual({ success: true, target: GITHUB_NOTICES_URL });
      });
    });

    it('reports failure when an injected URL opener resolves false or rejects', async () => {
      const noLocalFile = () => Promise.resolve(['/invalid/path']);
      const failingOpener = vi.fn().mockRejectedValue(new Error('Not found'));
      vi.spyOn(console, 'error').mockImplementation(() => {});

      expect(await openThirdPartyLicenses(noLocalFile, failingOpener, vi.fn().mockResolvedValue(false))).toEqual({ success: false, target: GITHUB_NOTICES_URL });
      expect(await openThirdPartyLicenses(noLocalFile, failingOpener, vi.fn().mockRejectedValue(new Error('x')))).toEqual({ success: false, target: GITHUB_NOTICES_URL });
      vi.restoreAllMocks();
    });

    it('points to immutable release tag ref instead of main branch', () => {
      expect(GITHUB_NOTICES_URL).toBe(`https://github.com/axpdev-lab/aeroftp/blob/v${packageJson.version}/THIRD-PARTY-NOTICES.md`);
      expect(GITHUB_NOTICES_URL).not.toContain('/blob/main/');
    });
  });
});
