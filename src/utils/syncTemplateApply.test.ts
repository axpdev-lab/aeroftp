// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from 'vitest';
import {
    buildAeroSyncTabStatePatch,
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
        const patch = buildAeroSyncTabStatePatch(settingsFromTemplate(template), 'local-remote');
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
