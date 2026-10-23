// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { translate } from '../../i18n';
import type { TranslationFunction } from '../../i18n/types';

/** Relative age for FileLu trash rows. Returns null when unknown so TrashTable
 *  can show its '-' placeholder; empty string would defeat `??` / `||` fallbacks. */
export function formatDeletedAgo(seconds: number | null, t: TranslationFunction = translate): string | null {
  if (seconds === null) return null;
  if (seconds < 60) return t('cloud.secondsAgo', { count: seconds });
  if (seconds < 3600) return t('cloud.minutesAgo', { count: Math.floor(seconds / 60) });
  if (seconds < 86400) return t('cloud.hoursAgo', { count: Math.floor(seconds / 3600) });
  return t('vault.recentDaysAgo', { count: Math.floor(seconds / 86400) });
}

/** Prefer the absolute date; fall back to relative age; never yield ''. */
export function fileLuDeletedAtLabel(
  deleted: string | null,
  deletedAgoSec: number | null,
  t: TranslationFunction = translate,
): string | null {
  return deleted || formatDeletedAgo(deletedAgoSec, t) || null;
}
