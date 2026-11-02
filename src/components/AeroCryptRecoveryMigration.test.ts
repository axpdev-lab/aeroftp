// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AeroCryptUnlock } from './AeroCryptUnlock';

const { invoke, readProfiles, pickSave, writeTextFile } = vi.hoisted(() => ({
    invoke: vi.fn(), readProfiles: vi.fn(), pickSave: vi.fn(), writeTextFile: vi.fn(),
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('../utils/serverProfileStore', () => ({ readSavedServerProfilesForWrite: readProfiles }));
vi.mock('../utils/pickPath', () => ({ pickFile: vi.fn(), pickSave }));
vi.mock('@tauri-apps/plugin-fs', () => ({ writeTextFile }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('qrcode.react', () => ({ QRCodeSVG: () => null }));

let root: Root;
let host: HTMLDivElement;
let migrated: boolean;
const oldKit = { vault_id: 'old-vault', version: 3, text: 'OLD RECOVERY KIT' };
const freshKit = { vault_id: 'new-vault', version: 3, text: 'FRESH RECOVERY KIT' };
const button = (key: string) => {
    const found = Array.from(host.querySelectorAll('button')).find((b) => b.textContent?.trim() === key);
    if (!found) throw new Error(`Missing button ${key}`);
    return found;
};
const click = async (key: string) => { await act(async () => button(key).click()); };
const mount = async (active = true) => {
    await act(async () => root.render(createElement(AeroCryptUnlock, {
        onClose: () => {}, onUnlocked: async () => {}, activeVaultId: active ? 'active' : undefined, profileId: 'profile', scope: '/vault',
    })));
};

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    vi.resetAllMocks();
    migrated = false;
    readProfiles.mockResolvedValue({ profiles: [], userId: 7 });
    invoke.mockImplementation(async (command: string, args?: { account?: string; configJson?: string }) => {
        switch (command) {
            case 'aerocrypt_provider_marker_status': return { hasLegacyMarker: !migrated, hasCurrentMarker: migrated };
            case 'aerocrypt_provider_read_config': return migrated ? 'NEW CONFIG' : 'OLD CONFIG';
            case 'aerocrypt_profile_recovery_kit': return oldKit;
            case 'get_credential': return args?.account?.includes('_pw_') ? 'secret' : '';
            case 'aerocrypt_provider_migrate_legacy_marker': migrated = true; return { changed: true, legacyDeleted: true };
            case 'store_overlay_metadata': throw new Error('cache write refused');
            case 'aerocrypt_build_emergency_kit':
                expect(args?.configJson).toBe('NEW CONFIG');
                return freshKit;
            default: return undefined;
        }
    });
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

describe('recovery kit after remote marker migration', () => {
    it.each(['cache write', 'account read'])('hides the old kit on %s failure and rebuilds only from the live config', async (failure) => {
        if (failure === 'account read') readProfiles.mockRejectedValue(new Error('account unavailable'));
        await mount();
        expect(host.textContent).toContain(oldKit.text);
        const oldCacheReads = invoke.mock.calls.filter(([c]) => c === 'aerocrypt_profile_recovery_kit').length;
        await click('aerocryptNative.migrateLegacyMarker');
        expect(host.textContent).toContain('aerocryptNative.overlayMetadataNotSaved');
        expect(host.textContent).not.toContain(oldKit.text);
        expect(host.textContent).not.toContain('aerocryptNative.saveKit');
        await click('aerocryptNative.showKit');
        expect(host.textContent).toContain(freshKit.text);
        expect(invoke.mock.calls.filter(([c]) => c === 'aerocrypt_profile_recovery_kit')).toHaveLength(oldCacheReads);
        expect(writeTextFile).not.toHaveBeenCalled();
    });

    it('blocks migration while the save picker is open, before React can disable the buttons', async () => {
        let finishPicker!: (path: string) => void;
        pickSave.mockReturnValue(new Promise<string>((resolve) => { finishPicker = resolve; }));
        await mount();
        await act(async () => {
            button('aerocryptNative.saveKit').click();
            button('aerocryptNative.migrateLegacyMarker').click();
        });
        expect(invoke.mock.calls.filter(([c]) => c === 'aerocrypt_provider_migrate_legacy_marker')).toHaveLength(0);
        expect(button('aerocryptNative.migrateLegacyMarker').disabled).toBe(true);
        await act(async () => finishPicker('/tmp/recovery.txt'));
        expect(writeTextFile).toHaveBeenCalledWith('/tmp/recovery.txt', oldKit.text);
        await click('aerocryptNative.migrateLegacyMarker');
        expect(invoke.mock.calls.filter(([c]) => c === 'aerocrypt_provider_migrate_legacy_marker')).toHaveLength(1);
        expect(host.textContent).not.toContain(oldKit.text);
    });

    it('blocks migration until an already-started kit write finishes', async () => {
        let finishWrite!: () => void;
        pickSave.mockResolvedValue('/tmp/recovery.txt');
        writeTextFile.mockReturnValue(new Promise<void>((resolve) => { finishWrite = resolve; }));
        await mount();
        await click('aerocryptNative.saveKit');
        expect(writeTextFile).toHaveBeenCalledWith('/tmp/recovery.txt', oldKit.text);
        await click('aerocryptNative.migrateLegacyMarker');
        expect(invoke.mock.calls.filter(([c]) => c === 'aerocrypt_provider_migrate_legacy_marker')).toHaveLength(0);
        await act(async () => finishWrite());
        await click('aerocryptNative.migrateLegacyMarker');
        expect(invoke.mock.calls.filter(([c]) => c === 'aerocrypt_provider_migrate_legacy_marker')).toHaveLength(1);
    });

    it('holds the shared guard before the migration credential read starts', async () => {
        let finishPassword!: (password: string) => void;
        const original = invoke.getMockImplementation()!;
        invoke.mockImplementation((command, args) => (
            command === 'get_credential' && args?.account?.includes('_pw_')
                ? new Promise<string>((resolve) => { finishPassword = resolve; })
                : original(command, args)
        ));
        await mount();
        await act(async () => {
            button('aerocryptNative.migrateLegacyMarker').click();
            button('aerocryptNative.saveKit').click();
        });
        expect(pickSave).not.toHaveBeenCalled();
        expect(button('aerocryptNative.saveKit').disabled).toBe(true);
        await act(async () => finishPassword('secret'));
        expect(invoke.mock.calls.filter(([c]) => c === 'aerocrypt_provider_migrate_legacy_marker')).toHaveLength(1);
        expect(writeTextFile).not.toHaveBeenCalled();
    });

    it('does not let a pending hydration restore the old kit after migration', async () => {
        let finishHydration!: (kit: typeof oldKit) => void;
        const original = invoke.getMockImplementation()!;
        invoke.mockImplementation((command: string, args?: { account?: string; configJson?: string }) => (
            command === 'aerocrypt_profile_recovery_kit'
                ? new Promise<typeof oldKit>((resolve) => { finishHydration = resolve; })
                : original(command, args)
        ));
        await mount();
        await click('aerocryptNative.migrateLegacyMarker');
        await act(async () => finishHydration(oldKit));
        expect(host.textContent).not.toContain(oldKit.text);
        await click('aerocryptNative.showKit');
        expect(host.textContent).toContain(freshKit.text);
    });

    it.each(['kit', 'missing', 'error'])('ignores a pending Show %s completion after migration publishes', async (outcome) => {
        const original = invoke.getMockImplementation()!;
        // An offline initial config leaves a cached kit but no vaultInfo fallback.
        invoke.mockImplementation((command, args) => command === 'aerocrypt_provider_read_config'
            ? Promise.resolve(null) : original(command, args));
        await mount();
        await click('aerocryptNative.hideKit');
        let resolveShow!: (value: unknown) => void;
        let rejectShow!: (reason: unknown) => void;
        const pending = new Promise((resolve, reject) => { resolveShow = resolve; rejectShow = reject; });
        invoke.mockImplementation((command, args) => {
            if (command === 'aerocrypt_profile_recovery_kit') {
                return outcome === 'kit' ? pending : Promise.reject(new Error('cache unavailable'));
            }
            if (command === 'aerocrypt_provider_read_config' && !migrated) return pending;
            return original(command, args);
        });
        await click('aerocryptNative.showKit');
        expect(invoke.mock.calls.some(([c]) => c === (outcome === 'kit' ? 'aerocrypt_profile_recovery_kit' : 'aerocrypt_provider_read_config'))).toBe(true);
        await click('aerocryptNative.migrateLegacyMarker');
        expect(host.textContent).toContain('aerocryptNative.overlayMetadataNotSaved');
        await act(async () => { if (outcome === 'error') rejectShow(new Error('stale Show error')); else resolveShow(outcome === 'kit' ? oldKit : null); });
        expect(host.textContent).not.toContain(oldKit.text);
        expect(host.textContent).toContain('aerocryptNative.overlayMetadataNotSaved');
        expect(host.textContent).not.toContain('aerocryptNative.kitUnavailable');
        expect(host.textContent).not.toContain('stale Show error');
    });

    it('disables Show while marker migration is pending', async () => {
        let finishMigration!: (value: unknown) => void;
        const original = invoke.getMockImplementation()!;
        invoke.mockImplementation((command, args) => command === 'aerocrypt_provider_migrate_legacy_marker'
            ? new Promise((resolve) => { finishMigration = resolve; }) : original(command, args));
        await mount();
        await click('aerocryptNative.migrateLegacyMarker');
        expect(button('aerocryptNative.showKit').disabled).toBe(true);
        const reads = invoke.mock.calls.length;
        await click('aerocryptNative.showKit');
        expect(invoke.mock.calls.length).toBe(reads);
        await act(async () => { migrated = true; finishMigration({ changed: true, legacyDeleted: true }); });
        expect(button('aerocryptNative.showKit').disabled).toBe(false);
    });

    it('offers the new cached kit when metadata persistence succeeds', async () => {
        const original = invoke.getMockImplementation()!;
        let cachedNew = false;
        invoke.mockImplementation((command: string, args?: { account?: string; configJson?: string }) => {
            if (command === 'store_overlay_metadata') { cachedNew = true; return Promise.resolve(); }
            if (command === 'aerocrypt_profile_recovery_kit' && cachedNew) return Promise.resolve(freshKit);
            return original(command, args);
        });
        await mount();
        await click('aerocryptNative.migrateLegacyMarker');
        expect(host.textContent).toContain(freshKit.text);
        expect(host.textContent).not.toContain(oldKit.text);
        expect(host.textContent).not.toContain('aerocryptNative.overlayMetadataNotSaved');
    });
    it.each(['cache-fails', 'cache-succeeds'])('keeps a locked dialog locked after migration when %s', async (cache) => {
        const original = invoke.getMockImplementation()!;
        if (cache === 'cache-succeeds') invoke.mockImplementation((command, args) => (
            command === 'store_overlay_metadata' ? Promise.resolve() : original(command, args)
        ));
        await mount(false);
        expect(button('aerocryptNative.unlock')).toBeDefined();
        await click('aerocryptNative.migrateLegacyMarker');
        expect(button('aerocryptNative.unlock')).toBeDefined();
        expect(host.textContent).not.toContain('aerocryptNative.lockVault');
        expect(invoke.mock.calls.some(([command]) => command === 'provider_apply_crypt_overlay')).toBe(false);
    });

    it('keeps the offline kit after a backend-certified pre-publication failure', async () => {
        const original = invoke.getMockImplementation()!;
        invoke.mockImplementation((command, args) => {
            if (command === 'aerocrypt_provider_migrate_legacy_marker') return Promise.reject({ message: 'bad password', markerMayHaveChanged: false });
            if (command === 'aerocrypt_provider_read_config' && migrated) return Promise.reject(new Error('offline'));
            return original(command, args);
        });
        await mount();
        await click('aerocryptNative.migrateLegacyMarker');
        expect(host.textContent).toContain('bad password');
        expect(host.textContent).toContain(oldKit.text);
        migrated = true; // Remote is now unavailable; cache use must not require it.
        await click('aerocryptNative.hideKit');
        await click('aerocryptNative.showKit');
        expect(host.textContent).toContain(oldKit.text);
    });

    it.each([true, undefined])('does not restore the old kit after an uncertain migration failure (%s)', async (mayHaveChanged) => {
        const original = invoke.getMockImplementation()!;
        invoke.mockImplementation((command, args) => {
            if (command === 'aerocrypt_provider_migrate_legacy_marker') {
                migrated = true;
                return Promise.reject(mayHaveChanged ? { message: 'lost reply', markerMayHaveChanged: true } : 'unclassified transport failure');
            }
            if (command === 'aerocrypt_provider_read_config' && migrated) return Promise.reject(new Error('offline'));
            return original(command, args);
        });
        await mount();
        await click('aerocryptNative.migrateLegacyMarker');
        expect(host.textContent).not.toContain(oldKit.text);
        expect(host.textContent).not.toContain('aerocryptNative.saveKit');
        await click('aerocryptNative.showKit');
        expect(host.textContent).not.toContain(oldKit.text);
        expect(host.textContent).toContain('offline');
        expect(writeTextFile).not.toHaveBeenCalled();
    });

    it('keeps the old kit invalid when a successful migration cannot be read back', async () => {
        const original = invoke.getMockImplementation()!;
        invoke.mockImplementation((command, args) => (
            command === 'aerocrypt_provider_read_config' && migrated
                ? Promise.reject(new Error('read-back offline')) : original(command, args)
        ));
        await mount();
        await click('aerocryptNative.migrateLegacyMarker');
        await click('aerocryptNative.showKit');
        expect(host.textContent).not.toContain(oldKit.text);
        expect(host.textContent).toContain('read-back offline');
        expect(writeTextFile).not.toHaveBeenCalled();
    });

    it('does not re-offer the old kit when another client already migrated the marker', async () => {
        const original = invoke.getMockImplementation()!;
        invoke.mockImplementation((command, args) => {
            if (command === 'aerocrypt_provider_migrate_legacy_marker') {
                migrated = true;
                return Promise.resolve({ changed: false, legacyDeleted: false });
            }
            return original(command, args);
        });
        await mount();
        expect(host.textContent).toContain(oldKit.text);
        const cacheReads = invoke.mock.calls.filter(([c]) => c === 'aerocrypt_profile_recovery_kit').length;
        await click('aerocryptNative.migrateLegacyMarker');
        expect(host.textContent).not.toContain(oldKit.text);
        expect(host.textContent).not.toContain('aerocryptNative.saveKit');
        await click('aerocryptNative.showKit');
        expect(host.textContent).toContain(freshKit.text);
        expect(invoke.mock.calls.filter(([c]) => c === 'aerocrypt_profile_recovery_kit')).toHaveLength(cacheReads);
    });

});
