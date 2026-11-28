// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { MyServersFilterBy } from '../../types/catalog';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));

import { announceServersImported, PROFILES_CHANGED_EVENT, SERVERS_IMPORTED_EVENT } from '../../utils/serverProfileStore';
import { resetServerListViewForImport, useServersImportedReset } from './serversImportedReset';

let root: Root;
let host: HTMLDivElement;

beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    localStorage.clear();
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
});

afterEach(() => {
    act(() => root.unmount());
    host.remove();
    localStorage.clear();
});

interface ViewSetters {
    setActiveFilter: (filter: MyServersFilterBy) => void;
    setSearchQuery: (query: string) => void;
    setActiveGroupId: (groupId: string | null) => void;
}

const Harness = ({ setActiveFilter, setSearchQuery, setActiveGroupId }: ViewSetters) => {
    useServersImportedReset(setActiveFilter, setSearchQuery, setActiveGroupId);
    return null;
};

const fixture = () => ({
    setActiveFilter: vi.fn<(filter: MyServersFilterBy) => void>(),
    setSearchQuery: vi.fn<(query: string) => void>(),
    setActiveGroupId: vi.fn<(groupId: string | null) => void>(),
});

describe('resetServerListViewForImport', () => {
    it('drops the filter chip, the group and the search, and persists "all"', () => {
        localStorage.setItem('aeroftp_myservers_filter', 'favorites');
        localStorage.setItem('aeroftp_myservers_group', 'g1');
        const s = fixture();
        resetServerListViewForImport(s.setActiveFilter, s.setSearchQuery, s.setActiveGroupId);
        expect(s.setActiveFilter).toHaveBeenCalledWith('all');
        expect(s.setSearchQuery).toHaveBeenCalledWith('');
        expect(s.setActiveGroupId).toHaveBeenCalledWith(null);
        expect(localStorage.getItem('aeroftp_myservers_filter')).toBe('all');
        expect(localStorage.getItem('aeroftp_myservers_group')).toBeNull();
    });
});

describe('useServersImportedReset', () => {
    it('resets the view when an import is announced in this window', async () => {
        const s = fixture();
        await act(async () => {
            root.render(createElement(Harness, s));
        });
        await act(async () => {
            announceServersImported();
        });
        expect(s.setActiveFilter).toHaveBeenCalledWith('all');
        expect(s.setSearchQuery).toHaveBeenCalledWith('');
        expect(s.setActiveGroupId).toHaveBeenCalledWith(null);
    });

    it('does not reset the view on a plain profiles-changed event (saves, deletes)', async () => {
        const s = fixture();
        await act(async () => {
            root.render(createElement(Harness, s));
        });
        await act(async () => {
            window.dispatchEvent(new CustomEvent(PROFILES_CHANGED_EVENT, { detail: { profiles: [] } }));
        });
        expect(s.setActiveFilter).not.toHaveBeenCalled();
        expect(s.setSearchQuery).not.toHaveBeenCalled();
        expect(s.setActiveGroupId).not.toHaveBeenCalled();
    });

    it('removes the listener on unmount', async () => {
        const s = fixture();
        await act(async () => {
            root.render(createElement(Harness, s));
        });
        act(() => root.unmount());
        await act(async () => {
            window.dispatchEvent(new CustomEvent(SERVERS_IMPORTED_EVENT));
        });
        expect(s.setActiveFilter).not.toHaveBeenCalled();
    });
});
