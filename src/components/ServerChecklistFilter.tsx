// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Filter, "select all" and summary for the server checklists of the
// Export/Import dialog (AeroFTP export, bridge export, bridge import
// preview). The three lists share one behavior:
//  - the filter matches the same text as the My Servers search box;
//  - "select all" acts on the rows on screen, and says so while a filter
//    is active;
//  - a selection survives filtering, and selected rows the filter hides
//    are counted under the list, so nothing is exported or imported
//    without being visible somewhere.

import * as React from 'react';
import { useMemo, useState } from 'react';
import { useTranslation } from '../i18n';
import { SearchBox } from './SearchBox';
import {
    SearchableServer,
    countSelectedOutside,
    filterServersByQuery,
    getServerSearchText,
    selectShownLabel,
    toggleVisibleSelection,
} from '../utils/serverListFilter';

type ChecklistServer = SearchableServer & { id: string };

export function useServerChecklistFilter<T extends ChecklistServer>(items: readonly T[]) {
    const [query, setQuery] = useState('');
    const texts = useMemo(() => new Map(items.map(s => [s.id, getServerSearchText(s)])), [items]);
    const visible = useMemo(
        () => filterServersByQuery(items, query, s => texts.get(s.id) ?? ''),
        [items, query, texts],
    );
    return { query, setQuery, visible, filtering: query.trim() !== '' };
}

export const ServerChecklistSearch: React.FC<{
    value: string;
    onChange: (value: string) => void;
    autoFocus?: boolean;
}> = ({ value, onChange, autoFocus }) => {
    const t = useTranslation();
    return (
        <SearchBox
            autoFocus={autoFocus}
            value={value}
            onChange={onChange}
            placeholder={t('settings.filterServersPlaceholder')}
            ariaLabel={t('settings.filterServersPlaceholder')}
            iconSize={14}
            containerClassName="mb-2"
            className="w-full pl-3 pr-3 py-1.5 text-sm rounded-lg border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 focus:outline-none focus:border-blue-400 dark:focus:border-blue-500"
        />
    );
};

/** Select/deselect the selectable rows on screen. Without a filter it reads
 *  "Select all"; with one it names how many shown rows it will touch. */
export const SelectShownButton: React.FC<{
    selected: ReadonlySet<string>;
    shownIds: readonly string[];
    filtering: boolean;
    onChange: (next: Set<string>) => void;
}> = ({ selected, shownIds, filtering, onChange }) => {
    const t = useTranslation();
    if (shownIds.length === 0) return null;
    const { key, params } = selectShownLabel(selected, shownIds, filtering);
    const label = t(key, params);
    return (
        <button
            type="button"
            onClick={() => onChange(toggleVisibleSelection(selected, shownIds))}
            className="text-xs text-blue-500 hover:text-blue-600 font-medium"
        >
            {label}
        </button>
    );
};

/** Shown under a filtered list that has no rows. */
export const ServerChecklistNoMatch: React.FC<{ query: string }> = ({ query }) => {
    const t = useTranslation();
    return (
        <div className="px-3 py-4 text-center text-sm text-gray-400 dark:text-gray-500">
            {t('introHub.noResultsHint', { query: query.trim() })}
        </div>
    );
};

/** "{selected} / {total} selected", plus how many rows the filter shows and
 *  how many selected rows it hides (those are still included). */
export const ServerChecklistSummary: React.FC<{
    selected: ReadonlySet<string>;
    /** Ids that count toward the total (the selectable rows). */
    allIds: readonly string[];
    /** Selectable ids currently on screen. */
    shownIds: readonly string[];
    /** Rows on screen, when some of them cannot be selected (a bridge target
     *  that does not take their protocol). Defaults to `shownIds.length`. */
    shownRows?: number;
    filtering: boolean;
    children?: React.ReactNode;
}> = ({ selected, allIds, shownIds, shownRows, filtering, children }) => {
    const t = useTranslation();
    const selectedCount = allIds.filter(id => selected.has(id)).length;
    const hidden = filtering ? countSelectedOutside(selected, allIds, shownIds) : 0;
    return (
        <div className="text-xs text-gray-500 dark:text-gray-400 mt-1 space-y-0.5">
            <div>
                {selectedCount} / {allIds.length} {t('settings.selected')}
                {filtering && ` · ${t('settings.filterShownCount', { shown: shownRows ?? shownIds.length })}`}
                {children}
            </div>
            {hidden > 0 && (
                <div className="text-amber-600 dark:text-amber-400">
                    {t('settings.selectedHiddenByFilter', { count: hidden })}
                </div>
            )}
        </div>
    );
};
