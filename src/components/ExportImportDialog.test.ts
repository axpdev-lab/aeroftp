// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ServerProfile } from '../types';

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
async function openImport(onImport: (s: ServerProfile[]) => void, onClose: () => void) {
    await act(async () => {
        root.render(createElement(ExportImportDialog, { servers: [], initialMode: 'import', onImport, onClose }));
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
    it('sends the chosen mode, passes every returned field through and keeps the dialog open on a Jottacloud chain', async () => {
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
            totalInFile: 2,
            metadata: { exportDate: '', aeroftpVersion: '', serverCount: 2, hasCredentials: true },
        });
        const onImport = vi.fn();
        const onClose = vi.fn();
        await openImport(onImport, onClose);
        await runImport('overwrite');

        expect(invoke).toHaveBeenCalledWith('import_server_profiles', {
            filePath: '/backups/servers.aeroftp',
            password: 'backup-password',
            existing: 'overwrite',
        });
        const stored = onImport.mock.calls[0][0] as (ServerProfile & Record<string, unknown>)[];
        expect([...onImport.mock.calls[0][1] as Set<string>]).toEqual(['srv_a']);
        expect(stored.map(s => s.id)).toEqual(['srv_a', 'srv_copy']);
        expect(stored[1].publicUrlBase).toBe('https://share.example');
        expect(stored[1].hasStoredFilenApiKey).toBe(true);
        expect(stored.every(s => !('importOutcome' in s))).toBe(true);
        expect(host.textContent).toContain('settings.overwrittenCount {"count":1}');
        expect(host.textContent).toContain('settings.duplicatedCount {"count":1}');
        expect(host.querySelector('[data-testid="import-jotta-warning"]')?.textContent)
            .toContain('settings.importJottaChainWarning {"names":"Jotta"}');
        await act(async () => {
            vi.advanceTimersByTime(5000);
        });
        expect(onClose).not.toHaveBeenCalled();
    });

    it('says what a failed write leaves for every imported profile, and stays open', async () => {
        vi.mocked(invoke).mockResolvedValue({
            servers: [
                { id: 'srv_a', name: 'A', host: 'a.example', port: 21, username: 'u', importOutcome: 'overwritten' },
                { id: 'srv_b', name: 'B', host: 'b.example', port: 21, username: 'u', importOutcome: 'added' },
                { id: 'srv_c', name: 'C', host: 'c.example', port: 21, username: 'u', importOutcome: 'added' },
            ],
            skipped: [],
            similar: 0,
            secretsStored: 2,
            jottaChains: [],
            totalInFile: 1,
            metadata: { exportDate: '', aeroftpVersion: '', serverCount: 1, hasCredentials: true },
        });
        const onImport = vi.fn(async () => { throw new Error('vault write refused'); });
        const onClose = vi.fn();
        await openImport(onImport, onClose);
        await runImport('overwrite');
        expect(host.textContent).toContain('vault write refused');
        expect(host.textContent).toContain('settings.importNotSavedSecretsKept {"count":2}');
        await act(async () => {
            vi.advanceTimersByTime(5000);
        });
        expect(onClose).not.toHaveBeenCalled();
    });

    it('defaults to skip, reports the skipped profiles and closes', async () => {
        vi.mocked(invoke).mockResolvedValue({
            servers: [{ id: 'srv_new', name: 'New', host: 'n.example', port: 22, username: 'u', importOutcome: 'added' }],
            skipped: ['Kept'],
            similar: 0,
            jottaChains: [],
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
        expect(host.querySelector('[data-testid="import-jotta-warning"]')).toBeNull();
        // Done: a second click cannot import the same file again.
        expect(button(b => b.textContent === 'settings.importServers').disabled).toBe(true);
        await act(async () => {
            vi.advanceTimersByTime(3000);
        });
        expect(onClose).toHaveBeenCalled();
    });
});
