// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { migrateLegacySettings, updateAppSettings } from './appSettings';

const backend = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: backend.invoke }));

const LEGACY = 'aeroftp_settings';
const stores = () => backend.invoke.mock.calls.filter(([command]) => command === 'store_credential');

let setItem: ReturnType<typeof vi.spyOn>;
beforeEach(() => {
    localStorage.clear();
    backend.invoke.mockReset();
    setItem = vi.spyOn(Storage.prototype, 'setItem');
});
afterEach(() => setItem.mockRestore());

// The settings live in the vault only: no write path leaves a plaintext copy,
// and the copy an older version left is removed once the vault holds them.
describe('confirmed app-settings persistence', () => {
    it('keeps the legacy copy on a vault failure and removes it after the next confirmed write', async () => {
        const original = { theme: 'dark', ui_settings: { my_servers_breakdown: false } };
        localStorage.setItem(LEGACY, JSON.stringify(original));
        setItem.mockClear();
        backend.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_credential') throw new Error('Credential not found: config_app_settings');
            throw new Error('vault write failed');
        });
        await expect(updateAppSettings(existing => ({ ...existing, theme: 'light' }))).rejects.toThrow('vault write failed');
        expect(JSON.parse(localStorage.getItem(LEGACY)!)).toEqual(original);

        backend.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_credential') throw new Error('Credential not found: config_app_settings');
        });
        const saved = await updateAppSettings(existing => ({ ...existing, theme: 'light' }));
        expect(saved).toEqual({ ...original, theme: 'light' });
        expect(JSON.parse(stores()[1][1].password)).toEqual({ ...original, theme: 'light' });
        expect(localStorage.getItem(LEGACY)).toBeNull();
        expect(setItem).not.toHaveBeenCalled();
    });

    it('merges overlapping updates after vault acknowledgement and writes no plaintext copy', async () => {
        let vault = JSON.stringify({ theme: 'dark' });
        let release!: () => void;
        const gate = new Promise<void>(r => { release = r; });
        let count = 0;
        backend.invoke.mockImplementation(async (command: string, args: { account: string; password: string }) => {
            expect(args.account).toBe('config_app_settings');
            if (command === 'get_credential') return vault;
            if (++count === 1) await gate;
            vault = args.password;
        });
        const first = updateAppSettings(existing => ({ ...existing, breakdown: true }));
        const second = updateAppSettings(existing => ({ ...existing, hostVisible: false }));
        await vi.waitFor(() => expect(count).toBe(1));
        release();
        await Promise.all([first, second]);
        expect(JSON.parse(vault)).toEqual({ theme: 'dark', breakdown: true, hostVisible: false });
        expect(localStorage.getItem(LEGACY)).toBeNull();
        expect(setItem).not.toHaveBeenCalled();
    });

    it('waits for a vault that is still starting instead of writing only the changed key', async () => {
        let reads = 0;
        backend.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_credential') {
                if (++reads <= 2) throw new Error('STORE_NOT_READY');
                return JSON.stringify({ theme: 'dark', fontSize: 18 });
            }
        });
        await updateAppSettings(existing => ({ ...existing, lastLocalPath: '/data' }));
        expect(stores()).toHaveLength(1);
        expect(JSON.parse(stores()[0][1].password)).toEqual({ theme: 'dark', fontSize: 18, lastLocalPath: '/data' });
    });

    it('rejects a write when the vault cannot be read, instead of replacing every setting', async () => {
        backend.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_credential') throw new Error('Failed to get credential: disk I/O error');
        });
        await expect(updateAppSettings(existing => ({ ...existing, lastLocalPath: '/data' }))).rejects.toThrow('CONFIG_UNAVAILABLE');
        expect(stores()).toEqual([]);
    });
});

describe('legacy settings migration', () => {
    it('writes nothing when the vault already holds the settings, and removes the plaintext copy', async () => {
        localStorage.setItem(LEGACY, JSON.stringify({ fontSize: 15 }));
        backend.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_credential') return JSON.stringify({ fontSize: 18 });
        });
        await migrateLegacySettings({ fontSize: 15 });
        expect(stores()).toEqual([]);
        expect(localStorage.getItem(LEGACY)).toBeNull();
    });

    it('moves the settings into a vault that holds none, then removes the plaintext copy', async () => {
        localStorage.setItem(LEGACY, JSON.stringify({ fontSize: 15 }));
        backend.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_credential') throw new Error('Credential not found: config_app_settings');
        });
        await migrateLegacySettings({ fontSize: 15 });
        expect(stores()).toHaveLength(1);
        expect(JSON.parse(stores()[0][1].password)).toEqual({ fontSize: 15 });
        expect(localStorage.getItem(LEGACY)).toBeNull();
    });

    it('rejects while the vault is not ready, writing nothing and keeping the copy for the next try', async () => {
        localStorage.setItem(LEGACY, JSON.stringify({ fontSize: 15 }));
        backend.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_credential') throw new Error('STORE_NOT_READY');
        });
        await expect(migrateLegacySettings({ fontSize: 15 })).rejects.toThrow('CONFIG_UNAVAILABLE');
        expect(stores()).toEqual([]);
        expect(JSON.parse(localStorage.getItem(LEGACY)!)).toEqual({ fontSize: 15 });
    });
});
