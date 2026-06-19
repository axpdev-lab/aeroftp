// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import {
    countCodingCheckpointRestoreActions,
    normalizeCodingCheckpointRestoreResult,
    summarizeCodingCheckpointRestoreResult,
} from './aiChatCodingCheckpointRestore';

describe('aiChatCodingCheckpointRestore', () => {
    it('normalizes and summarizes dry-run restore actions', () => {
        const result = normalizeCodingCheckpointRestoreResult({
            checkpoint_id: 'cag_abc',
            workspace_root: '/repo',
            dry_run: true,
            files: [
                {
                    path: 'src/app.ts',
                    action: 'restore',
                    existed_at_checkpoint: true,
                    size_bytes: 120,
                    sha256: 'abc123',
                },
                {
                    path: 'src/generated.ts',
                    action: 'delete',
                    existed_at_checkpoint: false,
                    size_bytes: 0,
                    sha256: null,
                },
                {
                    path: 'src/unchanged.ts',
                    action: 'noop',
                    existed_at_checkpoint: false,
                    size_bytes: 0,
                    sha256: null,
                },
            ],
        });

        expect(result).not.toBeNull();
        expect(countCodingCheckpointRestoreActions(result!)).toEqual({
            restore: 1,
            delete: 1,
            noop: 1,
        });
        expect(summarizeCodingCheckpointRestoreResult(result!)).toContain('Checkpoint restore dry run completed');
        expect(summarizeCodingCheckpointRestoreResult(result!)).toContain('Would rewrite 1, delete 1, skip 1');
    });

    it('summarizes applied restore results', () => {
        const result = normalizeCodingCheckpointRestoreResult({
            checkpoint_id: 'cag_done',
            workspace_root: '/repo',
            dry_run: false,
            files: [
                {
                    path: 'README.md',
                    action: 'restore',
                    existed_at_checkpoint: true,
                    size_bytes: 42,
                    sha256: 'def456',
                },
            ],
        });

        expect(result?.dry_run).toBe(false);
        expect(summarizeCodingCheckpointRestoreResult(result!)).toContain('Checkpoint restore completed');
        expect(summarizeCodingCheckpointRestoreResult(result!)).toContain('Rewrote 1, deleted 0, skipped 0');
    });
});
