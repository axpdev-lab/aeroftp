// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { describe, expect, it, vi } from 'vitest';
import { createLockNow, type LockPolicy, type LockScope } from './lockNow';
function harness(policy: LockPolicy = { account: true, vault: true }) {
    let version = 0;
    const order: string[] = [];
    const deps = {
        policy: vi.fn(async () => policy), contextVersion: () => version,
        interruptController: vi.fn(() => { order.push('interrupt'); }),
        lockAccount: vi.fn(async () => { order.push('account'); }),
        lockVault: vi.fn(async () => { order.push('vault'); }),
        confirmed: vi.fn(() => { order.push('confirmed'); }),
    };
    return { deps, order, action: createLockNow(deps), change: () => version++ };
}
describe('confirmed Lock Now', () => {
    it.each([
        [{ account: true, vault: false }, ['interrupt', 'account', 'confirmed']],
        [{ account: false, vault: true }, ['interrupt', 'vault', 'confirmed']],
        [{ account: true, vault: true }, ['interrupt', 'account', 'vault', 'confirmed']],
        [{ account: false, vault: false }, ['interrupt']],
    ] as [LockPolicy, string[]][])('applies the configured policy %j in backend-first order', async (policy, expected) => {
        const h = harness(policy); const result = await h.action();
        expect(h.order).toEqual(expected);
        expect(result.accountLocked).toBe(policy.account); expect(result.vaultLocked).toBe(policy.vault);
        if (!policy.account && !policy.vault) expect(result.unavailable).toBe(true);
    });
    it.each(['account', 'vault'] as LockScope[])('retains the %s-only control policy', async scope => {
        const h = harness(); await h.action(scope);
        expect(h.order).toEqual(['interrupt', scope, 'confirmed']);
    });
    it('never claims a failed account lock, and does not continue to the vault', async () => {
        const h = harness(); h.deps.lockAccount.mockRejectedValue(new Error('failed'));
        const result = await h.action();
        expect(result).toEqual({ accountLocked: false, vaultLocked: false, error: expect.any(Error) });
        expect(h.deps.confirmed).not.toHaveBeenCalled(); expect(h.deps.lockVault).not.toHaveBeenCalled();
    });
    it('publishes only the confirmed account if the subsequent vault lock fails', async () => {
        const h = harness(); h.deps.lockVault.mockRejectedValue(new Error('failed'));
        const result = await h.action();
        expect(result.accountLocked).toBe(true); expect(result.vaultLocked).toBe(false); expect(result.error).toBeInstanceOf(Error);
        expect(h.deps.confirmed).toHaveBeenCalledWith(result);
    });
    it('coalesces duplicate requests and refuses a competing scope', async () => {
        const h = harness(); let finish!: () => void;
        h.deps.lockAccount.mockImplementation(() => new Promise<void>(resolve => { finish = resolve; }));
        const first = h.action(); const duplicate = h.action();
        expect(duplicate).toBe(first); expect((await h.action('vault')).error).toBe('LOCK_BUSY');
        await Promise.resolve(); finish(); await first;
        expect(h.deps.policy).toHaveBeenCalledTimes(1); expect(h.deps.lockAccount).toHaveBeenCalledTimes(1); expect(h.deps.lockVault).toHaveBeenCalledTimes(1);
    });
    it('drops a request if the account changes during policy discovery', async () => {
        const h = harness(); h.deps.policy.mockImplementation(async () => { h.change(); return { account: true, vault: true }; });
        expect((await h.action()).stale).toBe(true); expect(h.deps.lockAccount).not.toHaveBeenCalled();
    });
    it('publishes the confirmed account on a stale request without locking the subsequent vault', async () => {
        const h = harness(); h.deps.lockAccount.mockImplementation(async () => { h.change(); });
        expect((await h.action()).stale).toBe(true);
        expect(h.deps.lockVault).not.toHaveBeenCalled();
        expect(h.deps.confirmed).toHaveBeenCalledWith({ accountLocked: true, vaultLocked: false });
    });
    it('publishes a vault lock confirmed after a profile refresh while retaining stale status', async () => {
        const h = harness({ account: false, vault: true });
        h.deps.lockVault.mockImplementation(async () => { h.change(); });
        const result = await h.action();
        expect(result).toEqual({ accountLocked: false, vaultLocked: true, stale: true });
        expect(h.deps.confirmed).toHaveBeenCalledWith({ accountLocked: false, vaultLocked: true });
    });
    it('publishes both confirmed layers if the last backend call finishes on a stale context', async () => {
        const h = harness(); h.deps.lockVault.mockImplementation(async () => { h.change(); });
        expect(await h.action()).toEqual({ accountLocked: true, vaultLocked: true, stale: true });
        expect(h.deps.confirmed).toHaveBeenCalledWith({ accountLocked: true, vaultLocked: true });
    });
    it('does not let its own confirmation events turn a successful request stale', async () => {
        const h = harness(); h.deps.confirmed.mockImplementation(() => { h.change(); });
        expect(await h.action()).toEqual({ accountLocked: true, vaultLocked: true });
    });
    it('retains a confirmed account after a failed vault call and concurrent context change', async () => {
        const h = harness(); h.deps.lockVault.mockImplementation(async () => { h.change(); throw new Error('failed'); });
        const result = await h.action();
        expect(result).toEqual({ accountLocked: true, vaultLocked: false, stale: true, error: expect.any(Error) });
        expect(h.deps.confirmed).toHaveBeenCalledWith({ accountLocked: true, vaultLocked: false, error: expect.any(Error) });
    });

});
