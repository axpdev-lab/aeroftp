// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// The row model and the pure logic behind the shared trash table.
//
// Every provider ships its own '🗑 View Trash' dialog — seventeen of them — and
// they had grown into seventeen copies of the same table: no sorting anywhere,
// no way to tell files from folders except the icon, timestamps wrapping onto
// two lines, and selection by one click at a time (discussion #347). Fixing
// that in each copy would have been seventeen chances to diverge, so the table
// itself is shared and this file holds the parts worth testing on their own.

/** One entry of a provider's trash, normalized for the shared table. */
export interface TrashRow {
    /** Stable identity for selection: the provider's own id or path. */
    id: string;
    name: string;
    isDir: boolean;
    /** Bytes; null or undefined for a folder, or when the provider omits it. */
    size?: number | null;
    /** When it was deleted, as the provider reports it: parsed for sorting and
     *  formatted for display unless the two fields below say otherwise. */
    deletedAt?: string | null;
    /** Epoch milliseconds, for the providers that report a number rather than a
     *  parseable string (Nextcloud). Wins over `deletedAt` when sorting. */
    deletedAtMs?: number | null;
    /** Pre-rendered text, for the providers whose own string is already the
     *  display form (Koofr). Wins over `deletedAt` when rendering. */
    deletedAtLabel?: string | null;
}

/** The instant a row was deleted, whichever way its provider reported it. */
export function trashRowDeletedMs(row: TrashRow): number | null {
    if (typeof row.deletedAtMs === 'number' && Number.isFinite(row.deletedAtMs)) {
        return row.deletedAtMs;
    }
    return parseDeletedAt(row.deletedAt);
}

export type TrashSortKey = 'type' | 'name' | 'size' | 'deletedAt';
export type SortDirection = 'asc' | 'desc';

export interface TrashSort {
    key: TrashSortKey;
    direction: SortDirection;
}

/**
 * Clicking a header sorts by it ascending; clicking the active one flips the
 * direction. Name ascending is the useful default for a fresh column, except
 * for size and date where "biggest" and "most recent" are what a person is
 * actually looking for.
 */
export function nextTrashSort(current: TrashSort | null, key: TrashSortKey): TrashSort {
    if (current && current.key === key) {
        return { key, direction: current.direction === 'asc' ? 'desc' : 'asc' };
    }
    return { key, direction: key === 'size' || key === 'deletedAt' ? 'desc' : 'asc' };
}

/** Parse whatever the provider called a timestamp. Unparseable sorts last. */
export function parseDeletedAt(value?: string | null): number | null {
    if (!value) return null;
    const ms = Date.parse(value);
    return Number.isFinite(ms) ? ms : null;
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

/**
 * Sort without mutating the input. Folders and files are only forced apart by
 * the 'type' key: on the other keys a folder is just another row, which is what
 * makes "the five biggest things in here" answerable.
 *
 * Ties always fall through to the name, so the order is total and a re-render
 * cannot shuffle equal rows.
 */
export function sortTrashRows(rows: readonly TrashRow[], sort: TrashSort | null): TrashRow[] {
    if (!sort) return [...rows];
    const dir = sort.direction === 'asc' ? 1 : -1;
    const byName = (a: TrashRow, b: TrashRow) => collator.compare(a.name, b.name);

    return [...rows].sort((a, b) => {
        switch (sort.key) {
            case 'type': {
                if (a.isDir !== b.isDir) return (a.isDir ? -1 : 1) * dir;
                return byName(a, b);
            }
            case 'name':
                return byName(a, b) * dir;
            case 'size': {
                // A folder has no size of its own; keep folders together at the
                // end rather than pretending they are zero bytes.
                const av = a.isDir ? null : a.size ?? null;
                const bv = b.isDir ? null : b.size ?? null;
                if (av === null && bv === null) return byName(a, b);
                if (av === null) return 1;
                if (bv === null) return -1;
                if (av === bv) return byName(a, b);
                return (av - bv) * dir;
            }
            case 'deletedAt': {
                const av = trashRowDeletedMs(a);
                const bv = trashRowDeletedMs(b);
                if (av === null && bv === null) return byName(a, b);
                if (av === null) return 1; // unknown dates last, both directions
                if (bv === null) return -1;
                if (av === bv) return byName(a, b);
                return (av - bv) * dir;
            }
            default:
                return 0;
        }
    });
}

/**
 * What a click on row `index` does to the selection.
 *
 * These rows are checkboxes, so a plain click toggles one row and leaves the
 * rest alone — Ctrl does the same thing, which is what Ehud asked for when he
 * asked for Ctrl "like in file managers": adding to a selection without losing
 * it. Shift extends from the last row clicked, and always ADDS the range rather
 * than replacing the selection, so several ranges can be picked in turn.
 */
export function selectionAfterRowClick(
    rows: readonly TrashRow[],
    index: number,
    selected: ReadonlySet<string>,
    anchor: number | null,
    modifiers: { shift?: boolean } = {},
): { selected: Set<string>; anchor: number | null } {
    const row = rows[index];
    if (!row) return { selected: new Set(selected), anchor };

    if (modifiers.shift && anchor !== null && anchor >= 0 && anchor < rows.length) {
        const [from, to] = anchor <= index ? [anchor, index] : [index, anchor];
        const next = new Set(selected);
        for (let i = from; i <= to; i++) next.add(rows[i].id);
        // The anchor stays put, so shift-clicking again re-extends from the
        // same origin instead of walking away one click at a time.
        return { selected: next, anchor };
    }

    const next = new Set(selected);
    if (next.has(row.id)) next.delete(row.id);
    else next.add(row.id);
    return { selected: next, anchor: index };
}
