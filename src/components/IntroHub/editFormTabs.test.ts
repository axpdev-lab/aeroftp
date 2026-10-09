// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { findOpenEditTab, markEditSessionEnded } from './editFormTabs';

const tab = (id: string, profileId?: string, editSessionEnded?: boolean) => ({
    id,
    editingProfile: profileId ? { id: profileId } : undefined,
    editSessionEnded,
});

describe('edit form tabs', () => {
    it('reuses a tab still editing the profile', () => {
        const tabs = [tab('new-1'), tab('edit-1', 'srv_a'), tab('edit-2', 'srv_b')];
        expect(findOpenEditTab(tabs, 'srv_a')?.id).toBe('edit-1');
        expect(findOpenEditTab(tabs, 'srv_c')).toBeUndefined();
    });

    it('does not reuse a tab whose edit session ended', () => {
        const tabs = markEditSessionEnded([tab('edit-1', 'srv_a'), tab('edit-2', 'srv_b')], 'edit-1');
        expect(findOpenEditTab(tabs, 'srv_a')).toBeUndefined();
        expect(findOpenEditTab(tabs, 'srv_b')?.id).toBe('edit-2');
    });

    it('marks only the named tab and keeps an already marked list as it is', () => {
        const tabs = [tab('edit-1', 'srv_a'), tab('edit-2', 'srv_b')];
        const marked = markEditSessionEnded(tabs, 'edit-1');
        expect(marked[0]).toMatchObject({ id: 'edit-1', editSessionEnded: true });
        expect(marked[1]).toBe(tabs[1]);
        expect(markEditSessionEnded(marked, 'edit-1')[0]).toBe(marked[0]);
    });
});
