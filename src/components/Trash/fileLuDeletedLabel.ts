// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** Relative age for FileLu trash rows. Returns null when unknown so TrashTable
 *  can show its '-' placeholder; empty string would defeat `??` / `||` fallbacks. */
export function formatDeletedAgo(seconds: number | null): string | null {
  if (seconds === null) return null;
  if (seconds < 60) return `${seconds}s ago`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

/** Prefer the absolute date; fall back to relative age; never yield ''. */
export function fileLuDeletedAtLabel(
  deleted: string | null,
  deletedAgoSec: number | null,
): string | null {
  return deleted || formatDeletedAgo(deletedAgoSec) || null;
}
