// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from 'vitest';
import {
    buildAeroSyncTabStatePatch,
    overlayLivePlanOnTemplate,
    settingsFromAerosyncScript,
    settingsFromLegacyScript,
    settingsFromTemplate,
} from './syncTemplateApply';
import type { AerosyncImportScriptResult, SyncScriptMeta, SyncTemplate } from '../types';

describe('AeroSync template import application', () => {
    it('maps a canonical script into Plan and Sync controls', () => {
        const imported = {
            profile: {
                local_path: '/photos', remote_path: '/backup', dry_run: true,
                conflict_mode: 'rename', track_renames: false, skip_matching: false,
                resync: false, watch: false, connect_profile: 'Cloud', connect_url: null,
                profile: {
                    id: 'backup', name: 'Backup', builtin: false,
                    direction: 'local_to_remote', compare_timestamp: true,
                    compare_size: true, compare_checksum: false,
                    exclude_patterns: ['*.tmp'], verify_policy: 'full',
                    delete_orphans: true, parallel_streams: 7,
                    compression_mode: 'on',
                    retry_policy: { max_retries: 3, base_delay_ms: 1, max_delay_ms: 2, timeout_ms: 3, backoff_multiplier: 2 },
                },
            },
            unmapped_fields: [], warnings: [], canonical_path: '/x', resolved_from_wrapper: false,
        } as AerosyncImportScriptResult;
        const patch = buildAeroSyncTabStatePatch(settingsFromAerosyncScript(imported), 'remote-local');
        expect(patch).toMatchObject({
            'sync.source': '/photos',
            'sync.destination': '/backup',
            'sync.exclude': '*.tmp',
            'sync.dryRun': true,
            'plan.preset': 'mirror',
            'plan.direction': 'right-to-left',
            'plan.conflictPolicy': 'rename',
            'plan.verifyPolicy': 'full_checksum',
            'plan.parallelStreams': 7,
            'plan.compressionMode': 'on',
        });
    });

    it('maps an .aerosync template into live paths and comparison settings', () => {
        const template: SyncTemplate = {
            schema_version: 1,
            name: 'Pull',
            description: '',
            created_by: 'test',
            path_patterns: [{ local: '/local', remote: '/remote' }],
            profile: {
                direction: 'remote_to_local',
                compare_timestamp: false,
                compare_size: true,
                compare_checksum: false,
                delete_orphans: false,
                parallel_streams: 3,
                compression_mode: 'auto',
            },
            exclude_patterns: ['cache/**', '*.part'],
            schedule: null,
        };
        const imported = settingsFromTemplate(template);
        expect(imported.ok).toBe(true);
        if (!imported.ok) return;
        const patch = buildAeroSyncTabStatePatch(imported.settings, 'local-remote');
        expect(patch).toMatchObject({
            'sync.source': '/local',
            'sync.destination': '/remote',
            'sync.exclude': 'cache/**, *.part',
            'plan.preset': 'backup',
            'plan.direction': 'right-to-left',
            'plan.verifyPolicy': 'size_only',
            'plan.parallelStreams': 3,
            'plan.compressionMode': 'auto',
        });
    });

    it('keeps live verify, compression and canary on an exported Mirror template (#514)', () => {
        const template: SyncTemplate = {
            schema_version: 1,
            name: 'Mirror',
            description: '',
            created_by: 'test',
            path_patterns: [{ local: '/a', remote: '/b' }],
            profile: {
                direction: 'local_to_remote',
                compare_timestamp: true,
                compare_size: true,
                compare_checksum: false,
                delete_orphans: true,
                parallel_streams: 4,
                compression_mode: 'off',
            },
            exclude_patterns: [],
            schedule: null,
        };
        const overlaid = overlayLivePlanOnTemplate(template, {
            compressionMode: 'auto',
            verifyPolicy: 'full_checksum',
            canary: { percent: 15, selection: 'newest' },
        });
        expect(overlaid.profile.compression_mode).toBe('auto');
        expect(overlaid.profile.verify_policy).toBe('full');
        expect(overlaid.profile.canary).toEqual({ percent: 15, selection: 'newest' });
        const imported = settingsFromTemplate(overlaid);
        expect(imported.ok).toBe(true);
        if (!imported.ok) return;
        const patch = buildAeroSyncTabStatePatch(imported.settings, 'local-remote');
        expect(patch['plan.compressionMode']).toBe('auto');
        expect(patch['plan.verifyPolicy']).toBe('full_checksum');
        expect(patch['plan.canaryMode']).toBe(true);
        expect(patch['plan.canaryPercent']).toBe(15);
        expect(patch['plan.canarySelection']).toBe('newest');
    });

    it('maps a legacy wrapper import without inventing absent runtime knobs', () => {
        const legacy: SyncScriptMeta = {
            schema: 1,
            profile_id: 'legacy',
            profile_name: 'Legacy',
            local_path: '/from',
            remote_path: '/to',
            direction: 'bidirectional',
            delete_orphans: true,
            exclude_patterns: [],
            retries: null,
            retries_sleep: null,
        };
        const patch = buildAeroSyncTabStatePatch(settingsFromLegacyScript(legacy), 'local-local');
        expect(patch).toEqual({
            'sync.source': '/from',
            'sync.destination': '/to',
            'sync.exclude': '',
            'plan.preset': 'bisync',
            'plan.direction': 'left-to-right',
        });
    });
});

describe('a template that does not carry exactly one path pair (C-10)', () => {
    const template = (pairs: { local: string; remote: string }[]): SyncTemplate => ({
        schema_version: 1,
        name: 'Pull',
        description: '',
        created_by: 'test',
        path_patterns: pairs,
        profile: {
            direction: 'remote_to_local',
            compare_timestamp: false,
            compare_size: true,
            compare_checksum: false,
            delete_orphans: false,
            parallel_streams: 3,
            compression_mode: 'auto',
        },
        exclude_patterns: [],
        schedule: null,
    });

    it('refuses two pairs instead of applying the first and dropping the rest', () => {
        const imported = settingsFromTemplate(template([
            { local: '/a', remote: '/ra' },
            { local: '/b', remote: '/rb' },
        ]));
        expect(imported.ok).toBe(false);
        if (imported.ok) return;
        // The number is what the operator acts on, so it is part of the result
        // rather than only of the message.
        expect(imported.usablePairs).toBe(2);
    });

    it('refuses an empty list instead of applying empty paths', () => {
        // `path_patterns[0]` behind an optional chain produced two empty
        // strings, which read downstream as a sync from the filesystem root.
        const imported = settingsFromTemplate(template([]));
        expect(imported.ok).toBe(false);
        if (imported.ok) return;
        expect(imported.usablePairs).toBe(0);
    });

    it('counts a pair with a blank side as no pair at all', () => {
        for (const pair of [{ local: '', remote: '/r' }, { local: '/l', remote: '   ' }]) {
            const imported = settingsFromTemplate(template([pair]));
            expect(imported.ok, JSON.stringify(pair)).toBe(false);
            if (imported.ok) continue;
            expect(imported.usablePairs).toBe(0);
        }
    });

    it('applies the one pair it does carry', () => {
        const imported = settingsFromTemplate(template([{ local: '/l', remote: '/r' }]));
        expect(imported.ok).toBe(true);
        if (!imported.ok) return;
        expect(imported.settings.localPath).toBe('/l');
        expect(imported.settings.remotePath).toBe('/r');
    });
});
