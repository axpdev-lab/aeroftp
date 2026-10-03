// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { usesProviderApi } from '../types';
import type { ProviderType } from '../types';
import {
  canRunFileBatch,
  fileBatchCommand,
  folderTransferIsComplete,
  keepsSingleFilePath,
  splitForFileBatch,
} from './fileBatchRouting';

const plain = { aeroVaultOverlay: false, gitHostUpload: false };
const files = (n: number) =>
  Array.from({ length: n }, (_, i) => ({ name: `f${i}.bin`, is_dir: false }));

describe('splitForFileBatch (issue #591)', () => {
  it('sends a selection of several files as one batch', () => {
    const items = files(8);
    const split = splitForFileBatch(items, plain);
    expect(split.batch).toEqual(items);
    expect(split.sequential).toEqual([]);
  });

  it('keeps folders on the folder command and batches the files beside them', () => {
    const items = [
      { name: 'a.txt', is_dir: false },
      { name: 'photos', is_dir: true },
      { name: 'b.txt', is_dir: false },
      { name: 'docs', is_dir: true },
    ];
    const split = splitForFileBatch(items, plain);
    expect(split.batch.map(i => i.name)).toEqual(['a.txt', 'b.txt']);
    expect(split.sequential.map(i => i.name)).toEqual(['photos', 'docs']);
  });

  it('leaves a single file on the single-file path (resume, SFTP delta)', () => {
    const items = [{ name: 'a.txt', is_dir: false }, { name: 'photos', is_dir: true }];
    const split = splitForFileBatch(items, plain);
    expect(split.batch).toEqual([]);
    expect(split.sequential).toEqual(items);
  });

  it('treats a missing is_dir as a file', () => {
    const split = splitForFileBatch([{ name: 'a' }, { name: 'b', is_dir: null }], plain);
    expect(split.batch).toHaveLength(2);
  });

  it('never batches inside an AeroVault overlay', () => {
    const split = splitForFileBatch(files(4), { aeroVaultOverlay: true, gitHostUpload: false });
    expect(split.batch).toEqual([]);
    expect(split.sequential).toHaveLength(4);
  });

  it('never batches an upload to GitHub or GitLab (commits and release assets)', () => {
    expect(canRunFileBatch({ aeroVaultOverlay: false, gitHostUpload: true })).toBe(false);
    expect(splitForFileBatch(files(4), { aeroVaultOverlay: false, gitHostUpload: true }).batch).toEqual([]);
  });

  it('never batches a cut on a session whose batch reports no per-file outcome', () => {
    // The legacy FTP-manager batch answers one aggregate string, so nothing
    // counts as landed and the cut would delete no source (or, read loosely,
    // the wrong ones). Per-file calls report each file.
    const items = [{ is_dir: false }, { is_dir: false }];
    const split = splitForFileBatch(items, { aeroVaultOverlay: false, gitHostUpload: false, legacyCut: true });
    expect(split.batch).toEqual([]);
    expect(split.sequential).toHaveLength(2);
  });

  it('every item ends up in exactly one of the two lists', () => {
    const items = [...files(3), { name: 'd1', is_dir: true }, { name: 'd2', is_dir: true }];
    const split = splitForFileBatch(items, plain);
    expect([...split.batch, ...split.sequential].sort((a, b) => a.name.localeCompare(b.name)))
      .toEqual([...items].sort((a, b) => a.name.localeCompare(b.name)));
  });
});

describe('fileBatchCommand (issue #591)', () => {
  // The protocols named in the report and the follow-up question, plus FTP
  // and FTPS: before the fix the batch branch was gated on
  // `!usesProviderApi(protocol)`, which is false for every one of them.
  const protocols: ProviderType[] = ['webdav', 'sftp', 's3', 'ftp', 'ftps', 'azure', 'googledrive'];

  it.each(protocols)('%s sessions use the provider file batch', protocol => {
    expect(usesProviderApi(protocol)).toBe(true);
    expect(fileBatchCommand('download', usesProviderApi(protocol))).toBe('provider_download_files_batch');
    expect(fileBatchCommand('upload', usesProviderApi(protocol))).toBe('provider_upload_files_batch');
  });

  it('a session with no protocol keeps the legacy FTP-manager batch', () => {
    expect(usesProviderApi(undefined)).toBe(false);
    expect(fileBatchCommand('download', false)).toBe('download_files_batch');
    expect(fileBatchCommand('upload', false)).toBe('upload_files_batch');
  });
});

describe('folderTransferIsComplete', () => {
  const outcome = (completed: number, skipped: number, failed: number, cancelled = false) =>
    ({ completed, skipped, failed, cancelled, message: '' });

  it('a folder where every file landed is complete', () => {
    expect(folderTransferIsComplete(outcome(5, 0, 0))).toBe(true);
  });

  it('an empty folder is complete (nothing to lose)', () => {
    expect(folderTransferIsComplete(outcome(0, 0, 0))).toBe(true);
  });

  it('a failed file makes it incomplete', () => {
    expect(folderTransferIsComplete(outcome(4, 0, 1))).toBe(false);
  });

  it('a skipped file makes it incomplete: the destination copy was not written by us', () => {
    expect(folderTransferIsComplete(outcome(4, 1, 0))).toBe(false);
    expect(folderTransferIsComplete(outcome(0, 5, 0))).toBe(false);
  });

  it('a cancelled run is incomplete whatever it did', () => {
    expect(folderTransferIsComplete(outcome(5, 0, 0, true))).toBe(false);
  });

  it('a missing answer is incomplete', () => {
    expect(folderTransferIsComplete(undefined)).toBe(false);
    expect(folderTransferIsComplete(null)).toBe(false);
  });
});

describe('keepsSingleFilePath', () => {
  const base = {
    direction: 'download' as const,
    isProviderSession: true,
    protocol: 'webdav',
    action: 'overwrite',
    destinationExists: false,
    renamed: false,
    destinationClaimed: false,
  };

  it('a new file goes in the batch on every protocol', () => {
    for (const protocol of ['webdav', 'sftp', 's3', 'ftp']) {
      expect(keepsSingleFilePath({ ...base, protocol })).toBe(false);
    }
  });

  it('an SFTP file over an existing destination keeps the delta-capable path', () => {
    expect(keepsSingleFilePath({ ...base, protocol: 'sftp', destinationExists: true })).toBe(true);
    expect(keepsSingleFilePath({ ...base, protocol: 'sftp', destinationExists: true, direction: 'upload' })).toBe(true);
  });

  it('a renamed SFTP target is a new file: batch', () => {
    expect(keepsSingleFilePath({ ...base, protocol: 'sftp', destinationExists: true, renamed: true })).toBe(false);
  });

  it('an existing destination on a protocol without delta goes in the batch', () => {
    expect(keepsSingleFilePath({ ...base, protocol: 'webdav', destinationExists: true })).toBe(false);
  });

  it('a resumed upload keeps the command that continues from the partial', () => {
    expect(keepsSingleFilePath({ ...base, direction: 'upload', action: 'resume', destinationExists: true })).toBe(true);
  });

  it('the legacy session with no protocol never splits', () => {
    expect(keepsSingleFilePath({ ...base, isProviderSession: false, protocol: 'sftp', destinationExists: true })).toBe(false);
  });

  it('a second file for a destination already in this transfer runs after the batch, never beside it', () => {
    // Two picked files with the same name from different folders map to one
    // remote path; in one batch they would be written concurrently and the
    // result would depend on which finished last.
    expect(keepsSingleFilePath({ ...base, destinationClaimed: true })).toBe(true);
    expect(keepsSingleFilePath({ ...base, direction: 'upload', destinationClaimed: true })).toBe(true);
    expect(keepsSingleFilePath({ ...base, destinationClaimed: false })).toBe(false);
  });
});
