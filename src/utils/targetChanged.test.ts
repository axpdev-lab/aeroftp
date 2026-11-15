// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import TABLE from '../shared/target-changed-cases.json';
import { isTargetChangedRefusal, TARGET_CHANGED_MARKER } from './targetChanged';

describe('a changed-target refusal', () => {
    it('is read as the shared table says, the same as the backend reads it', () => {
        expect(TABLE.cases.length).toBeGreaterThan(20);
        for (const row of TABLE.cases as { text: string; refusal: boolean }[]) {
            expect(isTargetChangedRefusal(row.text), row.text).toBe(row.refusal);
        }
    });

    it('is found in a thrown Error and in a rejected string alike', () => {
        const refusal = `${TARGET_CHANGED_MARKER} the connection changed since the permissions were read; nothing was changed`;
        expect(isTargetChangedRefusal(refusal)).toBe(true);
        expect(isTargetChangedRefusal(new Error(refusal))).toBe(true);
        expect(isTargetChangedRefusal(new Error(`chmod failed: ${refusal}`))).toBe(true);
        expect(isTargetChangedRefusal(undefined)).toBe(false);
        expect(isTargetChangedRefusal(new Error(`Permission denied on /srv/${TARGET_CHANGED_MARKER} notes`))).toBe(false);
    });
});
