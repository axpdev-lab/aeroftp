// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);
/** Rebase a dialog's deliberate edits onto the latest public preferences. */
export function mergeAppSettingsDraft<T extends object>(base: T, edited: T, latest: T): T {
    const merged = { ...latest };
    for (const key of Object.keys(edited) as (keyof T)[]) {
        if (['__proto__', 'constructor', 'prototype'].includes(String(key))) continue;
        if (!same(base[key], edited[key])) merged[key] = edited[key];
    }
    return merged;
}
