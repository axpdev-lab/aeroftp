// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import en from './locales/en.json';

describe('Drime free-tier quota strings', () => {
  it('uses the same GB number in desc and tooltip', () => {
    // Pin (review on merged #520): drimeDesc said 21 GB while drimeTooltip said
    // 20GB. Official free tier is 20 GB (drime.cloud docs + README table).
    const protocol = (en as {
      translations: { protocol: { drimeDesc: string; drimeTooltip: string } };
    }).translations.protocol;
    const desc = protocol.drimeDesc;
    const tooltip = protocol.drimeTooltip;
    const descGb = desc.match(/(\d+)\s*GB/i)?.[1];
    const tipGb = tooltip.match(/(\d+)\s*GB/i)?.[1];
    expect(descGb, `drimeDesc=${desc}`).toBeDefined();
    expect(tipGb, `drimeTooltip=${tooltip}`).toBeDefined();
    expect(descGb).toBe(tipGb);
    expect(descGb).toBe('20');
  });
});
