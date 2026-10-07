// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** Finish best-effort teardown only while the cancelled connection still owns it.
 * An already dispatched backend call must settle; later calls and UI resets must
 * not affect a newer connection or navigation that took over during that await.
 */
export async function runOwnedConnectionCleanup(
    owns: () => boolean,
    steps: ReadonlyArray<() => Promise<unknown>>,
): Promise<boolean> {
    for (const step of steps) {
        if (!owns()) return false;
        try { await step(); } catch { /* other protocol cleanup must still run */ }
    }
    return owns();
}
