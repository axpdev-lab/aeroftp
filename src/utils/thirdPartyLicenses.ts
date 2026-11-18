// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet (see AI-TRANSPARENCY.md)

import { invoke } from '@tauri-apps/api/core';
import { resolveResource, resourceDir } from '@tauri-apps/api/path';
import packageJson from '../../package.json';
import { openUrl } from './openUrl';

// The raw file at the release tag: GitHub's page for the file shows no text at
// this size, only a "View raw" link. The tag is the ref of the published
// build: on a tag run, build.yml's linux-validate requires every version site
// to equal the tag (check-version-sites --tag) and the tag's notices to match
// its lockfiles (check:licenses), and every publish job requires that job.
export const GITHUB_NOTICES_URL = `https://raw.githubusercontent.com/axpdev-lab/aeroftp/v${packageJson.version}/THIRD-PARTY-NOTICES.md`;

/**
 * Absolute Linux candidate paths where THIRD-PARTY-NOTICES.md may be installed.
 * Flatpak candidate (/app/share/doc/...) is only present if FlatPark installs it.
 */
export const LINUX_NOTICE_PATHS = [
  '/usr/share/doc/aeroftp/THIRD-PARTY-NOTICES.md',
  '/snap/aeroftp/current/usr/share/doc/aeroftp/THIRD-PARTY-NOTICES.md',
  '/app/share/doc/aeroftp/THIRD-PARTY-NOTICES.md',
];

/**
 * Returns candidate paths for THIRD-PARTY-NOTICES.md.
 * No relative paths are included to avoid CWD-dependent resolution.
 * First candidate is the Tauri-resolved bundled resource ('../THIRD-PARTY-NOTICES.md').
 * The Windows portable ZIP ships the file beside AeroFTP.exe: Tauri's resource
 * directory on Windows is the running executable's own folder (tauri-utils
 * resource_dir returns current_exe().parent() there), so the file is looked
 * up at its root. Tauri's executableDir is not that folder: it is unsupported
 * on Windows and the XDG bin directory on Linux.
 * Subsequent candidates are standard Linux package install locations.
 */
export async function getCandidateNoticePaths(
  resourceResolver: (path: string) => Promise<string> = resolveResource,
  resourceDirGetter: () => Promise<string> = resourceDir
): Promise<string[]> {
  const candidates: string[] = [];
  try {
    const resPath = await resourceResolver('../THIRD-PARTY-NOTICES.md');
    if (resPath && typeof resPath === 'string') {
      candidates.push(resPath);
    }
  } catch {
    // Resource resolver error or running outside packaged Tauri app
  }
  try {
    const dir = await resourceDirGetter();
    if (dir && typeof dir === 'string') {
      const sep = dir.includes('\\') ? '\\' : '/';
      const besideExe = dir.endsWith(sep) ? `${dir}THIRD-PARTY-NOTICES.md` : `${dir}${sep}THIRD-PARTY-NOTICES.md`;
      if (!candidates.includes(besideExe)) {
        candidates.push(besideExe);
      }
    }
  } catch {
    // Resource dir error or web environment
  }
  candidates.push(...LINUX_NOTICE_PATHS);
  return candidates;
}

/**
 * Open the third-party license notices file in the system default reader.
 * Tries local bundled files first (resolved resource then absolute Linux paths),
 * falls back to GitHub repository URL. Reports success: false when the URL
 * opener rejects or resolves false (nothing was opened); an opener that
 * resolves without a value counts as opened.
 */
export async function openThirdPartyLicenses(
  candidatesFetcher: () => Promise<string[]> = getCandidateNoticePaths,
  opener: (path: string) => Promise<void> = (path: string) => invoke<void>('open_local_file', { path }),
  urlOpener: (url: string) => Promise<boolean | void> = openUrl
): Promise<{ success: boolean; target: string }> {
  const candidates = await candidatesFetcher();

  for (const candidate of candidates) {
    try {
      await opener(candidate);
      return { success: true, target: candidate };
    } catch {
      // Continue to next candidate
    }
  }

  // Fallback to online repository version
  try {
    const opened = await urlOpener(GITHUB_NOTICES_URL);
    return { success: opened !== false, target: GITHUB_NOTICES_URL };
  } catch {
    // The rejection is not logged: like openUrl's, it can carry the URL.
    console.error('Failed to open third-party licenses');
    return { success: false, target: GITHUB_NOTICES_URL };
  }
}
