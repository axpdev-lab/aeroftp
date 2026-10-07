// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { expect, it, vi } from 'vitest';
import { runOwnedConnectionCleanup } from './connectionCleanup';

it.each(['fulfilled', 'rejected'])('does not disconnect or reset a newer session after %s stale overlay cleanup', async outcome => {
    let generation = 1;
    let settle!: () => void;
    const overlay = new Promise<void>((resolve, reject) => {
        settle = outcome === 'fulfilled' ? resolve : () => reject(new Error('old overlay gone'));
    });
    const disconnect = vi.fn(async () => {});
    const resetWorkspace = vi.fn();
    const flow = (async () => {
        if (await runOwnedConnectionCleanup(() => generation === 1, [() => overlay, disconnect])) resetWorkspace();
    })();
    generation = 2;
    settle();
    await flow;
    expect(disconnect).not.toHaveBeenCalled();
    expect(resetWorkspace).not.toHaveBeenCalled();
});

it('continues other protocol cleanup after an error while ownership remains current', async () => {
    const disconnect = vi.fn(async () => {});
    expect(await runOwnedConnectionCleanup(() => true, [async () => { throw new Error('not this protocol'); }, disconnect])).toBe(true);
    expect(disconnect).toHaveBeenCalledOnce();
});
