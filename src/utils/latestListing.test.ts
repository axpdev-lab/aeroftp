// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { expect, it, vi } from 'vitest';
import { LatestListing } from './latestListing';

function deferred<T>() {
    let resolve!: (value: T) => void;
    let reject!: (error: Error) => void;
    const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}

it('does not let a late old result replace a newer navigation', async () => {
    const listing = new LatestListing();
    const old = deferred<string>();
    const commit = vi.fn(); const loading = vi.fn();
    const first = listing.run(() => old.promise, commit, loading);
    expect(await listing.run(async () => '/newer', commit, loading)).toBe(true);
    old.resolve('/older');
    expect(await first).toBe(false);
    expect(commit).toHaveBeenCalledExactlyOnceWith('/newer');
    expect(loading.mock.calls).toEqual([[true], [true], [false]]);
});

it('keeps loading active and suppresses an old failure while the latest request is pending', async () => {
    const listing = new LatestListing(); const old = deferred<string>(); const newest = deferred<string>();
    const commit = vi.fn(); const loading = vi.fn();
    const first = listing.run(() => old.promise, commit, loading);
    const second = listing.run(() => newest.promise, commit, loading);
    old.reject(new Error('old failure'));
    expect(await first).toBe(false);
    expect(loading.mock.calls).toEqual([[true], [true]]);
    newest.resolve('/newer');
    expect(await second).toBe(true);
    expect(commit).toHaveBeenCalledExactlyOnceWith('/newer');
    expect(loading).toHaveBeenLastCalledWith(false);
});

it('propagates the current failure and ends its loading state without committing', async () => {
    const listing = new LatestListing(); const failure = new Error('current failure');
    const commit = vi.fn(); const loading = vi.fn();
    await expect(listing.run(async () => { throw failure; }, commit, loading)).rejects.toBe(failure);
    expect(commit).not.toHaveBeenCalled(); expect(loading.mock.calls).toEqual([[true], [false]]);
});

it('reports a declined fallback as failure and exposes current ownership to asynchronous loaders', async () => {
    const listing = new LatestListing(); const gate = deferred<null>(); const commit = vi.fn(); const loading = vi.fn();
    let oldIsCurrent!: () => boolean;
    const first = listing.run(isCurrent => { oldIsCurrent = isCurrent; return gate.promise; }, commit, loading);
    expect(oldIsCurrent()).toBe(true);
    expect(await listing.run(async () => null, commit, loading)).toBe(false);
    expect(oldIsCurrent()).toBe(false);
    gate.resolve(null); expect(await first).toBe(false); expect(commit).not.toHaveBeenCalled();
});
