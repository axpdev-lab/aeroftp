// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { setupBoxChevronClass, setupBoxDefaultOpen } from './CollapsibleSetupBox';

// #215 idea D flash fix: a bridge setup box must NOT open during the "Checking…"
// moment and then collapse on 🟢. It starts collapsed and only opens once the
// state settles to a non-green value.
describe('setupBoxDefaultOpen', () => {
    it('non-bridge box starts closed (owner 2026-09-24)', () => {
        expect(setupBoxDefaultOpen(false, undefined)).toBe(false);
        expect(setupBoxDefaultOpen(false, 'green')).toBe(false);
        expect(setupBoxDefaultOpen(undefined, undefined)).toBe(false);
    });

    it('bridge box stays collapsed while loading (undefined) — no flash', () => {
        expect(setupBoxDefaultOpen(true, undefined)).toBe(false);
    });

    it('bridge box collapses when active (🟢)', () => {
        expect(setupBoxDefaultOpen(true, 'green')).toBe(false);
    });

    it('bridge box expands only once it settles to a non-green state', () => {
        expect(setupBoxDefaultOpen(true, 'amber')).toBe(true);
        expect(setupBoxDefaultOpen(true, 'red')).toBe(true);
    });
});

describe('setupBoxChevronClass', () => {
    it('points down while closed and up while open', () => {
        expect(setupBoxChevronClass(false)).toBe('');
        expect(setupBoxChevronClass(true)).toBe('rotate-180');
    });
});
