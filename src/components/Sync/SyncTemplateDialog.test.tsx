// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later

import * as React from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { SyncTemplateDialog } from './SyncTemplateDialog';
import { createTabStateStore, TabStateStoreContext } from '../AeroSync/tabStateStore';
import type { SyncProfile } from '../../types';

const mocks = vi.hoisted(() => ({
    invoke: vi.fn(),
    pickSave: vi.fn(),
    pickFile: vi.fn(),
    writeTextFile: vi.fn(),
    readTextFile: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('@tauri-apps/plugin-fs', () => ({
    writeTextFile: mocks.writeTextFile,
    readTextFile: mocks.readTextFile,
}));
vi.mock('../../utils/pickPath', () => ({ pickSave: mocks.pickSave, pickFile: mocks.pickFile }));
vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));

const mirror: SyncProfile = {
    id: 'mirror', name: 'Mirror', builtin: true, direction: 'local_to_remote',
    compare_timestamp: true, compare_size: true, compare_checksum: false,
    exclude_patterns: [], retry_policy: {
        max_retries: 3, base_delay_ms: 100, max_delay_ms: 1000, timeout_ms: 10000, backoff_multiplier: 2,
    },
    verify_policy: 'size_only', delete_orphans: true, parallel_streams: 1, compression_mode: 'off',
};

const backendTemplate = () => JSON.stringify({
    schema_version: 1,
    name: 'Update template',
    description: '',
    created_by: 'AeroFTP v4.2.4',
    path_patterns: [{ local: '/local', remote: '/remote' }],
    profile: {
        direction: 'local_to_remote', compare_timestamp: true, compare_size: true,
        compare_checksum: false, delete_orphans: true, parallel_streams: 1,
        compression_mode: 'off', verify_policy: 'size_only',
    },
    exclude_patterns: ['cache/**'],
    schedule: null,
});

let root: Root;
let host: HTMLDivElement;

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
    mocks.invoke.mockReset().mockImplementation(async (command: string) => {
        if (command === 'load_sync_profiles_cmd') return [mirror];
        if (command === 'export_sync_template_cmd') return backendTemplate();
        return null;
    });
    mocks.pickSave.mockReset().mockResolvedValue('/tmp/update.aerosync');
    mocks.pickFile.mockReset();
    mocks.writeTextFile.mockReset().mockResolvedValue(undefined);
    mocks.readTextFile.mockReset();
});

afterEach(async () => {
    await React.act(async () => root.unmount());
    host.remove();
    vi.restoreAllMocks();
});

it('exports every live Plan option in an Update .aerosync template', async () => {
    const store = createTabStateStore();
    store.set('plan.preset', 'update');
    store.set('plan.direction', 'right-to-left');
    store.set('plan.conflictPolicy', 'newer-wins');
    store.set('plan.versionedBackup', { enabled: true, backupDir: 'history/versions' });
    store.set('plan.speedMode', 'turbo');
    store.set('plan.verifyPolicy', 'full_checksum');
    store.set('plan.canaryMode', true);
    store.set('plan.canaryPercent', 25);
    store.set('plan.canarySelection', 'largest');
    store.set('plan.transferBudgetMb', 64);
    store.set('plan.ecEnabled', true);
    store.set('plan.ecPct', 25);
    store.set('sync.exclude', 'cache/**');

    await React.act(async () => root.render(React.createElement(
        TabStateStoreContext.Provider,
        { value: store },
        React.createElement(SyncTemplateDialog, {
            isOpen: true,
            onClose: () => undefined,
            localPath: '/local',
            remotePath: '/remote',
            serverProfileName: 'Cloud',
            excludePatterns: [],
            pairKind: 'local-remote',
            onApplyImport: () => undefined,
        }),
    )));
    const format = Array.from(host.querySelectorAll('button'))
        .find(button => button.textContent === 'syncPanel.templateFormatAerosync');
    expect(format).toBeTruthy();
    await React.act(async () => format!.click());
    const exportButtons = Array.from(host.querySelectorAll('button'))
        .filter(button => button.textContent === 'syncPanel.templateExport');
    await React.act(async () => exportButtons[exportButtons.length - 1]!.click());

    expect(mocks.writeTextFile).toHaveBeenCalledOnce();
    const written = mocks.writeTextFile.mock.calls[0][1] as string;
    const template = JSON.parse(written);
    expect(template.profile).toMatchObject({
        mode: 'update',
        direction: 'remote_to_local',
        delete_orphans: false,
        conflict_mode: 'newer',
        versioned_backup: { enabled: true, backup_dir: 'history/versions' },
        speed_mode: 'turbo',
        verify_policy: 'full',
        canary: { percent: 25, selection: 'largest' },
        transfer_budget_mb: 64,
        error_correction: { enabled: true, pct: 25 },
    });
    expect(template.exclude_patterns).toEqual(['cache/**']);
});
