// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { OAuthConnect } from './OAuthConnect';

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), t: (key: string) => key }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('../i18n', () => ({ useTranslation: () => mocks.t, useI18n: () => ({ t: mocks.t }) }));
vi.mock('../hooks/useOAuth2', () => ({
    useOAuth2: () => ({ isAuthenticating: false, error: null, startAuth: vi.fn(), connect: vi.fn() }),
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
