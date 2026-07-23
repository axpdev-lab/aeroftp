// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

export type TransferSpeedPreset = 'base' | 'fast' | 'super';

export const TRANSFER_SPEED_PRESETS: Record<
  TransferSpeedPreset,
  { label: string; channels: number }
> = {
  base: { label: 'Safe', channels: 1 },
  fast: { label: 'Balanced', channels: 3 },
  super: { label: 'Max', channels: 5 },
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
