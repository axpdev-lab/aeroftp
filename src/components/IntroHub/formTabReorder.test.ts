// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { reorderInheritingIndex } from '../../utils/reorderByIndex';
import { applyFormTabReorder } from './formTabReorder';
import headerRaw from './IntroHubHeader.tsx?raw';
import hubRaw from './IntroHub.tsx?raw';

describe('applyFormTabReorder', () => {
    it('keeps connectionParams on the same tab id after a 0<->1 swap', () => {
        const current = [
            { id: 'ftp', connectionParams: { server: 'ftp.example' }, extra: 'keep-a' },
            { id: 's3', connectionParams: { server: 's3.example' }, extra: 'keep-b' },
        ];
        // Header FormTab values have ids (and labels) but not connectionParams.
        const headerView = current.map(({ id }) => ({ id }));
        const reordered = reorderInheritingIndex(headerView, 0, 1);
        const next = applyFormTabReorder(current, reordered);

        expect(next.map((t) => t.id)).toEqual(['s3', 'ftp']);
        expect(next[0].connectionParams).toBe(current[1].connectionParams);
        expect(next[0].extra).toBe('keep-b');
        expect(next[1].connectionParams).toBe(current[0].connectionParams);
        expect(next[1].extra).toBe('keep-a');
    });
});

describe('IntroHubHeader form-tab drag (#347)', () => {
    it('wires HTML5 DnD through reorderInheritingIndex when more than one tab is open', () => {
        expect(headerRaw).toContain('draggable={canReorder}');
        expect(headerRaw).toContain('reorderInheritingIndex');
        expect(headerRaw).toContain('application/x-introhub-form-tab');
    });

    it('maps the header order back through applyFormTabReorder', () => {
        expect(hubRaw).toContain('applyFormTabReorder');
        expect(hubRaw).toContain('onReorderFormTabs={handleReorderFormTabs}');
    });
});
