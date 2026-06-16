// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import {
  formatLossKind,
  operationLossKind,
  activeEditLoss,
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
