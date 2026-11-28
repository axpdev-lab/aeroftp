// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect } from 'react';
import { SERVERS_IMPORTED_EVENT } from '../../utils/serverProfileStore';
import type { MyServersFilterBy } from '../../types/catalog';

/**
 * Drop every narrowing of the My Servers list (filter chip, group, search)
 * so profiles an import just stored are visible at once. The vault list
 * itself re-reads on PROFILES_CHANGED_EVENT; without this reset a filter
 * like "favorites" or "active" would hide the new cards until the user
 * clicks "All".
 */
export const resetServerListViewForImport = (
    setActiveFilter: (filter: MyServersFilterBy) => void,
    setSearchQuery: (query: string) => void,
    setActiveGroupId: (groupId: string | null) => void,
): void => {
    setActiveFilter('all');
    try {
        localStorage.setItem('aeroftp_myservers_filter', 'all');
        localStorage.removeItem('aeroftp_myservers_group');
    } catch {
        // storage unavailable: the in-memory reset still applies
    }
    setSearchQuery('');
    setActiveGroupId(null);
};

/**
 * Reset the My Servers view whenever an import stored profiles in this
 * window. The setters are React state setters, so they are stable and the
 * listener is attached once.
 */
export const useServersImportedReset = (
    setActiveFilter: (filter: MyServersFilterBy) => void,
    setSearchQuery: (query: string) => void,
    setActiveGroupId: (groupId: string | null) => void,
): void => {
    useEffect(() => {
        const onImported = () => resetServerListViewForImport(setActiveFilter, setSearchQuery, setActiveGroupId);
        window.addEventListener(SERVERS_IMPORTED_EVENT, onImported);
        return () => window.removeEventListener(SERVERS_IMPORTED_EVENT, onImported);
    }, [setActiveFilter, setSearchQuery, setActiveGroupId]);
};
