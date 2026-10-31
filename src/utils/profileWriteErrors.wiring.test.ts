// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { revertConvertedProfile } from './serverProfileStore';
import { transformWithOxc } from 'vite';
import { describe, expect, it, vi } from 'vitest';
import connection from '../components/ConnectionScreen.tsx?raw';
import hub from '../components/IntroHub/MyServersPanel.tsx?raw';
import settingsSource from '../components/SettingsPanel.tsx?raw';
import app from '../App.tsx?raw';
import { reorderVisibleInFull } from './reorderByIndex';
import { createProfileCredentialJournal } from './profileCredentialJournal';
import { mergeAppSettingsDraft } from './appSettingsDraft';
import { ConnectScope } from '../gui/connectScope';
import { normalizeGuiPresentation } from '../gui/presentation';
import { connectionProfileDefaults } from '../providers/connectionManifest';
import type { ServerProfile } from '../types';

// Run the production closures, with their captured IPC and React state cells
// supplied by the fixture. No save handler implementation is duplicated here.
function declaration(source: string, name: string) {
    const start = source.indexOf(`    const ${name} =`);
    const end = source.indexOf('\n    const ', start + 1);
    if (start < 0 || end < 0) throw new Error(`Missing closure: ${name}`);
    return source.slice(start, end);
}

async function execute(source: string, names: string[], context: Record<string, unknown>) {
    if (source === connection && source.includes('const readProfilesForSave =')) names = ['readProfilesForSave', 'persistProfilesForSave', ...names];
    const { code } = await transformWithOxc(names.map(name => declaration(source, name)).join('\n'), 'profile-write.ts');
    return new Function(...Object.keys(context), `${code}\nreturn { ${names.join(', ')} };`)(...Object.values(context)) as Record<string, (...args: any[]) => Promise<unknown>>;
}

async function snippet(source: string, context: Record<string, unknown>) {
    const { code } = await transformWithOxc(source, 'profile-write-snippet.ts');
    return new Function(...Object.keys(context), `return (async () => { ${code} })();`)(...Object.values(context));
}

function fixture(fail: 'read' | 'write' | 'none' = 'write') {
    const original = { id: 'old', name: 'Original', protocol: 'ftp', host: 'example.test', username: 'user' };
    const servers = [original, { ...original, id: 'other', name: 'Other' }];
    const read = vi.fn(async () => { if (fail === 'read') throw new Error('STORE_NOT_READY'); return structuredClone(servers); });
    // Like the real store, a write hands back the list as stored.
    const store = vi.fn(async (profiles: ServerProfile[]) => { if (fail === 'write') throw new Error('Disk full'); return profiles; });
    const events: Array<{ type: string; detail?: any }> = [];
    class Event { constructor(public type: string, public init?: { detail: unknown }) {} get detail() { return this.init?.detail; } }
    const context: Record<string, any> = {
        protocol: 'ftp', editingProfileId: 'old', editingProfile: original, connectionName: 'Edited', saveConnection: true,
        connectionParams: { server: 'example.test', username: 'user', password: 'fixture', options: {} },
        quickConnectDirs: { remoteDir: '/remote', localDir: '/local' }, selectedProvider: undefined, selectedProviderId: undefined,
        aeroCryptConfirmMismatch: false, overlaysRemotePathError: false, defaultSaltEntropyMismatch: false,
        cryptFormsHalfRecorded: false, remotePathEscapesOverlay: false, aeroCryptEnabled: false,
        mtpFingerprint: undefined, persistModeCredentials: true, inModeGroup: true,
        oauthOverlaySaveBlocked: false,
        modeChanged: true, targetModeLabel: 'Native', originalEditMode: { protocol: 'webdav' },
        customIconForSave: undefined, faviconForSave: undefined, editHydratedPasswordRef: { current: '' },
        modeCredentialSnapshotsRef: { current: { ftp: 'fixture' } }, bridgeSaveBlocked: false,
        loadSavedServerProfiles: read, loadSavedServerProfilesStrict: read, storeSavedServerProfiles: store, revertConvertedProfile, accountChangeGeneration: () => 0,
        readSavedServerProfilesForWrite: async () => ({ profiles: await read(), userId: 7, revision: 'r7' }),
        tryStoreCredential: vi.fn(async () => true), stashFilenApiKey: vi.fn(async () => false),
        aeroCryptOverlayFields: vi.fn(async () => ({})), migrateCryptCredentials: vi.fn(),
        normalizeMegaOptions: (v: unknown) => v, getDefaultPort: () => 21, connectionProfileDefaults,
        findDuplicateProfile: () => undefined,
        getStorageDedupKey: () => 'dedup', isBlompAuthUrl: () => false,
        syncPersistedModeCredentials: vi.fn(), carryFavoriteServer: vi.fn(), carryServerGroups: vi.fn(),
        deleteModeCredentials: vi.fn(), warnIfSavedByOtherAccount: vi.fn(),
        setSavedServersUpdate: vi.fn(), logActivity: vi.fn(), setGitHubAlert: vi.fn(),
        endEditSession: vi.fn(), setConnectionName: vi.fn(), setSaveConnection: vi.fn(), setPersistModeCredentials: vi.fn(),
        setMtpDevices: vi.fn(), setMtpSelectedDeviceId: vi.fn(), setMtpFingerprint: vi.fn(), setMtpDetectError: vi.fn(),
        onConnectionParamsChange: vi.fn(), onQuickConnectDirsChange: vi.fn(), onFormSaved: vi.fn(), onConnect: vi.fn(),
        logger: { warn: vi.fn(), error: vi.fn(), debug: vi.fn() }, t: (key: string) => key,
        window: { dispatchEvent: vi.fn((e: Event) => { events.push({ type: e.type, detail: e.detail }); }) }, CustomEvent: Event,
        invoke: vi.fn(async () => undefined), console: { error: vi.fn() },
        createProfileCredentialJournal,
        useCallback: (fn: unknown) => fn, servers, setServers: vi.fn(), onServersChange: vi.fn(),
        setRenamingId: vi.fn(), deleteTarget: original, setDeleteTarget: vi.fn(), setGroups: vi.fn(),
        copyProfileVaultSecrets: vi.fn(async () => ({})), deleteProfileVaultSecrets: vi.fn(async () => undefined),
        pruneServerFromGroups: vi.fn(async () => undefined),
        visibleListRef: { current: servers }, dragServerIdRef: { current: 'old' },
        setDragIdx: vi.fn(), setOverIdx: vi.fn(), DRAG_SENTINEL_TOP: -1, DRAG_SENTINEL_BOTTOM: -2, reorderVisibleInFull,
    };
    return { context, read, store, events, servers };
}

function settingsSaveFixture(launchOnStartup: boolean, autostartAtOpen: boolean | null, portableMode: boolean | null, touched = true) {
    const f = fixture('none');
    const settings = {
        fontSize: 14, fontFamily: 'system', introHubIconSize: 32,
        launchOnStartup, showSystemMenu: false,
    };
    Object.assign(f.context, {
        saveState: 'idle', settings,
        settingsBase: { current: { ...settings } }, settingsDraft: { current: { ...settings } },
        autostartAtOpen: { current: autostartAtOpen }, autostartEditRevision: { current: 0 }, autostartTouched: { current: touched }, portableMode,
        panelOpen: { current: true }, ConnectScope, bindSettingsScope: async (scope: ConnectScope) => scope,
        mergeAppSettingsDraft, normalizeGuiPresentation, updateAppSettings: vi.fn(async () => ({ ...settings })),
        clampAppFontSize: (v: unknown) => v, normalizeAppFontFamily: (v: unknown) => v, clampIntroHubIconSize: (v: unknown) => v,
        SETTINGS_VAULT_KEY: 'settings', SETTINGS_KEY: 'settings', OAUTH_SETTINGS_KEY: 'oauth',
        oauthSettings: Object.fromEntries(['googledrive', 'dropbox', 'onedrive', 'box', 'pcloud', 'fourshared', 'zohoworkdrive', 'yandexdisk'].map(p => [p, {}])),
        localStorage: { removeItem: vi.fn() }, enableAutostart: vi.fn(), disableAutostart: vi.fn(), isAutostartEnabled: vi.fn(),
        setSettings: vi.fn(), setHasChanges: vi.fn(), setSaveState: vi.fn(), onClose: vi.fn(), setTimeout: vi.fn(),
    });
    return f;
}

describe('saved profile write rejection in production handlers', () => {
    it.each(['read', 'write'] as const)('keeps the primary editor open on %s failure', async fail => {
        const f = fixture(fail);
        const fn = await execute(connection, ['saveToServers', 'handleConnectAndSave'], f.context);
        await fn.handleConnectAndSave();
        expect(f.context.setGitHubAlert).toHaveBeenCalledWith(expect.objectContaining({ type: 'error', title: 'toast.saveFailed' }));
        for (const name of ['endEditSession', 'onFormSaved', 'onConnectionParamsChange', 'syncPersistedModeCredentials', 'setSavedServersUpdate', 'logActivity']) {
            expect(f.context[name], name).not.toHaveBeenCalled();
        }
        if (fail === 'read') {
            expect(f.store).not.toHaveBeenCalled();
            expect(f.context.tryStoreCredential).not.toHaveBeenCalled();
        }
    });

    it('keeps the new-profile form open when its write fails', async () => {
        const f = fixture();
        f.context.editingProfileId = null;
        const fn = await execute(connection, ['saveToServers', 'handleConnectAndSave'], f.context);
        await fn.handleConnectAndSave();
        expect(f.context.setGitHubAlert).toHaveBeenCalled();
        expect(f.context.onFormSaved).not.toHaveBeenCalled();
        expect(f.context.setConnectionName).not.toHaveBeenCalled();
        expect(f.context.logActivity).not.toHaveBeenCalled();
    });

    it.each(['handleSaveAsNew', 'handleConvertMode'] as const)('%s stops before snapshots, groups and editor reset', async name => {
        const f = fixture();
        const fn = await execute(connection, [name], f.context);
        await fn[name]();
        expect(f.context.setGitHubAlert).toHaveBeenCalled();
        for (const n of ['syncPersistedModeCredentials', 'deleteModeCredentials', 'carryFavoriteServer', 'carryServerGroups', 'setSavedServersUpdate', 'endEditSession', 'onFormSaved']) {
            expect(f.context[n], n).not.toHaveBeenCalled();
        }
        expect(f.events.filter(e => e.detail?.type === 'success')).toEqual([]);
    });

    it.each(['handleSaveAsNew', 'handleConvertMode'] as const)('%s refuses a failed strict read before any credential mutation', async name => {
        const f = fixture('read');
        const fn = await execute(connection, [name], f.context);
        await fn[name]();
        expect(f.context.setGitHubAlert).toHaveBeenCalled();
        expect(f.store).not.toHaveBeenCalled();
        expect(f.context.tryStoreCredential).not.toHaveBeenCalled();
    });

    it('keeps the convert Undo available if restoring the profiles fails', async () => {
        const f = fixture('none');
        const fn = await execute(connection, ['handleConvertMode'], f.context);
        await fn.handleConvertMode();
        const undo = f.events.find(e => e.detail?.type === 'success')!.detail.action.onClick;
        f.store.mockRejectedValueOnce(new Error('Disk full'));
        for (const name of ['deleteModeCredentials', 'carryFavoriteServer', 'carryServerGroups', 'setSavedServersUpdate']) f.context[name].mockClear();
        await undo();
        expect(f.context.setGitHubAlert).toHaveBeenCalled();
        expect(f.context.deleteModeCredentials).not.toHaveBeenCalled();
        expect(f.context.carryFavoriteServer).not.toHaveBeenCalled();
        expect(f.context.setSavedServersUpdate).not.toHaveBeenCalled();
        expect(f.events.filter(e => e.detail?.title === 'connection.convertUndone')).toEqual([]);
    });

    it('retains the Settings dialog and dirty state when the saved profiles cannot be read', async () => {
        const f = fixture('read');
        Object.assign(f.context, {
            saveState: 'idle', settings: { fontSize: 14, fontFamily: 'system', introHubIconSize: 32 },
            settingsBase: { current: { fontSize: 12, fontFamily: 'system', introHubIconSize: 32 } },
            panelOpen: { current: true }, ConnectScope, bindSettingsScope: async (scope: ConnectScope) => scope,
            mergeAppSettingsDraft, normalizeGuiPresentation, updateAppSettings: vi.fn(async mutate => mutate({ fontSize: 12 })),
            clampAppFontSize: (v: unknown) => v, normalizeAppFontFamily: (v: unknown) => v, clampIntroHubIconSize: (v: unknown) => v,
            secureStoreAndClean: vi.fn(), SETTINGS_VAULT_KEY: 'settings', SETTINGS_KEY: 'settings', OAUTH_SETTINGS_KEY: 'oauth',
            oauthSettings: Object.fromEntries(['googledrive', 'dropbox', 'onedrive', 'box', 'pcloud', 'fourshared', 'zohoworkdrive', 'yandexdisk'].map(p => [p, {}])),
            localStorage: { removeItem: vi.fn() }, enableAutostart: vi.fn(), disableAutostart: vi.fn(),
            setSettings: vi.fn(), setHasChanges: vi.fn(), setSaveState: vi.fn(), onClose: vi.fn(), setTimeout: vi.fn(),
        });
        const fn = await execute(settingsSource, ['handleSave'], f.context);
        await fn.handleSave();
        expect(f.events.some(e => e.detail?.type === 'error')).toBe(true);
        expect(f.context.setSaveState).toHaveBeenLastCalledWith('idle');
        expect(f.context.setHasChanges).not.toHaveBeenCalled();
        expect(f.context.setTimeout).not.toHaveBeenCalled();
        expect(f.context.invoke).not.toHaveBeenCalled();
        expect(f.events.filter(e => e.type === 'aeroftp-settings-changed')).toEqual([]);
        expect(f.context.updateAppSettings).not.toHaveBeenCalled();
        expect(f.store).not.toHaveBeenCalled();
    });

    it('portable Settings save never changes the installed app autostart', async () => {
        const f = settingsSaveFixture(true, false, true);
        const fn = await execute(settingsSource, ['handleSave'], f.context);
        await fn.handleSave();
        expect(f.context.enableAutostart).not.toHaveBeenCalled();
        expect(f.context.disableAutostart).not.toHaveBeenCalled();
    });

    it('installed Settings save leaves autostart untouched when the checkbox did not change', async () => {
        const f = settingsSaveFixture(false, false, false);
        const fn = await execute(settingsSource, ['handleSave'], f.context);
        await fn.handleSave();
        expect(f.context.enableAutostart).not.toHaveBeenCalled();
        expect(f.context.disableAutostart).not.toHaveBeenCalled();
    });

    it('installed Settings save applies a changed autostart checkbox exactly once', async () => {
        const f = settingsSaveFixture(true, false, false);
        const fn = await execute(settingsSource, ['handleSave'], f.context);
        await fn.handleSave();
        expect(f.context.enableAutostart).toHaveBeenCalledTimes(1);
        expect(f.context.disableAutostart).not.toHaveBeenCalled();
    });

    it('installed Settings save leaves autostart untouched when only a saved value differs from the OS', async () => {
        const f = settingsSaveFixture(true, false, false, false);
        const fn = await execute(settingsSource, ['handleSave'], f.context);
        await fn.handleSave();
        expect(f.context.enableAutostart).not.toHaveBeenCalled();
        expect(f.context.disableAutostart).not.toHaveBeenCalled();
    });

    it('installed Settings save applies a checkbox edit when the OS state could not be read', async () => {
        const f = settingsSaveFixture(true, null, false);
        const fn = await execute(settingsSource, ['handleSave'], f.context);
        await fn.handleSave();
        expect(f.context.enableAutostart).toHaveBeenCalledTimes(1);
        expect(f.context.disableAutostart).not.toHaveBeenCalled();
    });

    it('saved-settings load keeps an OS autostart value that resolved first', async () => {
        const marker = settingsSource.indexOf('    // Load settings on open');
        const start = settingsSource.indexOf('    useEffect(() => {', marker);
        const end = settingsSource.indexOf('\n', settingsSource.indexOf('    }, [isOpen]);', start));
        let resolveSaved!: (settings: { launchOnStartup: boolean }) => void;
        const secureGetWithFallback = vi.fn(() => new Promise<{ launchOnStartup: boolean }>(resolve => { resolveSaved = resolve; }));
        const setSettings = vi.fn();
        const pending = snippet(settingsSource.slice(start, end), {
            useEffect: (callback: () => void) => callback(), isOpen: true,
            oauthEdits: { current: {} }, settingsLoadRevision: { current: 0 }, panelOpen: { current: true },
            ConnectScope, GuiError: class extends Error {}, bindSettingsScope: async (scope: ConnectScope) => scope,
            secureGetWithFallback, SETTINGS_VAULT_KEY: 'settings', SETTINGS_KEY: 'settings',
            defaultSettings: { fontSize: 14, fontFamily: 'system', introHubIconSize: 32, sftpDownloadPreset: 'ask', launchOnStartup: false },
            normalizeGuiPresentation: (value: unknown) => value, clampAppFontSize: (value: unknown) => value,
            normalizeAppFontFamily: (value: unknown) => value, clampIntroHubIconSize: (value: unknown) => value,
            normalizeSftpDownloadPreset: (value: unknown) => value, autostartAtOpen: { current: false },
            settingsBase: { current: null }, setSettings, loadSavedServerProfiles: async () => [], setServers: vi.fn(),
            defaultOAuthSettings: Object.fromEntries(['googledrive', 'dropbox', 'onedrive', 'box', 'pcloud', 'fourshared', 'zohoworkdrive', 'yandexdisk'].map(provider => [provider, {}])),
            loadOAuthClientCredentials: async () => ({}), notifyOAuthKeysUnavailable: vi.fn(), keyReadFailure: String,
            t: (key: string) => key, setOauthSettings: vi.fn(),
            invoke: vi.fn(async (command: string) => command === 'get_credential_store_status'
                ? { master_mode: false, is_locked: false, timeout_seconds: 0 }
                : command === 'aerovault_overlay_get_idle_timeout' ? 60 : false),
            setMasterPasswordStatus: vi.fn(), setAutoLockTimeout: vi.fn(), setAeroVaultOverlayTimeoutMin: vi.fn(),
            setVaultEntriesCount: vi.fn(), setVaultCategories: vi.fn(), setTotpEnabled: vi.fn(), console,
        });
        await pending;
        await vi.waitFor(() => expect(secureGetWithFallback).toHaveBeenCalledOnce());
        resolveSaved({ launchOnStartup: true });
        await vi.waitFor(() => expect(setSettings).toHaveBeenCalledWith(expect.objectContaining({ launchOnStartup: false })));
    });

    it('keystore import refresh keeps a checkbox edit made during its autostart read', async () => {
        const marker = settingsSource.indexOf('// The import may have toggled OS-level');
        const start = settingsSource.indexOf('if (portableMode === false) {', marker);
        const end = settingsSource.indexOf('                                                                            } catch (e) {', start);
        let resolveEnabled!: (enabled: boolean) => void;
        const isAutostartEnabled = vi.fn(() => new Promise<boolean>(resolve => { resolveEnabled = resolve; }));
        const autostartEditRevision = { current: 0 };
        const autostartAtOpen = { current: null };
        const setSettings = vi.fn();
        const pending = snippet(settingsSource.slice(start, end), {
            portableMode: false, autostartEditRevision, autostartAtOpen, isAutostartEnabled, setSettings,
        });
        await vi.waitFor(() => expect(isAutostartEnabled).toHaveBeenCalledOnce());
        autostartEditRevision.current++;
        resolveEnabled(false);
        await pending;
        expect(autostartAtOpen.current).toBe(false);
        expect(setSettings).not.toHaveBeenCalled();
    });

    it.each(['handleDuplicate', 'handleRenameSubmit', 'confirmDelete', 'handleDrop'] as const)('IntroHub %s never publishes unpersisted state', async name => {
        const f = fixture();
        const fn = await execute(hub, [name], f.context);
        if (name === 'handleDrop') await fn[name](1, { preventDefault: vi.fn(), dataTransfer: { getData: () => 'old' } });
        else await fn[name](f.servers[0], 'Renamed');
        expect(f.store).toHaveBeenCalled();
        expect(f.context.setServers).not.toHaveBeenCalled();
        expect(f.events.some(e => e.detail?.type === 'error')).toBe(true);
        expect(f.context.logActivity).not.toHaveBeenCalled();
        // A failed duplicate removes the secrets it copied for its unlisted copy,
        // never those of the profile it copied from.
        if (name === 'handleDuplicate') {
            expect(f.context.deleteProfileVaultSecrets).toHaveBeenCalledTimes(1);
            expect(f.context.deleteProfileVaultSecrets).not.toHaveBeenCalledWith(f.servers[0].id, expect.anything());
        } else {
            expect(f.context.deleteProfileVaultSecrets).not.toHaveBeenCalled();
        }
        expect(f.context.pruneServerFromGroups).not.toHaveBeenCalled();
        expect(f.context.setGroups).not.toHaveBeenCalled();
        expect(f.context.setRenamingId).not.toHaveBeenCalled();
        expect(f.context.setDeleteTarget).not.toHaveBeenCalled();
    });

    it('closes the primary editor after a successful persisted save', async () => {
        const f = fixture('none');
        const fn = await execute(connection, ['saveToServers', 'handleConnectAndSave'], f.context);
        await fn.handleConnectAndSave();
        expect(f.store).toHaveBeenCalledWith(expect.arrayContaining([expect.objectContaining({ id: 'old', name: 'Edited' })]), false, 7, 'r7');
        expect(f.context.onFormSaved).toHaveBeenCalledTimes(1);
        expect(f.context.syncPersistedModeCredentials).toHaveBeenCalledWith('old');
    });

    it.each(['handleSaveAsNew', 'handleConvertMode'] as const)('%s honors custom icon removal when switching modes', async name => {
        const f = fixture('none');
        Object.assign(f.servers[0], { customIconUrl: 'data:image/png;base64,dGVzdA==' });
        f.context.customIconForSave = undefined;
        const fn = await execute(connection, [name], f.context);
        await fn[name]();
        expect(f.store).toHaveBeenCalledOnce();
        const saved = f.store.mock.calls[0][0] as ServerProfile[];
        const converted = saved.find(profile => !['old', 'other'].includes(profile.id));
        expect(converted).toBeDefined();
        expect(converted!.customIconUrl).toBeUndefined();
    });

    it('does not reset the primary form while persistence is still pending', async () => {
        const f = fixture('none');
        let resolve!: () => void;
        f.store.mockImplementationOnce(profiles => new Promise<ServerProfile[]>(ok => { resolve = () => ok(profiles); }));
        const fn = await execute(connection, ['saveToServers', 'handleConnectAndSave'], f.context);
        const pending = fn.handleConnectAndSave();
        await vi.waitFor(() => expect(f.store).toHaveBeenCalled());
        expect(f.context.onFormSaved).not.toHaveBeenCalled();
        expect(f.context.logActivity).not.toHaveBeenCalled();
        resolve();
        await pending;
        expect(f.context.onFormSaved).toHaveBeenCalledOnce();
    });

    it.each(['handleDuplicate', 'handleRenameSubmit', 'confirmDelete', 'handleDrop'] as const)('IntroHub %s refuses an unavailable partition before mutating it', async name => {
        const f = fixture('read');
        const fn = await execute(hub, [name], f.context);
        if (name === 'handleDrop') await fn[name](1, { preventDefault: vi.fn(), dataTransfer: { getData: () => 'old' } });
        else await fn[name](f.servers[0], 'Renamed');
        expect(f.store).not.toHaveBeenCalled();
        expect(f.context.setServers).not.toHaveBeenCalled();
        expect(f.context.copyProfileVaultSecrets).not.toHaveBeenCalled();
        expect(f.events.some(e => e.detail?.type === 'error')).toBe(true);
    });

    it('IntroHub preserves profiles that appeared since the last render', async () => {
        const f = fixture('none');
        const unseen = { ...f.servers[0], id: 'unseen' };
        f.read.mockResolvedValueOnce([...f.servers, unseen]);
        const fn = await execute(hub, ['handleRenameSubmit'], f.context);
        await fn.handleRenameSubmit(f.servers[0], 'Renamed');
        expect(f.store).toHaveBeenCalledWith([expect.objectContaining({ name: 'Renamed' }), f.servers[1], unseen], false, 7, 'r7');
        expect(f.context.setRenamingId).toHaveBeenCalledWith(null);
    });

    it.each(['read', 'write'] as const)('the peer edit rejects %s failure so its consumer keeps the form open', async fail => {
        const f = fixture(fail);
        const fn = await execute(connection, ['savePeerEditedProfile'], f.context);
        await expect(fn.savePeerEditedProfile({ alias: 'Peer', localFolder: '/local' })).rejects.toThrow();
        expect(f.context.logActivity).not.toHaveBeenCalled();
        expect(f.context.setSavedServersUpdate).not.toHaveBeenCalled();
    });

    it.each(['read', 'write'] as const)('OAuth metadata %s failure keeps the editor open and reports the error', async fail => {
        const f = fixture(fail);
        const fn = await execute(connection, ['handleOAuthMetadataSave'], f.context);
        await fn.handleOAuthMetadataSave();
        expect(f.context.setGitHubAlert).toHaveBeenCalledWith(expect.objectContaining({ type: 'error' }));
        expect(f.context.onFormSaved).not.toHaveBeenCalled();
        expect(f.context.setSavedServersUpdate).not.toHaveBeenCalled();
        expect(f.context.logActivity).not.toHaveBeenCalled();
    });

    it.each(['4shared', 'OAuth new', 'OAuth edit'] as const)('%s does not connect as saved after a failed write', async mode => {
        const f = fixture();
        f.context.editingProfileId = mode === 'OAuth edit' ? 'old' : null;
        const marker = mode === '4shared' ? 'onConnected={async (displayName) => {' : 'onConnected={async (displayName, extraOptions) => {';
        const start = connection.indexOf(marker);
        const end = connection.indexOf('\n                                }}', start);
        const helpers = connection.includes('const readProfilesForSave =')
            ? declaration(connection, 'readProfilesForSave') + declaration(connection, 'persistProfilesForSave') : '';
        await snippet(helpers + '\nconst onConnected = ' + connection.slice(start + 'onConnected={'.length, end) + '\n};\nawait onConnected("Cloud", {});', f.context);
        expect(f.store).toHaveBeenCalled();
        expect(f.context.onConnect).not.toHaveBeenCalled();
        expect(f.context.setGitHubAlert).toHaveBeenCalled();
    });

    it.each(['read', 'write'] as const)('favicon %s failure logs an error and does not refresh a persisted card', async fail => {
        const f = fixture(fail);
        const start = app.indexOf('  const handleFaviconDetected =');
        const end = app.indexOf('\n  useFaviconDetection', start);
        Object.assign(f.context, { React: { useCallback: (fn: unknown) => fn }, setSessions: vi.fn(), setServersRefreshKey: vi.fn() });
        await snippet(app.slice(start, end) + '\nawait handleFaviconDetected("old", "https://example.test/icon.png");', f.context);
        expect(f.context.setServersRefreshKey).not.toHaveBeenCalled();
        expect(f.context.logger.warn).toHaveBeenCalled();
    });

    it.each(['read', 'write'] as const)('Filen migration %s failure does not mark completion', async fail => {
        const f = fixture(fail);
        const start = app.indexOf('  useEffect(() => {', app.indexOf('// Issue #230: one-time migration'));
        const end = app.indexOf('  // Listen for app background', start);
        const effect = app.slice(start, end);
        Object.assign(f.context, {
            useEffect: (fn: () => unknown) => fn(), localStorage: { getItem: () => null, setItem: vi.fn() },
            migrateFilenApiKeysToVault: vi.fn(async () => ({ migrated: f.servers, changed: true })), setServersRefreshKey: vi.fn(),
        });
        await snippet(effect, f.context);
        await new Promise(resolve => setTimeout(resolve, 0));
        expect(f.context.localStorage.setItem).not.toHaveBeenCalled();
        expect(f.context.setServersRefreshKey).not.toHaveBeenCalled();
        expect(f.context.logger.warn).toHaveBeenCalled();
    });

    it('failed import persistence retains import input and reports the failure', async () => {
        const f = fixture();
        const start = settingsSource.indexOf('                                                                        try {', settingsSource.indexOf('// Reload server profiles from the imported keystore'));
        const end = settingsSource.indexOf("setKeystoreImportPassword('');", start) + "setKeystoreImportPassword('');".length;
        Object.assign(f.context, {
            result: { profilesAfterDecisions: f.servers }, setKeystoreMessage: vi.fn(), onServersChanged: vi.fn(),
            setKeystoreMetadata: vi.fn(), setKeystoreImportFilePath: vi.fn(), setKeystoreImportPassword: vi.fn(), setKeystoreImportResult: vi.fn(),
            invoke: vi.fn(async () => JSON.stringify(f.servers)),
        });
        await snippet(settingsSource.slice(start, end), f.context);
        expect(f.context.setKeystoreMessage).toHaveBeenCalledWith(expect.objectContaining({ type: 'error' }));
        expect(f.context.setServers).not.toHaveBeenCalled();
        expect(f.context.onServersChanged).not.toHaveBeenCalled();
        expect(f.context.setKeystoreImportFilePath).not.toHaveBeenCalled();
        expect(f.context.setKeystoreImportPassword).not.toHaveBeenCalled();
        expect(f.context.setKeystoreImportResult).toHaveBeenCalledWith(null);
    });

    it.each(['replace', 'clear', 'native'] as const)('restores existing password, Filen and overlay secrets after a failed profile %s', async mode => {
        const f = fixture();
        const secrets = new Map([
            ['server_old', 'old-password'], ['filen_api_key_old', 'old-filen'],
            ['aerocrypt_overlay_pw_old', 'old-overlay'], ['aerocrypt_overlay_salt_old', 'old-salt'],
            ['aerocrypt_overlay_keyfile_path_old', '/old/keyfile'],
        ]);
        const before = new Map(secrets);
        const invoke = vi.fn(async (cmd: string, args: any) => {
            if (cmd === 'get_credential') {
                if (!secrets.has(args.account)) throw new Error(`Credential not found: ${args.account}`);
                return secrets.get(args.account);
            }
            if (cmd === 'store_credential') secrets.set(args.account, args.password);
            if (cmd === 'delete_credential') secrets.delete(args.account);
        });
        Object.assign(f.context, {
            invoke, aeroCryptEnabled: true, overlayEligible: true, aeroCryptKind: mode === 'native' ? 'aerocrypt' : 'rclone-crypt',
            aeroCryptPassword: 'new-overlay', aeroCryptSalt: 'new-salt', aeroCryptKeyfilePath: '/new/keyfile',
            aeroCryptWithHeader: true, effectiveUseDefaultSalt: false, aeroCryptFilenameEnc: true, aeroCryptDirNameEnc: true,
            aeroCryptPasswordForm: undefined, aeroCryptSaltForm: undefined, overlaysRemotePath: '/remote', normalizeRemotePath: (s: string) => s,
            resolveOverlayScope: () => '/remote',
        });
        f.context.servers[0].hasStoredFilenApiKey = true;
        f.context.connectionParams.options.filen_api_key = mode === 'clear' ? '' : 'new-filen';
        if (mode === 'clear') {
            f.context.connectionParams.password = '';
            f.context.editHydratedPasswordRef.current = 'old-password';
        }
        const context = { ...f.context };
        for (const name of ['tryStoreCredential', 'stashFilenApiKey', 'aeroCryptOverlayFields']) delete context[name];
        const names = ['tryStoreCredential', 'stashFilenApiKey', 'aeroCryptOverlayFields', 'saveToServers', 'handleConnectAndSave'];
        // Execute the real credential helpers too; only the IPC vault is fake.
        if (connection.includes('const writeProfileCredential =')) names.unshift('writeProfileCredential');
        const fn = await execute(connection, names, context);
        await fn.handleConnectAndSave();
        expect(secrets).toEqual(before);
        expect(f.context.onFormSaved).not.toHaveBeenCalled();
    });

    it('keeps the delete callback bound to the current translator', () => {
        const deleteCallback = declaration(hub, 'confirmDelete');
        expect(deleteCallback).toMatch(/\}, \[[^\]]*\bt\b[^\]]*\]\);/);
    });
});

describe('profile credential journal', () => {
    it('removes newly minted secrets when the profile cannot be saved', async () => {
        const invoke = vi.fn().mockRejectedValueOnce(new Error('Failed to get credential: Credential not found: server_new')).mockResolvedValue(undefined);
        const journal = createProfileCredentialJournal(invoke);
        await journal.write('server_new', 'fixture');
        await journal.rollback();
        expect(invoke).toHaveBeenLastCalledWith('delete_credential', { account: 'server_new' });
    });
    it('refuses to change an unreadable secret and refuses subsequent profile persistence', async () => {
        const invoke = vi.fn().mockRejectedValue(new Error('STORE_NOT_READY'));
        const journal = createProfileCredentialJournal(invoke);
        await expect(journal.write('server_old', 'fixture')).rejects.toThrow('STORE_NOT_READY');
        expect(() => journal.assertReady()).toThrow('STORE_NOT_READY');
        expect(invoke).toHaveBeenCalledTimes(1);
        await journal.rollback();
        expect(invoke).toHaveBeenCalledTimes(1);
    });
    it('restores the original value after repeated mutations and releases it after success', async () => {
        const invoke = vi.fn().mockResolvedValue('original');
        const journal = createProfileCredentialJournal(invoke);
        await journal.write('server_old', 'first');
        await journal.write('server_old', 'second');
        await journal.rollback();
        expect(invoke.mock.calls.filter(([cmd]) => cmd === 'get_credential')).toHaveLength(1);
        expect(invoke).toHaveBeenLastCalledWith('store_credential', { account: 'server_old', password: 'original' });
        const committed = createProfileCredentialJournal(invoke);
        await committed.write('server_old', 'saved');
        committed.commit();
        invoke.mockClear();
        await committed.rollback();
        expect(invoke).not.toHaveBeenCalled();
    });
    it('reports rollback failure without leaking secret values and still restores other accounts', async () => {
        const invoke = vi.fn().mockResolvedValue('secret-fixture');
        const journal = createProfileCredentialJournal(invoke);
        await journal.write('server_old', 'replacement');
        await journal.write('filen_api_key_old', 'replacement');
        invoke.mockRejectedValueOnce(new Error('disk full'));
        await expect(journal.rollback()).rejects.toThrow('Could not restore saved profile credentials: server_old');
        expect(invoke).toHaveBeenLastCalledWith('store_credential', { account: 'filen_api_key_old', password: 'secret-fixture' });
    });
});

describe('all saved-profile write sites handle persistence failures', () => {
    it.each([['ConnectionScreen', connection], ['IntroHub', hub], ['SettingsPanel', settingsSource], ['App', app]])('%s has no swallowed or unawaited store writes', (_name, source) => {
        expect(source).not.toMatch(/storeSavedServerProfiles\([^\n]*\)\.catch\(\s*\(\)\s*=>\s*\{\s*\}\s*\)/);
        expect(source).not.toMatch(/try\s*\{\s*await storeSavedServerProfiles\([^\n]*\);\s*\}\s*catch\s*\{\s*(?:\/\*[^*]*\*\/)\s*\}/);
        for (const line of source.split('\n').filter(line => /storeSavedServerProfiles\(.+\)/.test(line) && !line.trim().startsWith('//'))) {
            expect(line).toMatch(/\bawait storeSavedServerProfiles\(/);
        }
    });
});

describe('the saved-profile list shown after a write', () => {
    // The vault settles the names (" (1)" for a name another profile holds),
    // and My Servers keeps its own copy (it does not listen to
    // PROFILES_CHANGED_EVENT), so it must take the list the write hands back.
    it('My Servers shows the list as stored, not the list it sent', () => {
        const writes = hub.split('\n').filter(line => /storeSavedServerProfiles\(.+\)/.test(line) && !line.trim().startsWith('//'));
        expect(writes.length).toBeGreaterThan(0);
        for (const line of writes) {
            expect(line).toMatch(/setServers\(await storeSavedServerProfiles\(|(?:const )?stored = await storeSavedServerProfiles\(/);
        }
    });

    it('Settings never shows the list it sent once the write has stored it', () => {
        // Its one list write is the keystore restore, which shows what that stored.
        expect(settingsSource).not.toMatch(/await storeSavedServerProfiles\(\w+/);
        expect(settingsSource).toContain('const stored = await replaceSavedServerProfiles(');
        expect(settingsSource).toContain('setServers(stored);');
    });
});
