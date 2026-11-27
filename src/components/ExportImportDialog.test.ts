// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ServerProfile } from '../types';

import { PROFILES_CHANGED_EVENT, SERVERS_IMPORTED_EVENT, type ProfilesChangedDetail } from '../utils/serverProfileStore';
import { IMPORT_SUCCESS_CLOSE_DELAY_MS } from './ImportSuccessOverlay';

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
vi.mock('./ConfigurationTemplatePanel', () => ({
    ConfigurationTemplatePanel: () => createElement('div', { 'data-testid': 'mock-template-panel' }),
}));

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

        // A preview first (nothing to review here), then the import.
        expect(invoke).toHaveBeenCalledWith('preview_server_import', {
            filePath: '/backups/servers.aeroftp',
            password: 'backup-password',
            existing: 'overwrite',
        });
        expect(invoke).toHaveBeenCalledWith('import_server_profiles', {
            filePath: '/backups/servers.aeroftp',
            password: 'backup-password',
            existing: 'overwrite',
            keepRelaxed: [],
            reviewedDigest: null,
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
        expect(invoke).toHaveBeenCalledTimes(2);
        await act(async () => {
            vi.advanceTimersByTime(5000);
        });
        expect(onClose).not.toHaveBeenCalled();
    });

    // #1506 Requested 198: options that turn a security check off are shown
    // before anything is stored, unticked, and only the ticked ones are kept.
    it('reviews the security-relaxing options before importing and keeps only the ticked ones', async () => {
        const report = {
            servers: [{ id: 'srv_a', name: 'Lab', host: 'files.example', port: 21, username: 'u', importOutcome: 'added' }],
            stored: [{ id: 'srv_a', name: 'Lab' }],
            skipped: [], similar: 0, jottaChains: [], oauthAppsKept: [], sharedSecretsFailed: [], targetChanged: [],
            totalInFile: 1, metadata: { exportDate: '', aeroftpVersion: '', serverCount: 1, hasCredentials: false },
            fileDigest: 'digest-of-the-reviewed-file',
            security: [{
                fileIndex: 0,
                name: 'Lab',
                reset: [{ key: 'verifyCert', value: 'false' }, { key: 'trustPasvAddress', value: 'true' }],
                kept: [],
            }],
        };
        vi.mocked(invoke).mockResolvedValue(report);
        await openImport(vi.fn(), vi.fn());
        await runImport('skip');
        expect(invoke).toHaveBeenCalledTimes(1);
        expect(vi.mocked(invoke).mock.calls[0][0]).toBe('preview_server_import');
        expect(host.querySelector('[data-testid="import-security-review"]')?.textContent)
            .toContain('settings.importSecurityPasvAddress');
        const keep = host.querySelector<HTMLInputElement>('[data-testid="import-security-keep-0:trustPasvAddress"]')!;
        expect(keep.checked).toBe(false);
        await click(keep);
        await click(button(b => b.textContent === 'settings.importServers'));
        expect(invoke).toHaveBeenLastCalledWith('import_server_profiles', {
            filePath: '/backups/servers.aeroftp',
            password: 'backup-password',
            existing: 'skip',
            keepRelaxed: [{ fileIndex: 0, key: 'trustPasvAddress' }],
            reviewedDigest: 'digest-of-the-reviewed-file',
        });
        // Another password is another file for the review: it is gone.
        const field = host.querySelector<HTMLInputElement>('input[type="password"]')!;
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(field, 'another-password');
            field.dispatchEvent(new Event('input', { bubbles: true }));
        });
        expect(host.querySelector('[data-testid="import-security-review"]')).toBeNull();
    });

    // Greptile on #1524: a file replaced since its review is refused, the
    // review is dropped, and the next Import previews the file again.
    it('drops the review when the file changed since it', async () => {
        const report = {
            servers: [], stored: [], skipped: [], similar: 0, jottaChains: [], oauthAppsKept: [],
            sharedSecretsFailed: [], targetChanged: [], totalInFile: 1,
            metadata: { exportDate: '', aeroftpVersion: '', serverCount: 1, hasCredentials: false },
            fileDigest: 'digest-a',
            security: [{ fileIndex: 0, name: 'Lab', reset: [{ key: 'verifyCert', value: 'false' }], kept: [] }],
        };
        vi.mocked(invoke).mockImplementation(async (command: string) => {
            if (command === 'import_server_profiles') throw 'IMPORT_FILE_CHANGED: The backup file changed since its review';
            return report;
        });
        await openImport(vi.fn(), vi.fn());
        await runImport('skip');
        expect(host.querySelector('[data-testid="import-security-review"]')).not.toBeNull();
        await click(button(b => b.textContent === 'settings.importServers'));
        expect(vi.mocked(invoke).mock.lastCall?.[0]).toBe('import_server_profiles');
        expect(host.querySelector('[data-testid="import-security-review"]')).toBeNull();
        expect(host.textContent).toContain('IMPORT_FILE_CHANGED');
        await click(button(b => b.textContent === 'settings.importServers'));
        expect(vi.mocked(invoke).mock.lastCall?.[0]).toBe('preview_server_import');
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
        expect(vi.mocked(invoke).mock.calls[1][1]).toMatchObject({ existing: 'skip', keepRelaxed: [] });
        expect(host.textContent).toContain('settings.duplicatesSkippedCount {"count":1}');
        expect(host.querySelector('[data-testid="import-warning"]')).toBeNull();
        // Done: neither a second click nor Enter imports the same file again.
        expect(button(b => b.textContent === 'settings.importServers').disabled).toBe(true);
        const field = host.querySelector<HTMLInputElement>('input[type="password"]')!;
        await act(async () => {
            field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
        });
        expect(invoke).toHaveBeenCalledTimes(2);
        await act(async () => {
            vi.advanceTimersByTime(3000);
        });
        expect(onClose).toHaveBeenCalled();
    });

    it('opens the Configuration template view from the start-screen card', async () => {
        await act(async () => {
            root.render(createElement(ExportImportDialog, {
                servers: [],
                onImport: vi.fn(),
                onProfilesStored: vi.fn(),
                onClose: vi.fn(),
            }));
        });
        const card = host.querySelector<HTMLButtonElement>('[data-testid="export-import-template-card"]')!;
        expect(card).not.toBeNull();
        await click(card);
        expect(host.querySelector('[data-testid="mock-template-panel"]')).not.toBeNull();
        expect(host.textContent).toContain('settings.templateTitle');
    });
});

describe('ExportImportDialog import success overlay and list refresh', () => {
    const cleanImportResult = {
        servers: [{ id: 'srv_new', name: 'New', host: 'n.example', port: 22, username: 'u', importOutcome: 'added' }],
        skipped: ['Kept'],
        similar: 0,
        jottaChains: [],
        oauthAppsKept: [],
        sharedSecretsFailed: [],
        targetChanged: [],
        stored: [{ id: 'srv_new', name: 'New' }],
        totalInFile: 2,
        metadata: { exportDate: '', aeroftpVersion: '', serverCount: 2, hasCredentials: false },
    };

    it('announces the import, shows the counts overlay and closes after the short pause', async () => {
        vi.mocked(invoke).mockResolvedValue(cleanImportResult);
        const importedEvents: Event[] = [];
        const listen = (e: Event) => importedEvents.push(e);
        window.addEventListener(SERVERS_IMPORTED_EVENT, listen);
        const onClose = vi.fn();
        await openImport(vi.fn(), onClose);
        await runImport('skip');
        window.removeEventListener(SERVERS_IMPORTED_EVENT, listen);

        // The My Servers list is told an import landed, so it can drop its
        // active filter and show the new profiles without clicking "All".
        expect(importedEvents.length).toBe(1);
        // The overlay shows the real counts with the existing i18n keys.
        expect(host.querySelector('[data-testid="import-success-overlay"]')).not.toBeNull();
        expect(host.querySelector('[data-testid="import-success-check"]')).not.toBeNull();
        const summary = host.querySelector('[data-testid="import-success-summary"]')?.textContent ?? '';
        expect(summary).toContain('settings.importSuccess {"count":1}');
        expect(summary).toContain('settings.bridgeSkippedCount {"count":1}');
        expect(summary).not.toContain('overwrittenCount');
        // The dialog closes only after the pause, not instantly.
        await act(async () => { vi.advanceTimersByTime(IMPORT_SUCCESS_CLOSE_DELAY_MS - 1); });
        expect(onClose).not.toHaveBeenCalled();
        await act(async () => { vi.advanceTimersByTime(1); });
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('a click on the overlay closes the dialog at once, and only once', async () => {
        vi.mocked(invoke).mockResolvedValue(cleanImportResult);
        const onClose = vi.fn();
        await openImport(vi.fn(), onClose);
        await runImport('skip');
        await click(host.querySelector<HTMLElement>('[data-testid="import-success-overlay"]')!);
        expect(onClose).toHaveBeenCalledTimes(1);
        await act(async () => { vi.advanceTimersByTime(5000); });
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('Escape on the overlay closes the dialog at once', async () => {
        vi.mocked(invoke).mockResolvedValue(cleanImportResult);
        const onClose = vi.fn();
        await openImport(vi.fn(), onClose);
        await runImport('skip');
        await act(async () => {
            window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
        });
        expect(onClose).toHaveBeenCalledTimes(1);
    });

    it('a warned import keeps the dialog open and shows no overlay', async () => {
        vi.mocked(invoke).mockResolvedValue({
            ...cleanImportResult,
            jottaChains: [{ id: 'srv_new', name: 'New' }],
        });
        const onClose = vi.fn();
        await openImport(vi.fn(), onClose);
        await runImport('skip');
        expect(host.querySelector('[data-testid="import-success-overlay"]')).toBeNull();
        expect(host.querySelector('[data-testid="import-warning"]')).not.toBeNull();
        await act(async () => { vi.advanceTimersByTime(5000); });
        expect(onClose).not.toHaveBeenCalled();
    });
});

describe('ExportImportDialog export account binding', () => {
    const profile = { id: 'srv_a', name: 'A', host: 'h', port: 21, username: 'u', protocol: 'ftp' } as ServerProfile;

    const openExport = async (accountId?: number | null) => {
        await act(async () => {
            root.render(createElement(ExportImportDialog, {
                servers: [profile],
                accountId,
                initialMode: 'export',
                onImport: vi.fn(),
                onProfilesStored: vi.fn(),
                onClose: vi.fn(),
            }));
        });
    };

    const typePasswords = async () => {
        const inputs = [...host.querySelectorAll<HTMLInputElement>('input[type="password"]')];
        expect(inputs.length).toBe(2);
        await act(async () => {
            for (const input of inputs) {
                Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, 'export-password-1');
                input.dispatchEvent(new Event('input', { bubbles: true }));
            }
        });
    };

    const exportButton = () =>
        [...host.querySelectorAll('button')].find(b =>
            (b.textContent ?? '').includes('settings.exportServers') && (b.textContent ?? '').includes('('));

    const unlockStatus = (activeUserId: number | null | undefined) =>
        vi.mocked(invoke).mockImplementation(async (command: string) => {
            if (command === 'user_partitions_unlock_status') return { activeUserId };
            if (command === 'export_server_profiles') return {};
            throw new Error(`unexpected invoke ${command}`);
        });

    it('refuses the export when the active account is not the list account', async () => {
        unlockStatus(9);
        const { pickSave } = await import('../utils/pickPath');
        await openExport(7);
        await typePasswords();
        await click(exportButton()!);
        expect(vi.mocked(invoke)).not.toHaveBeenCalledWith('export_server_profiles', expect.anything());
        expect(host.textContent).toContain('settings.exportAccountChanged');
        expect(pickSave).not.toHaveBeenCalled();
    });

    it('refuses the export when the list has no account binding', async () => {
        unlockStatus(7);
        await openExport(undefined);
        await typePasswords();
        await click(exportButton()!);
        expect(vi.mocked(invoke)).not.toHaveBeenCalledWith('export_server_profiles', expect.anything());
        expect(host.textContent).toContain('settings.exportAccountUnbound');
    });

    it('exports with the list account id when it still matches the active one', async () => {
        unlockStatus(7);
        const { pickSave } = await import('../utils/pickPath');
        vi.mocked(pickSave).mockResolvedValue('/tmp/out.aeroftp');
        await openExport(7);
        await typePasswords();
        await click(exportButton()!);
        await act(async () => { await Promise.resolve(); });
        expect(vi.mocked(invoke)).toHaveBeenCalledWith('export_server_profiles', expect.objectContaining({ accountId: 7 }));
    });

    it('exports with a null account id when both the list and the active account are unbound', async () => {
        unlockStatus(undefined);
        const { pickSave } = await import('../utils/pickPath');
        vi.mocked(pickSave).mockResolvedValue('/tmp/out.aeroftp');
        await openExport(null);
        await typePasswords();
        await click(exportButton()!);
        await act(async () => { await Promise.resolve(); });
        expect(vi.mocked(invoke)).toHaveBeenCalledWith('export_server_profiles', expect.objectContaining({ accountId: null }));
    });
});
