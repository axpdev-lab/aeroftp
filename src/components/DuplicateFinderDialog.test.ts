// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import dialogRaw from './DuplicateFinderDialog.tsx?raw';
import appRaw from '../App.tsx?raw';
import en from '../i18n/locales/en.json';

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

describe('the delete flow asks exactly once (#537)', () => {
  /** The JSX body of the `<DuplicateFinderDialog … />` element in App.tsx. */
  const mountSite = (): string => {
    const start = appRaw.indexOf('<DuplicateFinderDialog');
    expect(start, 'DuplicateFinderDialog is mounted in App.tsx').toBeGreaterThan(-1);
    const end = appRaw.indexOf('/>', appRaw.indexOf('onDeleteFiles', start));
    return appRaw.slice(start, end);
  };

  it('does not let the caller raise a second confirmation', () => {
    // This is the deadlock. `ConfirmDialog` is mounted ~300 lines earlier in
    // App.tsx, so the confirm this used to open rendered *behind* the finder's
    // overlay and its backdrop swallowed the clicks, while the promise the
    // callback awaited never settled and "Delete Selected" span forever.
    expect(mountSite()).not.toMatch(/setConfirmDialog/);
  });

  it('hands the setting to the dialog instead of acting on it in the callback', () => {
    expect(mountSite()).toMatch(/confirmBeforeDelete=\{confirmBeforeDelete\}/);
  });

  it('lets the dialog skip its own prompt when the setting is off', () => {
    // `confirmBeforeDelete` has to reach the branch, not just the props: with the
    // caller's prompt gone, a dialog that ignored the setting would confirm even
    // for users who turned confirmations off.
    const handler = dialogRaw.slice(dialogRaw.indexOf('const handleDelete'));
    expect(handler.slice(0, 400)).toMatch(/if \(!confirmBeforeDelete\)/);
    expect(handler.slice(0, 400)).toMatch(/runDelete\(\)/);
  });
});

describe('every string the dialog asks for exists', () => {
  const resolve = (key: string): unknown => {
    let cursor: unknown = (en as { translations: Record<string, unknown> }).translations;
    for (const part of key.split('.')) {
      if (typeof cursor !== 'object' || cursor === null) return undefined;
      cursor = (cursor as Record<string, unknown>)[part];
    }
    return cursor;
  };

  it('resolves in en.json, which is the fallback for all 47 locales', () => {
    // `t()` returns the key itself when nothing resolves, so a typo is not a
    // silent miss: it prints "duplicates.confirmDelete" at the user. That is
    // what the delete confirmation of #537 did — the key was in none of the 47
    // locale files, so the question it asked was its own name.
    const keys = [...dialogRaw.matchAll(/\bt\(\s*'([\w.]+)'/g)].map((m) => m[1]);
    expect(keys.length).toBeGreaterThan(20);
    const missing = [...new Set(keys)].filter((k) => resolve(k) === undefined);
    expect(missing, 'keys used by the dialog but absent from en.json').toEqual([]);
  });
});
