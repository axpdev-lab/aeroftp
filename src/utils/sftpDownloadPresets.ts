// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { ProviderType } from '../types';

export type SftpDownloadPreset =
  | 'compatibility'
  | 'efficient'
  | 'balanced'
  | 'fast'
  | 'maximum-tested';

export const DEFAULT_SFTP_DOWNLOAD_PRESET: SftpDownloadPreset = 'efficient';

export const SFTP_DOWNLOAD_PRESETS: ReadonlyArray<{
  id: SftpDownloadPreset;
  connections: 1 | 4 | 8 | 12;
  labelKey: string;
  descriptionKey: string;
}> = [
  {
    id: 'compatibility',
    connections: 1,
    labelKey: 'transfer.sftpModeCompatibility',
    descriptionKey: 'transfer.sftpModeCompatibilityDescription',
  },
  {
    id: 'efficient',
    connections: 1,
    labelKey: 'transfer.sftpModeEfficient',
    descriptionKey: 'transfer.sftpModeEfficientDescription',
  },
  {
    id: 'balanced',
    connections: 4,
    labelKey: 'transfer.sftpModeBalanced',
    descriptionKey: 'transfer.sftpModeBalancedDescription',
  },
  {
    id: 'fast',
    connections: 8,
    labelKey: 'transfer.sftpModeFast',
    descriptionKey: 'transfer.sftpModeFastDescription',
  },
  {
    id: 'maximum-tested',
    connections: 12,
    labelKey: 'transfer.sftpModeMaximumTested',
    descriptionKey: 'transfer.sftpModeMaximumTestedDescription',
  },
];

const VALID_SFTP_DOWNLOAD_PRESETS = new Set<SftpDownloadPreset>(
  SFTP_DOWNLOAD_PRESETS.map(({ id }) => id),
);

export const normalizeSftpDownloadPreset = (value: unknown): SftpDownloadPreset => {
  return typeof value === 'string' && VALID_SFTP_DOWNLOAD_PRESETS.has(value as SftpDownloadPreset)
    ? value as SftpDownloadPreset
    : DEFAULT_SFTP_DOWNLOAD_PRESET;
};

export const nextSftpDownloadPreset = (current: SftpDownloadPreset): SftpDownloadPreset => {
  const index = SFTP_DOWNLOAD_PRESETS.findIndex(({ id }) => id === current);
  return SFTP_DOWNLOAD_PRESETS[(index + 1) % SFTP_DOWNLOAD_PRESETS.length].id;
};

export const getSftpDownloadPresetDefinition = (preset: SftpDownloadPreset) => {
  return SFTP_DOWNLOAD_PRESETS.find(({ id }) => id === preset)
    ?? SFTP_DOWNLOAD_PRESETS[1];
};

export const buildSftpDownloadPresetPayload = (
  protocol: ProviderType | undefined,
  preset: SftpDownloadPreset,
): { sftpDownloadPreset?: SftpDownloadPreset } => {
  return protocol === 'sftp' ? { sftpDownloadPreset: preset } : {};
};
