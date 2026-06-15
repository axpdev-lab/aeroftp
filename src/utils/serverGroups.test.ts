// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Tests for the server-groups storage helpers (#320). Covers the pure
// normaliser (the defensive coercion of untrusted vault/localStorage payloads)
// and the id generator. The vault-backed load/save paths go through the Tauri
// invoke bridge and are exercised by the live integration run, not here.

import { describe, expect, it } from 'vitest';
import { normalizeServerGroups, newServerGroupId, type ServerGroup } from './serverGroups';

describe('normalizeServerGroups', () => {
    it('returns [] for non-array input', () => {
        expect(normalizeServerGroups(null)).toEqual([]);
        expect(normalizeServerGroups(undefined)).toEqual([]);
        expect(normalizeServerGroups({})).toEqual([]);
        expect(normalizeServerGroups('nope')).toEqual([]);
    });

    it('keeps valid groups and sorts by order', () => {
        const raw = [
            { id: 'b', name: 'Beta', order: 2, members: ['s2'] },
            { id: 'a', name: 'Alpha', order: 1, members: ['s1'] },
        ];
        const out = normalizeServerGroups(raw);
        expect(out.map(g => g.id)).toEqual(['a', 'b']);
        expect(out[0]).toEqual({ id: 'a', name: 'Alpha', color: undefined, order: 1, members: ['s1'] });
    });

    it('drops malformed entries instead of throwing', () => {
        const raw = [
            null,
            42,
            { id: 'ok', name: 'OK' },                // missing order/members -> defaulted
            { id: 'noName', order: 0 },              // no name -> dropped
            { name: 'noId', order: 0 },              // no id -> dropped
        ];
        const out = normalizeServerGroups(raw);
        expect(out).toHaveLength(1);
        expect(out[0].id).toBe('ok');
        expect(out[0].order).toBe(0);
        expect(out[0].members).toEqual([]);
    });

    it('filters non-string member ids', () => {
        const raw = [{ id: 'g', name: 'G', order: 0, members: ['a', 5, null, 'b'] }];
        const out = normalizeServerGroups(raw);
        expect(out[0].members).toEqual(['a', 'b']);
    });

    it('preserves a custom colour when present', () => {
        const raw: ServerGroup[] = [{ id: 'g', name: 'G', color: '#10b981', order: 0, members: [] }];
        expect(normalizeServerGroups(raw)[0].color).toBe('#10b981');
    });
});

describe('newServerGroupId', () => {
    it('produces a grp_-prefixed id', () => {
        expect(newServerGroupId()).toMatch(/^grp_\d+_[a-z0-9]+$/);
    });
});
