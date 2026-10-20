// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import type { ConnectionParams } from '../types';

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), account: 0 }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('../utils/userPartitions', () => ({ accountChangeGeneration: () => mocks.account }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
import { useProtonSignIn } from './useProtonSignIn';

function deferred<T>() {
    let resolve!: (value: T) => void;
    let reject!: (reason: unknown) => void;
    const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}

let host: HTMLDivElement;
let root: Root;
let current: ReturnType<typeof useProtonSignIn>;
const changed = vi.fn();
type Props = Parameters<typeof useProtonSignIn>[0];
const params: ConnectionParams = { protocol: 'proton', server: '', username: '', password: '', options: { proton_mode: 'native' } };
function Harness(props: Props) { current = useProtonSignIn(props); return null; }
async function render(over: Partial<Props> = {}) {
    await act(async () => root.render(createElement(Harness, {
        active: true, nativeForm: true, available: true, profileId: 'profile-a', params,
        onParamsChange: changed, ...over,
    })));
}
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    mocks.account = 0;
    changed.mockReset();
    let nextToken = 0;
    vi.stubGlobal('crypto', { randomUUID: () => `synthetic-token-${++nextToken}` });
    mocks.invoke.mockReset().mockImplementation((command: string) => {
        if (command === 'proton_native_status') return Promise.resolve({ signedIn: false });
        return Promise.resolve(undefined);
    });
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); vi.unstubAllGlobals(); });

it.each([
    ['mode', { nativeForm: false }], ['profile', { profileId: 'profile-b' }], ['close', { active: false }],
] as const)('retires a browser result after a %s change', async (_label, change) => {
    const pending = deferred<{ email: string }>();
    mocks.invoke.mockImplementation((command: string) => command === 'proton_native_sign_in'
        ? pending.promise : Promise.resolve({ signedIn: false }));
    await render();
    let job!: Promise<void>;
    await act(async () => { job = current.signIn(); });
    await render(change);
    await act(async () => { pending.resolve({ email: 'old@synthetic.invalid' }); await job; });
    expect(changed).not.toHaveBeenCalled();
    expect(current.signedIn).toBe(false);
    expect(mocks.invoke).toHaveBeenCalledWith('cancel_connection', expect.objectContaining({ token: expect.any(String) }));
});

it('rejects an account change even before React rerenders and can start again in the new scope', async () => {
    const old = deferred<{ email: string }>();
    const next = deferred<{ email: string }>();
    let started = 0;
    mocks.invoke.mockImplementation((command: string) => command === 'proton_native_sign_in'
        ? (++started === 1 ? old.promise : next.promise) : Promise.resolve({ signedIn: false }));
    await render();
    let first!: Promise<void>;
    let second!: Promise<void>;
    await act(async () => { first = current.signIn(); });
    mocks.account += 1;
    // Old callbacks must not start a new browser flow before the account's
    // replacement form has committed.
    await current.signIn();
    expect(started).toBe(1);
    await act(async () => { old.resolve({ email: 'old@synthetic.invalid' }); await first; });
    expect(changed).not.toHaveBeenCalled();
    await render();
    await act(async () => { second = current.signIn(); });
    expect(current.signingIn).toBe(true);
    await act(async () => { next.resolve({ email: 'new@synthetic.invalid' }); await second; });
    expect(changed).toHaveBeenCalledTimes(1);
    expect(changed.mock.calls[0][0].username).toBe('new@synthetic.invalid');
});

it('cancels on unmount and ignores the browser result afterward', async () => {
    const login = deferred<{ email: string }>();
    mocks.invoke.mockImplementation((name: string) => name === 'proton_native_sign_in'
        ? login.promise : Promise.resolve({ signedIn: false }));
    await render();
    let job!: Promise<void>;
    await act(async () => { job = current.signIn(); });
    await act(async () => { root.render(null); });
    await act(async () => { login.resolve({ email: 'old@synthetic.invalid' }); await job; });
    expect(changed).not.toHaveBeenCalled();
    expect(mocks.invoke).toHaveBeenCalledWith('cancel_connection', expect.any(Object));
});

it('uses current form values and suppresses a stale status response after sign-in', async () => {
    const status = deferred<{ signedIn: boolean }>();
    const login = deferred<{ email: string }>();
    mocks.invoke.mockImplementation((command: string) => command === 'proton_native_sign_in'
        ? login.promise : command === 'proton_native_status' ? status.promise : Promise.resolve());
    await render();
    let job!: Promise<void>;
    await act(async () => { job = current.signIn(); void current.signIn(); });
    expect(mocks.invoke.mock.calls.filter(([name]) => name === 'proton_native_sign_in')).toHaveLength(1);
    await render({ params: { ...params, options: { proton_mode: 'native', manualTotalBytes: 123 } } });
    await act(async () => { login.resolve({ email: 'new@synthetic.invalid' }); await job; status.resolve({ signedIn: false }); });
    expect(changed.mock.calls[0][0].options.manualTotalBytes).toBe(123);
    expect(current.signedIn).toBe(true);
});

it('ignores a cancelled operation error and finally after another operation starts', async () => {
    const old = deferred<{ email: string }>();
    const next = deferred<{ email: string }>();
    let count = 0;
    mocks.invoke.mockImplementation((name: string) => name === 'proton_native_sign_in'
        ? (++count === 1 ? old.promise : next.promise) : Promise.resolve({ signedIn: false }));
    await render();
    let first!: Promise<void>;
    let second!: Promise<void>;
    await act(async () => { first = current.signIn(); current.cancel(); second = current.signIn(); });
    await act(async () => { old.reject('old error'); await first; });
    expect(current.error).toBeNull();
    expect(current.signingIn).toBe(true);
    await act(async () => { next.resolve({ email: 'new@synthetic.invalid' }); await second; });
});
