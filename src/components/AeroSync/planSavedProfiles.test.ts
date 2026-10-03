// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later

import * as React from 'react';
import type { PresetPlan } from '../../utils/syncPresets';
import type { AeroSyncRuntime } from './types';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { PlanTabContent } from './PlanTabContent';
import { SyncTemplateDialog } from '../Sync/SyncTemplateDialog';
import { createTabStateStore, TabStateStoreContext } from './tabStateStore';
import { compareEntries } from '../../utils/compareEndpoints';
import { deleteSavedSyncProfile } from '../../utils/syncProfiles';
import { retryPolicyForSpeed } from '../../utils/remoteSyncRunner';
import type { SyncProfile } from '../../types';

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), pickFile: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('../../i18n', () => ({ useTranslation: () => (key: string, args?: Record<string, unknown>) => args ? `${key} ${Object.values(args).join(' ')}` : key }));
vi.mock('../../utils/pickPath', () => ({ pickFile: mocks.pickFile, pickSave: vi.fn() }));
vi.mock('@tauri-apps/plugin-fs', () => ({ readTextFile: vi.fn(), writeTextFile: vi.fn() }));
vi.mock('../../hooks/useDraggableModal', () => ({ useDraggableModal: () => ({ modalRef: { current: null }, style: {}, onMouseDown: vi.fn() }) }));

const profile = (patch: Partial<SyncProfile> = {}): SyncProfile => ({
    id: 'nightly', name: 'Nightly import', builtin: false, direction: 'remote_to_local',
    compare_timestamp: true, compare_size: true, compare_checksum: false,
    exclude_patterns: [], retry_policy: retryPolicyForSpeed('normal'), verify_policy: 'full',
    delete_orphans: false, parallel_streams: 0, compression_mode: 'auto', ...patch,
});
const result = compareEntries(
    [{ name: 'left.bin', isDir: false, size: 10 }],
    [{ name: 'right.bin', isDir: false, size: 20 }],
);
const roots: Root[] = [];
let host: HTMLDivElement;
let store: ReturnType<typeof createTabStateStore>;
let profiles: SyncProfile[];
let onExecute: ReturnType<typeof vi.fn<(plan: PresetPlan, runtime: AeroSyncRuntime) => void>>;
let onRescan: ReturnType<typeof vi.fn<(args: { userExcludes: string[]; backupDir: string }) => void>>;
const flush = async () => { await React.act(async () => { await Promise.resolve(); }); };
const mount = async (pairKind = 'local-remote', withStore = true) => {
    const root = createRoot(host); roots.push(root);
    await React.act(async () => root.render(React.createElement(TabStateStoreContext.Provider, { value: withStore ? store : null },
        React.createElement(PlanTabContent, { result, pairKind, canExecute: true, onExecute, onRescan,
            compareBackupDir: '.aeroftp-backup', cli: { localPath: '/local', remotePath: '/remote', profileName: 'Server' } }))));
    return root;
};
const chooser = () => host.querySelector<HTMLSelectElement>('select[aria-label="aerosync.savedPresets"]');
const choose = async (id = 'nightly') => {
    const select = chooser(); expect(select).not.toBeNull();
    await React.act(async () => { select!.value = id; select!.dispatchEvent(new Event('change', { bubbles: true })); });
};
const button = (key: string) => Array.from(host.querySelectorAll<HTMLButtonElement>('button')).find(b => b.textContent?.includes(key))!;
const click = async (key: string) => { const b = button(key); expect(b).toBeTruthy(); await React.act(async () => b.click()); };

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div'); document.body.append(host);
    store = createTabStateStore(); store.set('sync.source', '/local'); store.set('sync.destination', '/remote');
    profiles = [profile({ id: 'stock', name: 'Backend built-in', builtin: true }), profile()];
    onExecute = vi.fn(); onRescan = vi.fn(); mocks.invoke.mockReset();
    mocks.invoke.mockImplementation(async (cmd: string) => {
        if (cmd === 'load_sync_profiles_cmd') return profiles;
        if (cmd === 'sync_backup_validate') return { status: 'valid', dir: '.aeroftp-backup', ancestors: [] };
        throw new Error(`Unexpected IPC ${cmd}`);
    });
});
afterEach(async () => {
    await React.act(async () => roots.splice(0).forEach(root => root.unmount()));
    host.remove(); vi.restoreAllMocks();
});

describe('saved profiles in the real Plan tab', () => {
    it('loads custom profiles after the four chips without duplicating backend built-ins', async () => {
        await mount();
        expect(mocks.invoke).toHaveBeenCalledWith('load_sync_profiles_cmd');
        expect(chooser()?.textContent).toContain('Nightly import');
        expect(host.textContent).not.toContain('Backend built-in');
        const chip = button('Backup');
        expect(chip).toBeTruthy();
        expect(chip.compareDocumentPosition(chooser()!)).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
    });
    it.each(['local-remote', 'remote-local', 'local-local'])('applies pull settings and runtime with %s orientation, preserving endpoints', async pairKind => {
        await mount(pairKind); await choose();
        expect(store.get('plan.preset', '')).toBe('backup');
        expect(store.get('plan.direction', '')).toBe(pairKind === 'remote-local' ? 'left-to-right' : 'right-to-left');
        expect(store.get('plan.verifyPolicy', '')).toBe('full_checksum');
        expect(store.get('sync.source', '')).toBe('/local'); expect(store.get('sync.destination', '')).toBe('/remote');
        expect(host.textContent).toContain('aerosync.savedPresetLimits');
        expect(onExecute).not.toHaveBeenCalled();
        await click('aerosync.execute');
        expect(onExecute).toHaveBeenCalledTimes(1);
        const [plan, runtime] = onExecute.mock.calls[0];
        expect(plan.bucketPlans.find((b: { bucket: string }) => b.bucket === (pairKind === 'remote-local' ? 'only-left' : 'only-right'))?.action)
            .toBe(pairKind === 'remote-local' ? 'copy-to-right' : 'copy-to-left');
        expect(runtime.verifyPolicy).toBe('full_checksum');
    });
    it('keeps built-ins usable when empty or unreadable, with an explicit load error', async () => {
        profiles = []; await mount(); expect(chooser()?.options.length).toBe(1);
        mocks.invoke.mockImplementation(async (cmd: string) => { if (cmd === 'load_sync_profiles_cmd') throw new Error('disk denied'); return { status: 'valid', dir: '.aeroftp-backup', ancestors: [] }; });
        await React.act(async () => window.dispatchEvent(new Event('aeroftp-sync-profiles-changed')));
        expect(host.textContent).toContain('disk denied'); expect(host.textContent).toContain('aerosync.savedPresetsLoadFailed');
        expect(button('Backup').disabled).toBe(false);
    });
    it('shows limitations instead of applying saved compare, retry, stream or compression overrides', async () => {
        profiles = [profile({ compare_checksum: true, compare_size: false, compare_timestamp: false,
            parallel_streams: 16, compression_mode: 'on', retry_policy: { ...retryPolicyForSpeed('normal'), max_retries: 99 } })];
        await mount(); await choose(); await click('aerosync.execute');
        expect(host.textContent).toContain('aerosync.savedPresetLimits');
        expect(onExecute.mock.calls[0][1].retryPolicy).toEqual(retryPolicyForSpeed('normal'));
        expect(onExecute.mock.calls[0][1]).not.toHaveProperty('parallelStreams');
        expect(onExecute.mock.calls[0][1]).not.toHaveProperty('compressionMode');
        expect(result.appliedOptions.policy).toBe('size-and-mtime');
    });
    it.each([true, false])('notifies mounted lists only after a successful deletion (success=%s)', async success => {
        await mount(); await choose();
        mocks.invoke.mockImplementation(async (cmd: string) => {
            if (cmd === 'delete_sync_profile_cmd') { if (!success) throw new Error('delete refused'); profiles = []; return; }
            if (cmd === 'load_sync_profiles_cmd') return profiles;
            throw new Error(`Unexpected IPC ${cmd}`);
        });
        const listener = vi.fn(); window.addEventListener('aeroftp-sync-profiles-changed', listener);
        await React.act(async () => {
            const deleted = deleteSavedSyncProfile(mocks.invoke, profile(), profiles);
            if (success) await deleted; else await expect(deleted).rejects.toThrow('delete refused');
        });
        expect(listener).toHaveBeenCalledTimes(success ? 1 : 0);
        expect(chooser()?.textContent?.includes('Nightly import')).toBe(!success);
        if (success) expect(chooser()?.value).toBe('');
        window.removeEventListener('aeroftp-sync-profiles-changed', listener);
    });
    it('requires a rescan for saved exclusions and resets destructive confirmation even for identical modes', async () => {
        profiles = [profile({ id: 'one', delete_orphans: true }), profile({ id: 'two', delete_orphans: true }), profile({ id: 'excluded', exclude_patterns: ['*.tmp'] })];
        await mount(); await choose('one');
        const confirmation = Array.from(host.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')).find(c => c.parentElement?.textContent?.includes('aerosync.confirmDestructive'));
        expect(confirmation).toBeTruthy();
        await React.act(async () => confirmation!.click());
        expect(button('aerosync.execute').disabled).toBe(false);
        await choose('two'); expect(button('aerosync.execute').disabled).toBe(true);
        await choose('excluded'); expect(store.get('sync.exclude', '')).toBe('*.tmp');
        expect(button('aerosync.execute').disabled).toBe(true);
        await click('aerosync.rescan'); expect(onRescan).toHaveBeenCalledWith({ userExcludes: ['*.tmp'], backupDir: '.aeroftp-backup' });
        expect(onExecute).not.toHaveBeenCalled();
    });
    it('restores the selected id and manual overrides on remount, then clears it for a built-in', async () => {
        const root = await mount(); await choose(); store.set('plan.verifyPolicy', 'none');
        await flush(); await React.act(async () => root.render(null));
        await React.act(async () => root.render(React.createElement(TabStateStoreContext.Provider, { value: store }, React.createElement(PlanTabContent, { result, canExecute: true, onExecute }))));
        expect(chooser()?.value).toBe('nightly'); expect(store.get('plan.verifyPolicy', '')).toBe('none');
        await click('Mirror'); expect(chooser()?.value).toBe('');
    });
    it('applies saved settings when rendered without a dialog store', async () => {
        await mount('local-remote', false); await choose(); await click('aerosync.execute');
        expect(onExecute.mock.calls[0]?.[1].verifyPolicy).toBe('full_checksum');
    });
    it('reloads changes, discards out-of-order results and removes a deleted selection', async () => {
        await mount(); await choose();
        let resolveOld!: (value: SyncProfile[]) => void;
        mocks.invoke.mockImplementationOnce(() => new Promise(resolve => { resolveOld = resolve; }));
        await React.act(async () => window.dispatchEvent(new Event('aeroftp-sync-profiles-changed')));
        profiles = [profile({ id: 'fresh', name: 'Fresh preset' })];
        await React.act(async () => window.dispatchEvent(new Event('aeroftp-sync-profiles-changed')));
        await React.act(async () => resolveOld([profile({ id: 'stale', name: 'Stale preset' })]));
        expect(chooser()?.textContent).toContain('Fresh preset'); expect(chooser()?.textContent).not.toContain('Stale preset');
        expect(chooser()?.value).toBe(''); expect(store.get('plan.selectedSavedProfileId', '')).toBe('');
    });
    it('ignores a late load after unmount and unregisters reload events', async () => {
        let resolve!: (value: SyncProfile[]) => void;
        mocks.invoke.mockImplementationOnce(() => new Promise(r => { resolve = r; }));
        const root = await mount(); await React.act(async () => root.render(null));
        const count = mocks.invoke.mock.calls.length;
        await React.act(async () => { resolve(profiles); window.dispatchEvent(new Event('aeroftp-sync-profiles-changed')); });
        expect(host.textContent).toBe(''); expect(mocks.invoke.mock.calls.length).toBe(count);
        expect(store.has('plan.selectedSavedProfileId')).toBe(false);
    });
});

describe('actual script Save as preset notification', () => {
    it.each([true, false])('dispatches a profile reload only after a successful save (success=%s)', async success => {
        const listener = vi.fn(); window.addEventListener('aeroftp-sync-profiles-changed', listener);
        const imported = { profile: { profile: profile(), local_path: '/import/local', remote_path: '/import/remote' }, warnings: [], unmapped_fields: [], resolved_from_wrapper: false };
        mocks.pickFile.mockResolvedValue('/tmp/test.aeroftp-script');
        mocks.invoke.mockImplementation(async (cmd: string) => {
            if (cmd === 'load_sync_profiles_cmd') return [];
            if (cmd === 'aerosync_import_script_cmd') return imported;
            if (cmd === 'save_sync_profile_cmd') { expect(listener).not.toHaveBeenCalled(); if (!success) throw new Error('save refused'); return; }
            throw new Error(`Unexpected IPC ${cmd}`);
        });
        const root = createRoot(host); roots.push(root);
        await React.act(async () => root.render(React.createElement(SyncTemplateDialog, { isOpen: true, onClose: vi.fn(), localPath: '/local', remotePath: '/remote', serverProfileName: 'Server', excludePatterns: [], onApplyImport: vi.fn() })));
        await click('syncPanel.templateImport');
        await React.act(async () => Array.from(host.querySelectorAll<HTMLButtonElement>('button')).filter(b => b.textContent?.includes('syncPanel.templateImport')).slice(-1)[0].click());
        await click('syncPanel.aerosyncScriptApplyButton');
        expect(mocks.invoke).toHaveBeenCalledWith('save_sync_profile_cmd', { profile: expect.objectContaining({ builtin: false }) });
        expect(listener).toHaveBeenCalledTimes(success ? 1 : 0);
        if (!success) expect(host.textContent).toContain('save refused');
        window.removeEventListener('aeroftp-sync-profiles-changed', listener);
    });
});
