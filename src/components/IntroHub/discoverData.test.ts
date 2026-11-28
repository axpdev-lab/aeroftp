// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { buildDiscoverCategories, getTotalServiceCount } from './discoverData';

describe('Add Service grid: generic OpenStack Swift tile', () => {
    it('appears in the object-storage category, searchable and launchable to the plain Swift form', () => {
        const objectStorage = buildDiscoverCategories().find(c => c.id === 'object-storage');
        expect(objectStorage).toBeDefined();
        const tile = objectStorage!.items.find(i => i.id === 'swift-generic');
        expect(tile).toBeDefined();
        expect(tile!.name).toBe('OpenStack Swift');
        expect(tile!.protocol).toBe('swift');
        // No providerId: the tile launches Quick Connect on the generic Swift
        // form (auth URL, account, password/key), not on Blomp's preset.
        expect(tile!.providerId).toBeUndefined();
        expect(tile!.badge).toBe('Swift');
        expect(tile!.isGeneric).toBe(true);
        expect(tile!.searchAliases).toEqual(expect.arrayContaining(['openstack', 'swift']));
    });

    it('is counted in the canonical Add Service total', () => {
        const total = getTotalServiceCount();
        const sum = buildDiscoverCategories().reduce((n, c) => n + c.count, 0) + 1;
        expect(total).toBe(sum);
        expect(total).toBeGreaterThan(0);
    });

    it('does not displace Blomp from the cloud-storage category', () => {
        const cloud = buildDiscoverCategories().find(c => c.id === 'cloud-storage');
        expect(cloud!.items.find(i => i.id === 'blomp')).toBeDefined();
    });
});
