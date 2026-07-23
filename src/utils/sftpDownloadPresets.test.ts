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
  resolveFtpDownloadSegments,
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

describe('FTP and FTPS transfer presets', () => {
  it('keeps the Safe, Balanced, Max channel model, cycle, and description keys', () => {
    expect(TRANSFER_SPEED_PRESETS).toEqual({
      base: { label: 'Safe', channels: 1, descriptionKey: 'transfer.modeSafeDescription' },
      fast: { label: 'Balanced', channels: 3, descriptionKey: 'transfer.modeBalancedDescription' },
      super: { label: 'Max', channels: 5, descriptionKey: 'transfer.modeMaxDescription' },
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

  it('lets an explicit download-segments setting win over the preset', () => {
    expect(resolveFtpDownloadSegments(8, true, 3)).toBe(8);
    expect(resolveFtpDownloadSegments(2, true, 5)).toBe(2);
    expect(resolveFtpDownloadSegments(4, false, 3)).toBe(4);
  });

  it('maps the preset channels onto single-file FTP/FTPS downloads in Auto', () => {
    expect(resolveFtpDownloadSegments(0, true, 1)).toBe(1);
    expect(resolveFtpDownloadSegments(0, true, 3)).toBe(3);
    expect(resolveFtpDownloadSegments(0, true, 5)).toBe(5);
  });

  it('keeps the legacy single-stream contract for non-FTP protocols in Auto', () => {
    expect(resolveFtpDownloadSegments(0, false, 1)).toBeUndefined();
    expect(resolveFtpDownloadSegments(0, false, 5)).toBeUndefined();
  });
});
