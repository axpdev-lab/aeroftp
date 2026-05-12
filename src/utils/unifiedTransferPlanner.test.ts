// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { createLocalEndpoint, createRemoteEndpoint } from './panelEndpoints';
import { createUnifiedTransferPlan, selectUnifiedTransferEngine } from './unifiedTransferPlanner';
import type { PanelEndpoint } from '../types/aerofile';
import type { ServerProfile } from '../types';

const remote = (id: string, protocol: ServerProfile['protocol']): PanelEndpoint =>
    createRemoteEndpoint({
        id,
        name: id,
        protocol,
    }, '/');

describe('unified transfer planner', () => {
    it('selects the expected engine for every v1 pair kind', () => {
        const local = createLocalEndpoint('local', '/src');
        const local2 = createLocalEndpoint('local2', '/dst');
        const sftp = remote('prod', 'sftp');
        const drive = remote('drive', 'googledrive');

        expect(selectUnifiedTransferEngine('local-local', 'copy', local, local2)).toBe('local-fs');
        expect(selectUnifiedTransferEngine('local-local', 'backup', local, local2)).toBe('local-delta');
        expect(selectUnifiedTransferEngine('local-remote', 'copy', local, sftp)).toBe('provider-upload');
        expect(selectUnifiedTransferEngine('local-remote', 'sync', local, sftp)).toBe('aerosync');
        expect(selectUnifiedTransferEngine('remote-local', 'sync', sftp, local)).toBe('aerosync');
        expect(selectUnifiedTransferEngine('remote-local', 'copy', drive, local)).toBe('provider-download');
        expect(selectUnifiedTransferEngine('remote-remote', 'copy', sftp, drive)).toBe('cross-profile-transfer');
        expect(selectUnifiedTransferEngine('remote-remote', 'backup', sftp, drive)).toBe('cross-profile-sync');
    });

    it('requires preview for sync-style and destructive plans', () => {
        const source = createLocalEndpoint('local', '/src');
        const destination = createLocalEndpoint('local2', '/dst');
        const backup = createUnifiedTransferPlan({ mode: 'backup', source, destination, entryCount: 2 });
        const move = createUnifiedTransferPlan({ mode: 'move', source, destination, entryCount: 1 });

        expect(backup.engine).toBe('local-delta');
        expect(backup.requiresPreview).toBe(true);
        expect(backup.destructive).toBe(false);
        expect(backup.maxParallel).toBe(4);
        expect(move.requiresPreview).toBe(true);
        expect(move.destructive).toBe(true);
        expect(move.warnings).toContain('destructive-plan-requires-confirmation');
    });

    it('rejects same-endpoint plans before execution', () => {
        const source = createLocalEndpoint('local', '/same');
        const plan = createUnifiedTransferPlan({ mode: 'copy', source, destination: source });

        expect(plan.canExecute).toBe(false);
        expect(plan.engine).toBe('unsupported');
        expect(plan.warnings).toContain('unsupported-endpoint-pair');
    });
});
