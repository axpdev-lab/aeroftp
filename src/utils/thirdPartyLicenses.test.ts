// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet (see AI-TRANSPARENCY.md)

import { describe, it, expect, vi, afterEach } from 'vitest';

const shellOpen = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/plugin-shell', () => ({ open: shellOpen }));
// Tauri's path API, outside the app: every call fails unless a test sets it.
const tauriPath = vi.hoisted(() => {
  const outside = (): Promise<string> => Promise.reject(new Error('Tauri API not available'));
  return {
    resolveResource: vi.fn<(path: string) => Promise<string>>(outside),
    resourceDir: vi.fn<() => Promise<string>>(outside),
    executableDir: vi.fn<() => Promise<string>>(outside),
  };
});
vi.mock('@tauri-apps/api/path', () => tauriPath);
// The backend, outside the app: portable_info fails unless a test sets it.
const tauriCore = vi.hoisted(() => ({
  invoke: vi.fn<(command: string) => Promise<unknown>>(() => Promise.reject(new Error('Tauri API not available'))),
}));
vi.mock('@tauri-apps/api/core', () => tauriCore);
import packageJson from '../../package.json';
import {
  openThirdPartyLicenses,
  getCandidateNoticePaths,
  getOwnPendingNotices,
  plainWindowsPath,
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

    it('looks beside the executable through the resource directory, which on Windows is the executable folder', async () => {
      // The Windows portable ZIP: AeroFTP.exe and THIRD-PARTY-NOTICES.md side
      // by side; the bundled resource '../' resolves under _up_, which the ZIP
      // does not have.
      const mockResolver = vi.fn().mockResolvedValue('C:\\Tools\\AeroFTP\\_up_\\THIRD-PARTY-NOTICES.md');
      const mockResourceDir = vi.fn().mockResolvedValue('C:\\Tools\\AeroFTP');
      const paths = await getCandidateNoticePaths(mockResolver, mockResourceDir);

      expect(paths.slice(0, 2)).toEqual([
        'C:\\Tools\\AeroFTP\\_up_\\THIRD-PARTY-NOTICES.md',
        'C:\\Tools\\AeroFTP\\THIRD-PARTY-NOTICES.md',
      ]);
      expect(mockResourceDir).toHaveBeenCalled();
      // A trailing separator is not doubled.
      const trailing = await getCandidateNoticePaths(mockResolver, vi.fn().mockResolvedValue('C:\\Tools\\AeroFTP\\'));
      expect(trailing[1]).toBe('C:\\Tools\\AeroFTP\\THIRD-PARTY-NOTICES.md');
    });

    it('gives the exe folder in its ordinary form when Tauri returns it as an extended-length path, as it does on Windows', async () => {
      // The Windows unit-test job measured resource_dir() as
      // \\?\D:\a\...\deps for an exe in D:\a\...\deps.
      const paths = await getCandidateNoticePaths(
        vi.fn().mockResolvedValue('\\\\?\\C:\\Tools\\AeroFTP\\_up_\\THIRD-PARTY-NOTICES.md'),
        vi.fn().mockResolvedValue('\\\\?\\C:\\Tools\\AeroFTP')
      );
      expect(paths.slice(0, 2)).toEqual([
        'C:\\Tools\\AeroFTP\\_up_\\THIRD-PARTY-NOTICES.md',
        'C:\\Tools\\AeroFTP\\THIRD-PARTY-NOTICES.md',
      ]);
      const unc = await getCandidateNoticePaths(vi.fn().mockRejectedValue(new Error('none')), vi.fn().mockResolvedValue('\\\\?\\UNC\\server\\share\\AeroFTP'));
      expect(unc[0]).toBe('\\\\server\\share\\AeroFTP\\THIRD-PARTY-NOTICES.md');
      // Any other path is left as it is.
      expect(plainWindowsPath('/opt/AeroFTP')).toBe('/opt/AeroFTP');
      expect(plainWindowsPath('\\\\server\\share')).toBe('\\\\server\\share');
    });

    it('by default asks Tauri for the resource directory, never for executableDir', async () => {
      tauriPath.resolveResource.mockResolvedValueOnce('C:\\Tools\\AeroFTP\\_up_\\THIRD-PARTY-NOTICES.md');
      tauriPath.resourceDir.mockResolvedValueOnce('C:\\Tools\\AeroFTP');
      // What executableDir answers on Linux: the XDG bin directory.
      tauriPath.executableDir.mockResolvedValueOnce('/home/user/.local/bin');
      const paths = await getCandidateNoticePaths();

      expect(paths[1]).toBe('C:\\Tools\\AeroFTP\\THIRD-PARTY-NOTICES.md');
      expect(paths.some((p) => p.includes('.local/bin'))).toBe(false);
      expect(tauriPath.executableDir).not.toHaveBeenCalled();
    });

    it('opens the pending notices of a portable update first, while an editor keeps the installed ones', async () => {
      const pending = 'C:\\Tools\\AeroFTP\\.aeroftp-pending-docs\\THIRD-PARTY-NOTICES.md';
      const installed = 'C:\\Tools\\AeroFTP\\THIRD-PARTY-NOTICES.md';
      const candidates = (own: string | null) => () =>
        getCandidateNoticePaths(
          vi.fn().mockRejectedValue(new Error('no bundled resource')),
          vi.fn().mockResolvedValue('C:\\Tools\\AeroFTP'),
          vi.fn().mockResolvedValue(own)
        );
      const opener = vi.fn((path: string) => (path === pending || path === installed ? Promise.resolve() : Promise.reject(new Error('not found'))));
      expect(await openThirdPartyLicenses(candidates(pending), opener, vi.fn())).toEqual({ success: true, target: pending });
      // Once installed, the backend reports nothing pending and the installed one opens.
      const afterInstall = vi.fn((path: string) => (path === installed ? Promise.resolve() : Promise.reject(new Error('not found'))));
      expect(await openThirdPartyLicenses(candidates(null), afterInstall, vi.fn())).toEqual({ success: true, target: installed });
    });

    it('never opens pending notices the backend does not give as the running exe\'s, though the file is there', async () => {
      // An editor holds an older exe's pending notices, or an update not yet
      // swapped staged the next exe's: the backend checks the exe key and
      // returns nothing, so the installed notices open.
      const stale = 'C:\\Tools\\AeroFTP\\.aeroftp-pending-docs\\THIRD-PARTY-NOTICES.md';
      const staged = 'C:\\Tools\\AeroFTP\\.aeroftp-pending-docs.next\\THIRD-PARTY-NOTICES.md';
      const installed = 'C:\\Tools\\AeroFTP\\THIRD-PARTY-NOTICES.md';
      const paths = await getCandidateNoticePaths(
        vi.fn().mockRejectedValue(new Error('no bundled resource')),
        vi.fn().mockResolvedValue('C:\\Tools\\AeroFTP'),
        vi.fn().mockResolvedValue(null)
      );
      expect(paths.some((p) => p.includes('.aeroftp-pending-docs'))).toBe(false);
      const opener = vi.fn(() => Promise.resolve());
      expect(await openThirdPartyLicenses(() => Promise.resolve(paths), opener, vi.fn())).toEqual({ success: true, target: installed });
      expect(opener).not.toHaveBeenCalledWith(stale);
      expect(opener).not.toHaveBeenCalledWith(staged);
    });

    it('asks the backend for the running exe\'s pending notices and treats a failure as none', async () => {
      tauriCore.invoke.mockResolvedValueOnce({ is_portable: true, pending_notices: 'C:\\Tools\\AeroFTP\\.aeroftp-pending-docs.next\\THIRD-PARTY-NOTICES.md' });
      expect(await getOwnPendingNotices()).toBe('C:\\Tools\\AeroFTP\\.aeroftp-pending-docs.next\\THIRD-PARTY-NOTICES.md');
      expect(tauriCore.invoke).toHaveBeenLastCalledWith('portable_info');
      tauriCore.invoke.mockResolvedValueOnce({ is_portable: true, pending_notices: null });
      expect(await getOwnPendingNotices()).toBeNull();
      tauriCore.invoke.mockResolvedValueOnce({ is_portable: false });
      expect(await getOwnPendingNotices()).toBeNull();
      tauriCore.invoke.mockRejectedValueOnce(new Error('outside the app'));
      expect(await getOwnPendingNotices()).toBeNull();
      // A getter that rejects leaves the other candidates.
      const paths = await getCandidateNoticePaths(
        vi.fn().mockRejectedValue(new Error('no bundled resource')),
        vi.fn().mockResolvedValue('C:\\Tools\\AeroFTP'),
        vi.fn().mockRejectedValue(new Error('broken'))
      );
      expect(paths[0]).toBe('C:\\Tools\\AeroFTP\\THIRD-PARTY-NOTICES.md');
    });

    it('opens the notices of an extracted Windows portable ZIP with the network unavailable', async () => {
      const shipped = 'C:\\Tools\\AeroFTP\\THIRD-PARTY-NOTICES.md';
      const candidates = () =>
        getCandidateNoticePaths(
          vi.fn().mockResolvedValue('C:\\Tools\\AeroFTP\\_up_\\THIRD-PARTY-NOTICES.md'),
          vi.fn().mockResolvedValue('C:\\Tools\\AeroFTP')
        );
      // Only the file the ZIP extracted exists; the network is down.
      const opener = vi.fn((path: string) => (path === shipped ? Promise.resolve() : Promise.reject(new Error('not found'))));
      const offline = vi.fn().mockRejectedValue(new Error('network unavailable'));

      expect(await openThirdPartyLicenses(candidates, opener, offline)).toEqual({ success: true, target: shipped });
      expect(offline).not.toHaveBeenCalled();
    });

    it('never looks in a directory that is not the executable folder (no XDG bin directory on Linux)', async () => {
      const paths = await getCandidateNoticePaths(
        vi.fn().mockResolvedValue('/usr/lib/AeroFTP/_up_/THIRD-PARTY-NOTICES.md'),
        vi.fn().mockResolvedValue('/usr/lib/AeroFTP')
      );
      expect(paths).toEqual([
        '/usr/lib/AeroFTP/_up_/THIRD-PARTY-NOTICES.md',
        '/usr/lib/AeroFTP/THIRD-PARTY-NOTICES.md',
        ...LINUX_NOTICE_PATHS,
      ]);
      expect(paths.some((p) => p.includes('/.local/bin/'))).toBe(false);
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

      it('reports failure for a URL that does not parse, without calling either opener or logging the input', async () => {
        const windowOpen = vi.fn().mockReturnValue({});
        vi.stubGlobal('window', { open: windowOpen });
        const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});

        for (const url of ['', 'not a url', 'https://user:dummy-token-1234@[bad']) {
          await expect(openUrl(url)).resolves.toBe(false);
        }
        expect(shellOpen).not.toHaveBeenCalled();
        expect(windowOpen).not.toHaveBeenCalled();
        expect(JSON.stringify(warn.mock.calls)).not.toContain('dummy-token-1234');
      });

      it('logs a fixed diagnostic when the shell or window.open rejects with the URL, never the URL', async () => {
        const secret = 'https://gitlab.example.com/private-group/private-project/-/blob/main/secret-path.txt';
        shellOpen.mockRejectedValue(new Error(`Command xdg-open ${secret} failed`));
        vi.stubGlobal('window', { open: vi.fn(() => { throw new SyntaxError(`Invalid URL ${secret}`); }) });
        const error = vi.spyOn(console, 'error').mockImplementation(() => {});

        await expect(openUrl(secret)).resolves.toBe(false);
        expect(error).toHaveBeenCalledTimes(2);
        const logged = error.mock.calls.flat().map((arg) => (arg instanceof Error ? `${arg.message} ${arg.stack ?? ''}` : String(arg))).join(' ');
        expect(logged).not.toContain('private-project');
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

    it('points to the raw file at the immutable release tag, not to main or to the blob page', () => {
      expect(GITHUB_NOTICES_URL).toBe(`https://raw.githubusercontent.com/axpdev-lab/aeroftp/v${packageJson.version}/THIRD-PARTY-NOTICES.md`);
      expect(GITHUB_NOTICES_URL).not.toMatch(/\/main\/|\/blob\//);
      // The ref is the release tag of this build's version, the one the tag
      // workflow checks before it publishes (check-version-sites --tag and
      // check:licenses in linux-validate).
      const url = new URL(GITHUB_NOTICES_URL);
      const [owner, repo, ref, ...file] = url.pathname.slice(1).split('/');
      expect({ host: url.host, owner, repo, ref, file: file.join('/') }).toEqual({
        host: 'raw.githubusercontent.com',
        owner: 'axpdev-lab',
        repo: 'aeroftp',
        ref: `v${packageJson.version}`,
        file: 'THIRD-PARTY-NOTICES.md',
      });
    });

    it('logs a fixed diagnostic when an injected URL opener rejects, never the rejection', async () => {
      const error = vi.spyOn(console, 'error').mockImplementation(() => {});
      const noLocalFile = () => Promise.resolve(['/invalid/path']);
      await openThirdPartyLicenses(noLocalFile, vi.fn().mockRejectedValue(new Error('Not found')), vi.fn().mockRejectedValue(new Error(`spawn failed: xdg-open ${GITHUB_NOTICES_URL}`)));
      expect(JSON.stringify(error.mock.calls)).not.toContain('raw.githubusercontent.com');
      expect(error).toHaveBeenCalledWith('Failed to open third-party licenses');
      vi.restoreAllMocks();
    });
  });
});
