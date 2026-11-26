// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { accountChangeGeneration } from '../../utils/userPartitions';

/** What a palette read captured when it started. */
export interface PaletteReadToken {
    generation: number;
    accountBefore: number;
}

/**
 * Read guard for the account-scoped palette reads in IntroHub.
 *
 * A read is identified by a monotonic generation AND the
 * `accountChangeGeneration()` captured when it started: a completion is
 * current only if no newer read began and no account switch/lock happened
 * meanwhile, so a read that answered after an account switch is discarded
 * instead of committing the other account's profile list. Writers (an import
 * that appends profiles) invalidate the generation, so a read still in flight
 * cannot commit over the imported list.
 */
export class PaletteReadGuard {
    private generation = 0;

    /** Begin a read: the returned token must still match at completion. */
    begin(): PaletteReadToken {
        this.generation += 1;
        return { generation: this.generation, accountBefore: accountChangeGeneration() };
    }

    /** Invalidate every in-flight read (called by list writers, e.g. import). */
    invalidate(): void {
        this.generation += 1;
    }

    /** True when `token` still identifies the latest read on the same account. */
    isCurrent(token: PaletteReadToken): boolean {
        return (
            token.generation === this.generation &&
            token.accountBefore === accountChangeGeneration()
        );
    }
}
