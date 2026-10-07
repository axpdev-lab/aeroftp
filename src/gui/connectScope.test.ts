// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { expect, it, vi } from 'vitest';
import { ConnectScope } from './connectScope';
function deferred<T>() {
    let resolve!: (value: T) => void;
    const promise = new Promise<T>(yes => { resolve = yes; });
    return { promise, resolve };
}
it('refuses a late credential result before the subsequent connect dispatch', async () => {
    const scope = new ConnectScope(); const credential = deferred<string>(); const connect = vi.fn();
    const flow = (async () => { const password = await scope.step(() => credential.promise); return scope.step(() => connect(password)); })();
    await Promise.resolve(); scope.cancel(new Error('lease_interrupted')); credential.resolve('PRIVATE_SENTINEL');
    await expect(flow).rejects.toThrow('lease_interrupted'); expect(connect).not.toHaveBeenCalled();
});
it('checks account ownership around rejected waits so cancellation cannot initiate OAuth re-auth', async () => {
    let account = 1; const auth = deferred<void>(); const reauth = vi.fn();
    const scope = new ConnectScope(() => {}, async () => { if (account !== 1) throw new Error('scope_changed'); });
    const flow = (async () => { try { await scope.step(() => auth.promise.then(() => { throw new Error('token expired'); })); }
        catch { scope.assert(); await reauth(); } })();
    await Promise.resolve(); account = 2; auth.resolve();
    await expect(flow).rejects.toThrow('scope_changed'); expect(reauth).not.toHaveBeenCalled();
});
it('waits for bound backend cancellation cleanup and detaches it before a later attempt', async () => {
    const scope = new ConnectScope(); const operation = deferred<string>(); const cleanup = deferred<void>();
    const cancel = vi.fn(() => cleanup.promise);
    let settled = false; const flow = scope.cancellable(cancel, () => operation.promise).finally(() => { settled = true; });
    await Promise.resolve(); scope.cancel(new Error('stopped')); operation.resolve('late');
    await new Promise(resolve => setTimeout(resolve, 10)); expect(cancel).toHaveBeenCalledOnce(); expect(settled).toBe(false);
    cleanup.resolve(); await expect(flow).rejects.toThrow('stopped'); expect(settled).toBe(true);
    const completed = new ConnectScope(); const staleCancel = vi.fn();
    expect(await completed.cancellable(staleCancel, async () => 'ok')).toBe('ok');
    completed.cancel(new Error('later')); expect(staleCancel).not.toHaveBeenCalled();
});
it('propagates parent Stop to an account-scoped child without dispatching more work', async () => {
    const parent = new ConnectScope(); const child = parent.checked(async () => {}); const backend = vi.fn();
    parent.cancel(new Error('Stop'));
    await expect(child.step(backend)).rejects.toThrow('CONNECT_CANCELLED'); expect(backend).not.toHaveBeenCalled();
});
