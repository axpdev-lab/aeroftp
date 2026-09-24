// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { aeroSyncTabsFor } from './types';

describe('aeroSyncTabsFor', () => {
    // The Sync tab runs the local-to-local engine. With a remote on one side it
    // would copy into a local folder named after the remote path.
    it('does not offer the local-only Sync tab when a remote is on either side', () => {
        expect(aeroSyncTabsFor('local-remote')).toEqual(['compare', 'plan']);
        expect(aeroSyncTabsFor('remote-local')).toEqual(['compare', 'plan']);
    });

    it('offers the Sync tab for two local folders and without a panel context', () => {
        expect(aeroSyncTabsFor('local-local')).toEqual(['compare', 'plan', 'sync']);
        expect(aeroSyncTabsFor(null)).toEqual(['compare', 'plan', 'sync']);
    });
});
