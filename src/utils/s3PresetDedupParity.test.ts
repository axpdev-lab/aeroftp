// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import dedupTs from './storageDedup.ts?raw';
import dedupRs from '../../src-tauri/src/storage_dedup.rs?raw';

// The dedup key must be the same in the GUI and in the Rust backend (CLI, MCP):
// one drive per account. A preset added to only one of the two lists splits
// quota aggregation between them, and no other test notices.
function tsPresetIds(): string[] {
    const body = /const S3_PRESET_IDS = new Set<string>\(\[([\s\S]*?)\]\);/.exec(dedupTs)?.[1] ?? '';
    return [...body.matchAll(/'([^']+)'/g)].map(m => m[1]).sort();
}

function rustPresetIds(): string[] {
    const fn = /pub fn is_s3_preset[\s\S]*?\n\}/.exec(dedupRs)?.[0] ?? '';
    return [...fn.matchAll(/"([^"]+)"/g)].map(m => m[1]).sort();
}

describe('S3 dedup preset lists', () => {
    it('are the same set in TypeScript and Rust', () => {
        expect(tsPresetIds().length).toBeGreaterThan(10);
        expect(tsPresetIds()).toEqual(rustPresetIds());
    });

    it('include tigris', () => {
        expect(tsPresetIds()).toContain('tigris');
    });
});
