// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

// Issue #591: which part of a multi-item transfer runs as one parallel
// backend batch and which part keeps the per-item path.
//
// Every GUI entry point that moves several items at once (panel selection,
// cross-panel drag, clipboard paste, the upload dialog, the transfer planner)
// asks this module first. Before it existed each entry point awaited one
// `downloadFile` / `uploadFile` per item, so the "concurrent transfers"
// setting applied to folders only, on every protocol.

export type FileBatchDirection = 'download' | 'upload';

export interface FileBatchSessionFlags {
  /** AeroVault overlay sessions extract and add entries one by one. */
  aeroVaultOverlay: boolean;
  /** An upload to GitHub or GitLab, repository or releases: an upload there
   *  is a commit or a release asset handled by the single-file command (the
   *  repository case also has an atomic batch-commit path), and both
   *  providers are single-session, so a batch would add no parallelism. */
  gitHostUpload: boolean;
  /** A cut (the paste deletes the sources that landed) on a session with no
   *  protocol: its legacy FTP-manager batch reports no per-file outcome, so
   *  per-file calls must say which sources may go. */
  legacyCut?: boolean;
}

export interface FileBatchSplit<T> {
  /** Plain files sent together as one backend batch. */
  batch: T[];
  /** Items that keep the per-item path: folders (each already parallel
   *  inside), or everything when a batch would not apply. */
  sequential: T[];
}

/** True when a session can hand a list of files to the backend batch. */
export function canRunFileBatch(flags: FileBatchSessionFlags): boolean {
  return !flags.aeroVaultOverlay && !flags.gitHostUpload && !flags.legacyCut;
}

/**
 * Split a multi-item transfer. Two or more plain files form the batch;
 * folders stay on the folder command, which runs its own files in parallel.
 * A single file keeps the single-file path, which also carries resume and,
 * on SFTP, the rsync delta transfer.
 */
export function splitForFileBatch<T extends { is_dir?: boolean | null }>(
  items: readonly T[],
  flags: FileBatchSessionFlags,
): FileBatchSplit<T> {
  if (!canRunFileBatch(flags)) {
    return { batch: [], sequential: [...items] };
  }
  const files = items.filter(item => !item.is_dir);
  if (files.length < 2) {
    return { batch: [], sequential: [...items] };
  }
  return { batch: files, sequential: items.filter(item => !!item.is_dir) };
}

/**
 * Backend command for a file batch. Every protocol the GUI connects with,
 * FTP and FTPS included, is a provider session (`usesProviderApi`); the
 * legacy FTP-manager batch remains only for a session that has no protocol.
 */
export function fileBatchCommand(
  direction: FileBatchDirection,
  isProviderSession: boolean,
): string {
  if (isProviderSession) {
    return direction === 'download' ? 'provider_download_files_batch' : 'provider_upload_files_batch';
  }
  return direction === 'download' ? 'download_files_batch' : 'upload_files_batch';
}

/**
 * Whether one file of a batch keeps the single-file command instead of going
 * in the parallel batch:
 * - an upload the user asked to RESUME: only `provider_upload_file` continues
 *   from the remote partial;
 * - on SFTP, a file whose destination already exists and keeps its name: only
 *   the single-file commands try an rsync delta first (the batch executor has
 *   no delta step), and an existing destination is the one case where a
 *   delta can save the transfer. A new file gains nothing from either.
 *
 * `destinationExists` comes from checkOverwrite, which looks at the listing
 * of the panel on screen. For a paste or a planner run into another folder
 * that listing is not the destination, so on SFTP a file existing only over
 * there goes in the batch (it loses the delta, nothing else) and one existing
 * only here goes single (it loses the parallelism, nothing else). Safe both
 * ways; it is checkOverwrite's long-standing limit, not a new one.
 *
 * - a file whose destination another file of the same transfer already
 *   writes (`destinationClaimed`: two picked files with one name from
 *   different folders): in the batch both would be written at once and the
 *   result would depend on which finished last. After the batch, one at a
 *   time, the later file wins, as it did before the batch existed.
 */
export function keepsSingleFilePath(file: {
  direction: FileBatchDirection;
  isProviderSession: boolean;
  protocol: string | undefined;
  action: string;
  destinationExists: boolean;
  renamed: boolean;
  destinationClaimed: boolean;
}): boolean {
  if (!file.isProviderSession) return false;
  if (file.destinationClaimed) return true;
  if (file.direction === 'upload' && file.action === 'resume') return true;
  return file.protocol === 'sftp' && file.destinationExists && !file.renamed;
}

/** What the folder transfer commands answer (Rust `FolderTransferOutcome`). */
export interface FolderTransferOutcome {
  completed: number;
  skipped: number;
  failed: number;
  cancelled: boolean;
  message: string;
}

/**
 * Whether a folder transfer finished COMPLETELY: not cancelled, no file
 * failed and no file skipped. A "cut" paste deletes the source folder only on
 * this answer. A skipped file means the destination already had a file of
 * that name which this transfer did not write, so deleting the source would
 * lose its content; the file batch keeps a skipped file's source the same way.
 */
export function folderTransferIsComplete(outcome: FolderTransferOutcome | null | undefined): boolean {
  return !!outcome && !outcome.cancelled && outcome.failed === 0 && outcome.skipped === 0;
}
