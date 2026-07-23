// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

export type TransferSpeedPreset = 'base' | 'fast' | 'super';

export const TRANSFER_SPEED_PRESETS: Record<
  TransferSpeedPreset,
  { label: string; channels: number; descriptionKey: string }
> = {
  base: { label: 'Safe', channels: 1, descriptionKey: 'transfer.modeSafeDescription' },
  fast: { label: 'Balanced', channels: 3, descriptionKey: 'transfer.modeBalancedDescription' },
  super: { label: 'Max', channels: 5, descriptionKey: 'transfer.modeMaxDescription' },
};

/**
 * Single-file FTP/FTPS contract (speed-button audit, PD-FTP-1): the toolbar
 * speed preset supplies the intra-file channel count for one large download
 * (N independent connections with byte ranges), while an explicit
 * "download segments" setting always wins. Returns undefined when neither
 * applies, keeping the legacy single-stream behaviour for other protocols.
 */
export const resolveFtpDownloadSegments = (
  explicitSegments: number,
  supportsFtpPresets: boolean,
  presetChannels: number,
): number | undefined => {
  if (explicitSegments > 0) return explicitSegments;
  return supportsFtpPresets ? presetChannels : undefined;
};

export const deriveTransferSpeedPreset = (channels: number): TransferSpeedPreset => {
  if (channels <= 1) return 'base';
  if (channels <= 3) return 'fast';
  return 'super';
};

export const nextTransferSpeedPreset = (current: TransferSpeedPreset): TransferSpeedPreset => {
  if (current === 'base') return 'fast';
  if (current === 'fast') return 'super';
  return 'base';
};
