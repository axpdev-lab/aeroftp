// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Vault-aware My Servers sidebar rule (#311 D3): the static filter chips are a
// map of the vault, not a fixed menu, so a sparse vault shows a short list
// instead of a dozen 0-count protocol chips. The rule, kept pure here so it is
// unit-tested in the node vitest env (the sidebar component itself is not):
//   - `all` is always pinned (the escape hatch back to the full table);
//   - `favorites` / `encrypted` hide when their count is 0;
//   - protocol chips show only when non-empty.
// User-created groups are NOT covered here: they ALWAYS render, even at 0
// members (the owner exception), straight from the groups array.

import type { MyServersFilterBy } from '../types/catalog';

export const QUICK_FILTER_IDS: MyServersFilterBy[] = ['all', 'favorites', 'encrypted'];
export const PROTOCOL_FILTER_IDS: MyServersFilterBy[] = ['ftp', 's3', 'webdav', 'cloud', 'media', 'dev', 'local-bridge'];

/** Quick chips to show: `all` always, the rest only when their count > 0. */
export function visibleQuickFilters(counts: Partial<Record<MyServersFilterBy, number>>): MyServersFilterBy[] {
    return QUICK_FILTER_IDS.filter((id) => id === 'all' || (counts[id] ?? 0) > 0);
}

/** Protocol chips to show: only those with a non-zero count. */
export function visibleProtocolFilters(counts: Partial<Record<MyServersFilterBy, number>>): MyServersFilterBy[] {
    return PROTOCOL_FILTER_IDS.filter((id) => (counts[id] ?? 0) > 0);
}
