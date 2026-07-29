// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { createTabStateStore } from './tabStateStore';

// What the store has to guarantee is exactly what the Plan tab lost on every
// switch to Sync: a value written before the unmount is the value the remount
// reads back, and an untouched control still gets its default.
describe('createTabStateStore', () => {
  it('gives back the fallback for a key nobody wrote', () => {
    const store = createTabStateStore();
    expect(store.get('plan.preset', 'backup')).toBe('backup');
    expect(store.has('plan.preset')).toBe(false);
  });

  it('returns what was written, across as many reads as the remounts take', () => {
    const store = createTabStateStore();
    store.set('plan.preset', 'mirror');
    store.set('plan.parallelStreams', 16);
    expect(store.get('plan.preset', 'backup')).toBe('mirror');
    expect(store.get('plan.preset', 'backup')).toBe('mirror');
    expect(store.get('plan.parallelStreams', 4)).toBe(16);
    expect(store.has('plan.preset')).toBe(true);
  });

  it('keeps a falsy value instead of falling back to the default', () => {
    const store = createTabStateStore();
    store.set('plan.ecEnabled', false);
    store.set('sync.exclude', '');
    store.set('plan.transferBudgetMb', 0);
    // The bug this rules out: `values.get(key) || fallback` would resurrect the
    // default for every control the user deliberately turned off or cleared.
    expect(store.get('plan.ecEnabled', true)).toBe(false);
    expect(store.get('sync.exclude', 'node_modules')).toBe('');
    expect(store.get('plan.transferBudgetMb', 500)).toBe(0);
  });

  it('forgets everything on reset, so a new AeroSync open starts clean', () => {
    const store = createTabStateStore();
    store.set('plan.preset', 'mirror');
    store.reset();
    expect(store.has('plan.preset')).toBe(false);
    expect(store.get('plan.preset', 'backup')).toBe('backup');
  });

  it('scopes tabs by key prefix, so Sync cannot read Plan out from under it', () => {
    const store = createTabStateStore();
    store.set('plan.source', '/plan');
    store.set('sync.source', '/sync');
    expect(store.get('plan.source', '')).toBe('/plan');
    expect(store.get('sync.source', '')).toBe('/sync');
  });
});
