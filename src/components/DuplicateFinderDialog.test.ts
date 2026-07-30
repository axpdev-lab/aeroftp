// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import dialogRaw from './DuplicateFinderDialog.tsx?raw';

describe('DuplicateFinderDialog open-handlers surface failures', () => {
  it('routes open_local_file and open_in_file_manager rejects to setError', () => {
    // Pin (review on merged #519): silent `.catch(() => { /* best-effort */ })`
    // left the open/reveal buttons dead. Both rejects must call setError so the
    // dialog's existing error channel shows the failure.
    const openLocal = dialogRaw.match(
      /invoke\('open_local_file'[\s\S]{0,200}?\.catch\(([^)]*)\)/,
    );
    const openManager = dialogRaw.match(
      /invoke\('open_in_file_manager'[\s\S]{0,200}?\.catch\(([^)]*)\)/,
    );
    expect(openLocal, 'open_local_file catch').toBeTruthy();
    expect(openManager, 'open_in_file_manager catch').toBeTruthy();

    // Each catch body (the next ~120 chars after `.catch`) must call setError.
    const afterOpen = dialogRaw.slice(dialogRaw.indexOf("invoke('open_local_file'"));
    const afterManager = dialogRaw.slice(dialogRaw.indexOf("invoke('open_in_file_manager'"));
    expect(afterOpen.slice(0, 280)).toMatch(/setError\(/);
    expect(afterManager.slice(0, 280)).toMatch(/setError\(/);
    expect(afterOpen.slice(0, 280)).not.toMatch(/best-effort/);
    expect(afterManager.slice(0, 280)).not.toMatch(/best-effort/);
  });

  it('localizes the fuzzy-cutoff placeholder instead of hard-coding "auto"', () => {
    expect(dialogRaw).toMatch(
      /placeholder=\{t\('duplicates\.fuzzyCutoffPlaceholder'\)/,
    );
    expect(dialogRaw).not.toMatch(/placeholder="auto"/);
  });
});
