// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import {
  DEFAULT_SFTP_DOWNLOAD_PRESET,
  SFTP_DOWNLOAD_PRESETS,
  buildSftpDownloadPresetPayload,
  getSftpDownloadPresetDefinition,
  nextSftpDownloadPreset,
  normalizeSftpDownloadPreset,
} from './sftpDownloadPresets';
import {
  TRANSFER_SPEED_PRESETS,
  deriveTransferSpeedPreset,
  nextTransferSpeedPreset,
} from './ftpTransferSpeedPresets';

describe('SFTP download presets', () => {
  it('defaults invalid and missing persisted values to Efficient', () => {
    expect(DEFAULT_SFTP_DOWNLOAD_PRESET).toBe('efficient');
    expect(normalizeSftpDownloadPreset(undefined)).toBe('efficient');
    expect(normalizeSftpDownloadPreset('turbo')).toBe('efficient');
  });

  it('preserves every valid backend preset ID', () => {
    for (const { id } of SFTP_DOWNLOAD_PRESETS) {
      expect(normalizeSftpDownloadPreset(id)).toBe(id);
    }
  });

  it('cycles all five presets in order and wraps', () => {
    expect(SFTP_DOWNLOAD_PRESETS.map(({ id }) => id)).toEqual([
      'compatibility',
      'efficient',
      'balanced',
      'fast',
      'maximum-tested',
    ]);
    expect(nextSftpDownloadPreset('compatibility')).toBe('efficient');
    expect(nextSftpDownloadPreset('efficient')).toBe('balanced');
    expect(nextSftpDownloadPreset('balanced')).toBe('fast');
    expect(nextSftpDownloadPreset('fast')).toBe('maximum-tested');
    expect(nextSftpDownloadPreset('maximum-tested')).toBe('compatibility');
  });

  it('exposes the backend connection labels without changing preset semantics', () => {
    expect(SFTP_DOWNLOAD_PRESETS.map(({ connections }) => connections)).toEqual([1, 1, 4, 8, 12]);
    expect(getSftpDownloadPresetDefinition('maximum-tested').labelKey)
      .toBe('transfer.sftpModeMaximumTested');
  });

  it('adds the camelCase Tauri field only to both SFTP download payloads', () => {
    const folderPayload = {
      remotePath: '/remote/folder',
      localPath: '/local/folder',
      ...buildSftpDownloadPresetPayload('sftp', 'balanced'),
    };
    const filePayload = {
      remotePath: '/remote/file',
      localPath: '/local/file',
      ...buildSftpDownloadPresetPayload('sftp', 'fast'),
    };

    expect(folderPayload.sftpDownloadPreset).toBe('balanced');
    expect(filePayload.sftpDownloadPreset).toBe('fast');
    expect(buildSftpDownloadPresetPayload('ftp', 'maximum-tested')).toEqual({});
    expect(buildSftpDownloadPresetPayload('ftps', 'maximum-tested')).toEqual({});
  });
});

describe('FTP and FTPS transfer presets remain unchanged', () => {
  it('keeps the Safe, Balanced, Max channel model and cycle', () => {
    expect(TRANSFER_SPEED_PRESETS).toEqual({
      base: { label: 'Safe', channels: 1 },
      fast: { label: 'Balanced', channels: 3 },
      super: { label: 'Max', channels: 5 },
    });
    expect([0, 1, 2, 3, 4, 5].map(deriveTransferSpeedPreset)).toEqual([
      'base',
      'base',
      'fast',
      'fast',
      'super',
      'super',
    ]);
    expect(nextTransferSpeedPreset('base')).toBe('fast');
    expect(nextTransferSpeedPreset('fast')).toBe('super');
    expect(nextTransferSpeedPreset('super')).toBe('base');
  });
});
