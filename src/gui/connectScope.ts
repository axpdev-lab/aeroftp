// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** Internal lifetime only: never carries a profile, credential or request args. */
export class ConnectScope {
    private cancellation: Error | undefined;
    private readonly callbacks = new Set<() => void>();
    constructor(private readonly guard: () => void = () => {}, private readonly check?: () => Promise<void>) {}
    assert(): void {
        if (this.cancellation) throw this.cancellation;
        this.guard();
    }
    cancel(reason: Error): void {
        if (this.cancellation) return;
        this.cancellation = reason;
        for (const callback of this.callbacks) { try { callback(); } catch { /* best-effort backend cancellation */ } }
        this.callbacks.clear();
    }
    onCancel(callback: () => void): () => void {
        this.assert(); this.callbacks.add(callback);
        return () => this.callbacks.delete(callback);
    }
    /** Account and lease checks surround both successful and rejected awaits. */
    async step<T>(run: () => T | Promise<T>): Promise<T> {
        const checkpoint = async () => {
            this.assert();
            if (this.check) {
                try { await this.check(); } catch (error) {
                    this.cancel(error instanceof Error ? error : new Error('CONNECT_CANCELLED'));
                    throw error;
                }
            }
            this.assert();
        };
        await checkpoint();
        try { return await run(); } finally { await checkpoint(); }
    }
    async cancellable<T>(cancel: () => void | Promise<void>, run: () => Promise<T>): Promise<T> {
        let cancellationWork: Promise<void> | undefined;
        const remove = this.onCancel(() => { cancellationWork = Promise.resolve(cancel()).catch(() => {}); });
        try { return await this.step(run); } finally { remove(); await cancellationWork; }
    }
    /** Share cancellation with the controller while adding an account check. */
    checked(check: () => Promise<void>): ConnectScope {
        const child = new ConnectScope(() => this.assert(), check);
        const remove = this.onCancel(() => child.cancel(new Error('CONNECT_CANCELLED')));
        // The parent owns this child's lifetime; no credential data is retained.
        child.onCancel(remove);
        return child;
    }
}
export type ProfileConnectOutcome = 'connected' | 'pending_human' | 'failed';
export type ProfileConnector = (profileId: string, scope: ConnectScope) => Promise<ProfileConnectOutcome>;
export type RegisterProfileConnector = (connector: ProfileConnector) => () => void;
