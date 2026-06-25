// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { normalizeCodingPatchResult, summarizeCodingPatchResult } from './aiChatCodingPatch';

describe('aiChatCodingPatch', () => {
    it('normalizes and summarizes successful apply results with checkpoints', () => {
        const result = normalizeCodingPatchResult({
            success: true,
            dry_run: false,
            checkpoint_id: 'cag_123',
            files: [
                {
                    path: 'src/app.ts',
                    status: 'modified',
                    hunks: 2,
                    additions: 4,
                    deletions: 1,
                    old_size_bytes: 100,
                    new_size_bytes: 120,
                },
            ],
            diagnostics: [],
            warnings: [],
        });

        expect(result).not.toBeNull();
        expect(result?.checkpoint_id).toBe('cag_123');
        expect(summarizeCodingPatchResult(result!)).toContain('Patch apply completed');
        expect(summarizeCodingPatchResult(result!)).toContain('Checkpoint: `cag_123`');
    });

    it('keeps conflict diagnostics for dry-run failures', () => {
        const result = normalizeCodingPatchResult({
            success: false,
            dry_run: true,
            checkpoint_id: null,
            files: [],
            diagnostics: [
                {
                    path: 'src/app.ts',
                    hunk_index: 0,
                    message: 'Patch context did not match',
                    expected: 'old line',
                    actual: 'new line',
                },
            ],
            warnings: [],
        });

        expect(result?.diagnostics).toHaveLength(1);
        expect(result?.diagnostics[0].expected).toBe('old line');
        expect(summarizeCodingPatchResult(result!)).toContain('Patch dry run found conflicts');
        expect(summarizeCodingPatchResult(result!)).toContain('Diagnostics: 1');
    });
});
