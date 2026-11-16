// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet (see AI-TRANSPARENCY.md)

import { invoke } from '@tauri-apps/api/core';
import { resolveResource } from '@tauri-apps/api/path';
import { openUrl } from './openUrl';

export const GITHUB_NOTICES_URL = 'https://github.com/axpdev-lab/aeroftp/blob/main/THIRD-PARTY-NOTICES.md';

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
 * Subsequent candidates are standard Linux package install locations.
 */
export async function getCandidateNoticePaths(
  resourceResolver: (path: string) => Promise<string> = resolveResource
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
  candidates.push(...LINUX_NOTICE_PATHS);
  return candidates;
}

/**
 * Open the third-party license notices file in the system default reader.
 * Tries local bundled files first (resolved resource then absolute Linux paths),
 * falls back to GitHub repository URL.
 */
export async function openThirdPartyLicenses(
  candidatesFetcher: () => Promise<string[]> = getCandidateNoticePaths,
  opener: (path: string) => Promise<void> = (path: string) => invoke<void>('open_local_file', { path }),
  urlOpener: (url: string) => Promise<void> = openUrl
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
    await urlOpener(GITHUB_NOTICES_URL);
    return { success: true, target: GITHUB_NOTICES_URL };
  } catch (err) {
    console.error('Failed to open third-party licenses:', err);
    return { success: false, target: GITHUB_NOTICES_URL };
  }
}
