// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ServerProfile } from '../types';

import { PROFILES_CHANGED_EVENT, type ProfilesChangedDetail } from '../utils/serverProfileStore';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/webview', () => ({
    getCurrentWebview: () => ({ onDragDropEvent: async () => () => undefined }),
}));
vi.mock('../utils/pickPath', () => ({
    pickFile: vi.fn(async () => '/backups/servers.aeroftp'),
    pickSave: vi.fn(),
}));
vi.mock('../i18n', () => ({
    useTranslation: () => (key: string, params?: Record<string, unknown>) =>
        params ? `${key} ${JSON.stringify(params)}` : key,
}));
// An account switch or lock while an import runs, for the test that needs one.
let accountChanges = 0;
vi.mock('../utils/userPartitions', async original => ({
    ...(await original<typeof import('../utils/userPartitions')>()),
    accountChangeGeneration: () => accountChanges,
}));
vi.mock('./BridgeSourcePanel', () => ({ BridgeSourcePanel: () => null }));

import { invoke } from '@tauri-apps/api/core';
import { ExportImportDialog } from './ExportImportDialog';

let root: Root;
let host: HTMLDivElement;

beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    vi.useFakeTimers({ toFake: ['setTimeout'] });
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
});

afterEach(() => {
    act(() => root.unmount());
    host.remove();
    vi.useRealTimers();
    vi.mocked(invoke).mockReset();
});

/** The last button on screen that `match` accepts; there must be one. */
const button = (match: (b: HTMLButtonElement) => boolean): HTMLButtonElement => {
    const found = [...host.querySelectorAll('button')].filter(match);
    expect(found.length).toBeGreaterThan(0);
    return found[found.length - 1];
};

/** Click `el` and let React settle. */
const click = async (el: HTMLElement) => {
    await act(async () => {
        el.click();
    });
};

/** Open the import step and choose the file. */
async function openImport(onProfilesStored: (s: ServerProfile[]) => void, onClose: () => void) {
    await act(async () => {
        root.render(createElement(ExportImportDialog, { servers: [], initialMode: 'import', onImport: vi.fn(), onProfilesStored, onClose }));
    });
    await click(button(b => b.textContent?.includes('settings.importChooseFile') ?? false));
}

/** Type the password, pick `mode`, run the import. */
async function runImport(mode: 'skip' | 'overwrite' | 'duplicate') {
    const input = host.querySelector<HTMLInputElement>('input[type="password"]')!;
    await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, 'backup-password');
        input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await click(host.querySelector<HTMLButtonElement>(`[data-testid="import-existing-${mode}"]`)!);
    await click(button(b => b.textContent === 'settings.importServers'));
}

describe('ExportImportDialog .aeroftp import', () => {
    it('sends the chosen mode, hands the host the list stored and keeps the dialog open on a Jottacloud chain', async () => {
        vi.mocked(invoke).mockResolvedValue({
            servers: [
                { id: 'srv_a', name: 'Restored', host: 'a.example', port: 21, username: 'u', protocol: 'ftp',
                    hasStoredCredential: true, importOutcome: 'overwritten' },
                { id: 'srv_copy', name: 'Jotta', host: '', port: 443, username: 'u', protocol: 'jottacloud',
                    publicUrlBase: 'https://share.example', hasStoredFilenApiKey: true,
                    hasStoredCredential: false, importOutcome: 'duplicated' },
            ],
            skipped: [],
            similar: 0,
            jottaChains: [{ id: 'srv_copy', name: 'Jotta' }],
            oauthAppsKept: ['Drive copy'],
            sharedSecretsFailed: ['Proton'],
            targetChanged: ['Moved'],
            stored: [{ id: 'srv_a', name: 'Restored' }, { id: 'srv_copy', name: 'Jotta (1)' }],
            totalInFile: 2,
            metadata: { exportDate: '', aeroftpVersion: '', serverCount: 2, hasCredentials: true },
        });
        const onProfilesStored = vi.fn();
        const onClose = vi.fn();
        await openImport(onProfilesStored, onClose);
        await runImport('overwrite');

        expect(invoke).toHaveBeenCalledWith('import_server_profiles', {
            filePath: '/backups/servers.aeroftp',
            password: 'backup-password',
            existing: 'overwrite',
        });
        // The backend stored it: the host only shows the list it returned.
        expect(onProfilesStored).toHaveBeenCalledWith([{ id: 'srv_a', name: 'Restored' }, { id: 'srv_copy', name: 'Jotta (1)' }]);
        expect(host.textContent).toContain('settings.overwrittenCount {"count":1}');
        expect(host.textContent).toContain('settings.duplicatedCount {"count":1}');
        expect(host.querySelector('[data-testid="import-warning"]')?.textContent)
            .toContain('settings.importJottaChainWarning {"names":"Jotta"}');
        expect(host.querySelector('[data-testid="import-warning"]')?.textContent)
            .toContain('settings.importOauthAppKept {"names":"Drive copy"}');
        expect(host.querySelector('[data-testid="import-warning"]')?.textContent)
            .toContain('settings.importTargetChanged {"names":"Moved"}');
        expect(host.querySelector('[data-testid="import-warning"]')?.textContent)
            .toContain('settings.importSharedSecretsFailed {"names":"Proton"}');
        // Enter in the password field cannot import (and duplicate) again.
        const field = host.querySelector<HTMLInputElement>('input[type="password"]')!;
        await act(async () => {
            field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
        });
        expect(invoke).toHaveBeenCalledTimes(1);
        await act(async () => {
            vi.advanceTimersByTime(5000);
        });
        expect(onClose).not.toHaveBeenCalled();
    });

    it('does not hand the host the list of an account switched away from during the import', async () => {
        vi.mocked(invoke).mockImplementation(async () => {
            accountChanges += 1;
            return {
                servers: [{ id: 'srv_a', name: 'A', host: 'a.example', port: 21, username: 'u', importOutcome: 'added' }],
                stored: [{ id: 'srv_a', name: 'A' }],
                skipped: [], similar: 0, jottaChains: [], oauthAppsKept: [], sharedSecretsFailed: [], targetChanged: [],
                totalInFile: 1, metadata: { exportDate: '', aeroftpVersion: '', serverCount: 1, hasCredentials: false },
            };
        });
        const events: ProfilesChangedDetail[] = [];
        const listen = (e: Event) => events.push((e as CustomEvent<ProfilesChangedDetail>).detail);
        window.addEventListener(PROFILES_CHANGED_EVENT, listen);
        const onProfilesStored = vi.fn();
        await openImport(onProfilesStored, vi.fn());
        await runImport('skip');
        window.removeEventListener(PROFILES_CHANGED_EVENT, listen);
        expect(onProfilesStored).not.toHaveBeenCalled();
        expect(events).toEqual([{ connectionMetadata: false }]);
    });

    it('shows the error of a failed import, stores nothing and lets the import run again', async () => {
        vi.mocked(invoke).mockRejectedValue('PROFILE_LIST_BUSY: another import is writing');
        const onProfilesStored = vi.fn();
        const onClose = vi.fn();
        await openImport(onProfilesStored, onClose);
        await runImport('overwrite');
        expect(host.textContent).toContain('PROFILE_LIST_BUSY');
        expect(onProfilesStored).not.toHaveBeenCalled();
        expect(button(b => b.textContent === 'settings.importServers').disabled).toBe(false);
        await act(async () => {
            vi.advanceTimersByTime(5000);
        });
        expect(onClose).not.toHaveBeenCalled();
    });

    // A second Enter while the first import is still running starts nothing.
    it('runs one import when submitted again while it is in flight', async () => {
        let finish: (value: unknown) => void = () => undefined;
        vi.mocked(invoke).mockImplementation(() => new Promise(resolve => { finish = resolve; }));
        await openImport(vi.fn(), vi.fn());
        const field = host.querySelector<HTMLInputElement>('input[type="password"]')!;
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(field, 'backup-password');
            field.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await click(button(b => b.textContent === 'settings.importServers'));
        await act(async () => {
            field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
        });
        expect(invoke).toHaveBeenCalledTimes(1);
        await act(async () => {
            finish({ servers: [], skipped: [], similar: 0, jottaChains: [], oauthAppsKept: [], targetChanged: [], secretsStored: 0, userId: null });
        });
    });

    it('defaults to skip, reports the skipped profiles and closes', async () => {
        vi.mocked(invoke).mockResolvedValue({
            servers: [{ id: 'srv_new', name: 'New', host: 'n.example', port: 22, username: 'u', importOutcome: 'added' }],
            skipped: ['Kept'],
            similar: 0,
            jottaChains: [],
            oauthAppsKept: [],
            sharedSecretsFailed: [],
            stored: [],
            targetChanged: [],
            totalInFile: 2,
            metadata: { exportDate: '', aeroftpVersion: '', serverCount: 2, hasCredentials: false },
        });
        const onImport = vi.fn();
        const onClose = vi.fn();
        await openImport(onImport, onClose);
        expect(host.querySelector('[data-testid="import-existing-skip"]')?.getAttribute('aria-checked')).toBe('true');
        await runImport('skip');
        expect(vi.mocked(invoke).mock.calls[0][1]).toMatchObject({ existing: 'skip' });
        expect(host.textContent).toContain('settings.duplicatesSkippedCount {"count":1}');
        expect(host.querySelector('[data-testid="import-warning"]')).toBeNull();
        // Done: neither a second click nor Enter imports the same file again.
        expect(button(b => b.textContent === 'settings.importServers').disabled).toBe(true);
        const field = host.querySelector<HTMLInputElement>('input[type="password"]')!;
        await act(async () => {
            field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
        });
        expect(invoke).toHaveBeenCalledTimes(1);
        await act(async () => {
            vi.advanceTimersByTime(3000);
        });
        expect(onClose).toHaveBeenCalled();
    });
});
