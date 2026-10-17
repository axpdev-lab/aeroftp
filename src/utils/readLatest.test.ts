// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { readLatest } from './readLatest';

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => { resolve = r; });
  return { promise, resolve };
}

describe('readLatest', () => {
  it('lets a lone read commit', async () => {
    expect(await readLatest({ current: 0 }, async () => 'only')).toEqual({ value: 'only', latest: true });
  });

  it('does not let an older read that settles last replace a newer one', async () => {
    const seq = { current: 0 };
    const older = deferred<string>();
    const newer = deferred<string>();
    const first = readLatest(seq, () => older.promise);
    const second = readLatest(seq, () => newer.promise);
    newer.resolve('new');
    expect(await second).toEqual({ value: 'new', latest: true });
    older.resolve('old');
    expect(await first).toEqual({ value: 'old', latest: false });
  });

  it('retires a pending read as soon as a later one starts, whatever the order they settle in', async () => {
    const seq = { current: 0 };
    const a = deferred<number>();
    const b = deferred<number>();
    const first = readLatest(seq, () => a.promise);
    a.resolve(1);
    const second = readLatest(seq, () => b.promise);
    b.resolve(2);
    expect((await first).latest).toBe(false);
    expect((await second).latest).toBe(true);
  });

  it('keeps lists apart: a read on one sequence does not retire a read on another', async () => {
    const volumes = { current: 0 };
    const mounts = { current: 0 };
    const v = readLatest(volumes, async () => 'v');
    const m = readLatest(mounts, async () => 'm');
    expect((await v).latest).toBe(true);
    expect((await m).latest).toBe(true);
  });
});
