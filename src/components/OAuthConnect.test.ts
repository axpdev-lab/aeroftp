// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { OAuthConnect } from './OAuthConnect';

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), t: (key: string) => key, startAuth: vi.fn(), connect: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('../i18n', () => ({ useTranslation: () => mocks.t, useI18n: () => ({ t: mocks.t }) }));
vi.mock('../hooks/useOAuth2', () => ({
    useOAuth2: () => ({ isAuthenticating: false, error: null, startAuth: mocks.startAuth, connect: mocks.connect }),
    OAUTH_APPS: Object.fromEntries(['google_drive', 'googlephotos', 'dropbox', 'onedrive', 'box', 'pcloud', 'zoho_workdrive', 'yandexdisk'].map(p => [p, { help_url: 'https://example.test/help' }])),
}));

type Provider = React.ComponentProps<typeof OAuthConnect>['provider'];
let vault: Record<string, string>;
let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
    vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
    vault = {};
    mocks.invoke.mockReset();
    mocks.invoke.mockImplementation(async (command: string, args: { account?: string }) => {
        if (command === 'oauth2_redirect_uri') return 'http://127.0.0.1:9000/callback';
        const account = args.account!;
        if (!(account in vault)) throw new Error(`Failed to get credential: Credential not found: ${account}`);
        return vault[account];
    });
    container = document.createElement('div');
    document.body.append(container);
    root = createRoot(container);
});
afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
    vi.useRealTimers();
    vi.unstubAllGlobals();
});

async function render(provider: Provider) {
    await act(async () => root.render(React.createElement(OAuthConnect, { provider, onConnected: vi.fn(), isEditing: true })));
}
const field = (name: 'Id' | 'Secret') => container.querySelector<HTMLInputElement>(`input[placeholder="connection.oauth.enterClient${name}"]`)!;

describe.each<Provider>(['googledrive', 'googlephotos', 'dropbox', 'onedrive', 'box', 'pcloud', 'zohoworkdrive', 'yandexdisk'])('%s OAuth form', provider => {
    it('shows the backend callback with the appropriate registration guidance', async () => {
        const callback = provider === 'onedrive' || provider === 'pcloud' || provider === 'yandexdisk'
            ? 'http://localhost:49172/callback'
            : 'http://127.0.0.1:49172/callback';
        const previous = mocks.invoke.getMockImplementation()!;
        mocks.invoke.mockImplementation((command: string, args: { account?: string }) =>
            command === 'oauth2_redirect_uri' ? Promise.resolve(callback) : previous(command, args));
        await render(provider);
        expect(container.querySelector('code')?.textContent).toBe(callback);
        const isGoogle = provider === 'googledrive' || provider === 'googlephotos';
        expect(container.textContent).toContain(isGoogle
            ? 'connection.oauth.redirectUriNoteGoogle'
            : 'connection.oauth.redirectUriHelp');
        expect(container.textContent).not.toContain(isGoogle
            ? 'connection.oauth.redirectUriHelp'
            : 'connection.oauth.redirectUriNoteGoogle');
        const copyButton = container.querySelector<HTMLButtonElement>('button[title="common.copy"]')!;
        await act(async () => copyButton.click());
        expect(mocks.invoke).toHaveBeenCalledWith('copy_to_clipboard', { text: callback });
    });

    it.each(['modern', 'config_oauth_clients', 'config_aeroftp_oauth_settings'])('hydrates the imported %s app pair', async format => {
        const slug = provider === 'googlephotos' ? 'googledrive' : provider;
        vault = format === 'modern'
            ? { [`oauth_${slug}_client_id`]: 'fixture-id', [`oauth_${slug}_client_secret`]: 'fixture-secret' }
            : { [format]: JSON.stringify({ [slug]: { clientId: 'fixture-id', clientSecret: 'fixture-secret' } }) };
        await render(provider);
        expect(field('Id').value).toBe('fixture-id');
        expect(field('Secret').value).toBe('fixture-secret');
        expect(field('Secret').type).toBe('password');
        const signIn = [...container.querySelectorAll('button')].find(b => b.textContent?.includes('connection.oauth.signInWith'))!;
        expect(signIn.disabled).toBe(false);
    });
});

describe('OAuth vault loading lifetime', () => {
    it('retries a not-ready vault and populates the form once ready', async () => {
        vi.useFakeTimers();
        const previous = mocks.invoke.getMockImplementation()!;
        const attempts = new Set<string>();
        vault = { oauth_box_client_id: 'fixture-id', oauth_box_client_secret: 'fixture-secret' };
        mocks.invoke.mockImplementation((command: string, args: { account?: string }) => {
            if (command === 'get_credential' && !attempts.has(args.account!)) {
                attempts.add(args.account!);
                return Promise.reject(new Error('STORE_NOT_READY'));
            }
            return previous(command, args);
        });
        await render('box');
        await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
        expect(field('Id').value).toBe('fixture-id');
        expect(field('Secret').value).toBe('fixture-secret');
    });
    it('shows a read failure instead of pretending there are no app keys', async () => {
        mocks.invoke.mockRejectedValue(new Error('Vault I/O failure'));
        await render('box');
        expect(container.textContent).toContain('connection.oauthKeysReadFailed');
    });
    it('leaves missing credentials empty without a read-error alert', async () => {
        await render('box');
        expect(field('Id').value).toBe('');
        expect(container.textContent).not.toContain('connection.oauthKeysReadFailed');
    });
    it('does not overwrite manual app-key edits with a late vault response', async () => {
        let resolveId!: (value: string) => void;
        const pendingId = new Promise<string>(resolve => { resolveId = resolve; });
        const previous = mocks.invoke.getMockImplementation()!;
        vault.oauth_box_client_secret = 'stored-secret';
        mocks.invoke.mockImplementation((command: string, args: { account?: string }) => args.account === 'oauth_box_client_id' ? pendingId : previous(command, args));
        await render('box');
        await act(async () => {
            for (const [name, value] of [['Id', 'typed-id'], ['Secret', 'typed-secret']] as const) {
                const input = field(name);
                Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value);
                input.dispatchEvent(new Event('input', { bubbles: true }));
            }
            resolveId('stored-id');
        });
        expect(field('Id').value).toBe('typed-id');
        expect(field('Secret').value).toBe('typed-secret');
    });
    it('ignores a previous provider response after changing provider', async () => {
        let resolveId!: (value: string) => void;
        const pendingId = new Promise<string>(resolve => { resolveId = resolve; });
        const previous = mocks.invoke.getMockImplementation()!;
        vault = { oauth_box_client_secret: 'box-secret', oauth_pcloud_client_id: 'pcloud-id', oauth_pcloud_client_secret: 'pcloud-secret' };
        mocks.invoke.mockImplementation((command: string, args: { account?: string }) => args.account === 'oauth_box_client_id' ? pendingId : previous(command, args));
        await render('box');
        await render('pcloud');
        await act(async () => resolveId('box-id'));
        expect(field('Id').value).toBe('pcloud-id');
        expect(field('Secret').value).toBe('pcloud-secret');
    });
});

describe('saving the OAuth app before a sign-in', () => {
    const signInWith = async (saved: () => Promise<unknown>) => {
        const previous = mocks.invoke.getMockImplementation()!;
        mocks.invoke.mockImplementation((command: string, args: { account?: string }) =>
            command === 'store_oauth_app' ? saved() : previous(command, args));
        mocks.startAuth.mockReset();
        mocks.connect.mockReset();
        await render('dropbox');
        const type = (input: HTMLInputElement, value: string) => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value);
            input.dispatchEvent(new Event('input', { bubbles: true }));
        };
        await act(async () => {
            type(field('Id'), 'typed-id');
            type(field('Secret'), 'typed-secret');
        });
        const button = [...container.querySelectorAll('button')].find(b => b.className.includes('bg-blue-600') || b.textContent?.toLowerCase().includes('signin'));
        await act(async () => { button!.click(); });
    };

    it('stops the sign-in and shows why when the app could not be saved', async () => {
        await signInWith(async () => { throw new Error('PROFILE_LIST_BUSY'); });
        expect(mocks.startAuth).not.toHaveBeenCalled();
        expect(mocks.connect).not.toHaveBeenCalled();
        expect(container.textContent).toContain('PROFILE_LIST_BUSY');
    });

    it('goes on to the sign-in once the app is saved, as one call', async () => {
        await signInWith(async () => undefined);
        expect(mocks.invoke).toHaveBeenCalledWith('store_oauth_app', { provider: 'dropbox', clientId: 'typed-id', clientSecret: 'typed-secret' });
        expect(mocks.startAuth).toHaveBeenCalledTimes(1);
    });
});
