// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import {
  formatLossKind,
  operationLossKind,
  activeEditLoss,
  isLossyReencode,
  INITIAL_EDIT_STATE,
  type EditState,
} from './types';

describe('formatLossKind', () => {
  it('marks pixel-exact containers as lossless', () => {
    for (const f of ['png', 'PNG', 'webp', 'bmp', 'tiff']) {
      expect(formatLossKind(f)).toBe('lossless');
    }
  });

  it('marks re-encoding / palette containers as lossy', () => {
    for (const f of ['jpg', 'jpeg', 'JPEG', 'gif']) {
      expect(formatLossKind(f)).toBe('lossy');
    }
  });

  it('defaults unknown formats to lossy', () => {
    expect(formatLossKind('heic')).toBe('lossy');
  });
});

describe('operationLossKind', () => {
  it('treats geometry permutations and invert as lossless', () => {
    for (const op of ['Crop', 'Rotate90', 'Rotate180', 'Rotate270', 'FlipH', 'FlipV', 'Invert']) {
      expect(operationLossKind(op)).toBe('lossless');
    }
  });

  it('treats resampling and colour math as lossy', () => {
    for (const op of ['Resize', 'Brightness', 'Contrast', 'HueRotate', 'Blur', 'Sharpen', 'Grayscale']) {
      expect(operationLossKind(op)).toBe('lossy');
    }
  });
});

describe('activeEditLoss', () => {
  it('reports nothing for a pristine edit state', () => {
    expect(activeEditLoss(INITIAL_EDIT_STATE)).toEqual({ lossless: 0, lossy: 0 });
  });

  it('counts active operations by kind', () => {
    const state: EditState = {
      ...INITIAL_EDIT_STATE,
      rotation: 90, // lossless
      flipH: true, // lossless
      brightness: 20, // lossy
      grayscale: true, // lossy
    };
    expect(activeEditLoss(state)).toEqual({ lossless: 2, lossy: 2 });
  });
});

describe('isLossyReencode', () => {
  it('guards a lossy source re-encoded into a different lossy format', () => {
    expect(isLossyReencode('jpg', 'gif')).toBe(true);
    expect(isLossyReencode('gif', 'jpg')).toBe(true);
    expect(isLossyReencode('jpeg', 'gif')).toBe(true);
  });

  it('does not guard a pass-through save to the same format', () => {
    expect(isLossyReencode('jpg', 'jpg')).toBe(false);
    expect(isLossyReencode('JPG', 'jpg')).toBe(false); // case-insensitive
    expect(isLossyReencode('gif', 'gif')).toBe(false);
  });

  it('does not guard a lossless target (no new loss)', () => {
    expect(isLossyReencode('jpg', 'png')).toBe(false);
    expect(isLossyReencode('gif', 'webp')).toBe(false);
    expect(isLossyReencode('webp', 'bmp')).toBe(false);
  });

  it('does not guard a lossless source (informational note only, per scope)', () => {
    expect(isLossyReencode('png', 'jpg')).toBe(false);
    expect(isLossyReencode('bmp', 'gif')).toBe(false);
  });
});
