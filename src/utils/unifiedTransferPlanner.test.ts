// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { createLocalEndpoint, createOverlayEndpoint, createRemoteEndpoint } from './panelEndpoints';
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

    it('routes every overlay pair kind to the aerovault-overlay engine (Z.3.6)', () => {
        const local = createLocalEndpoint('local', '/src');
        const sftp = remote('prod', 'sftp');
        const overlay = createOverlayEndpoint({
            sessionId: 'avol_test',
            vaultPath: '/tmp/x.aerovault',
            source: 'local',
            path: '/docs',
        });

        expect(selectUnifiedTransferEngine('overlay-local', 'copy', overlay, local)).toBe('aerovault-overlay');
        expect(selectUnifiedTransferEngine('local-overlay', 'copy', local, overlay)).toBe('aerovault-overlay');
        expect(selectUnifiedTransferEngine('overlay-overlay', 'copy', overlay, overlay)).toBe('aerovault-overlay');
        expect(selectUnifiedTransferEngine('overlay-remote', 'copy', overlay, sftp)).toBe('aerovault-overlay');
        expect(selectUnifiedTransferEngine('remote-overlay', 'copy', sftp, overlay)).toBe('aerovault-overlay');
    });

    it('emits the busy-lock-ready warning for overlay plans (Z.3.6)', () => {
        const local = createLocalEndpoint('local', '/src');
        const overlay = createOverlayEndpoint({
            sessionId: 'avol_busy',
            vaultPath: '/tmp/lock.aerovault',
            source: 'local',
        });
        const plan = createUnifiedTransferPlan({ mode: 'copy', source: overlay, destination: local });

        expect(plan.engine).toBe('aerovault-overlay');
        expect(plan.warnings).toContain('aerovault-overlay-busy-lock-ready');
        // The "required" placeholder used before Z.3.6 should be gone.
        expect(plan.warnings).not.toContain('aerovault-overlay-busy-lock-required');
    });
});
