// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useLayoutEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../i18n';
import type { ConnectionParams } from '../types';
import { accountChangeGeneration } from '../utils/userPartitions';

interface Options {
    active: boolean;
    nativeForm: boolean;
    available: boolean;
    profileId: string | null;
    params: ConnectionParams;
    onParamsChange: (params: ConnectionParams) => void;
}

/** Owns a browser round trip only while its profile, mode and account remain open. */
export function useProtonSignIn(options: Options) {
    const t = useTranslation();
    const accountEpoch = accountChangeGeneration();
    const [signingIn, setSigningIn] = useState(false);
    const [signedIn, setSignedIn] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const latest = useRef(options);
    const scope = useRef<{ account: number } | null>(null);
    const pending = useRef<{ token: string; scope: object; account: number } | null>(null);
    const statusRead = useRef(0);
    useLayoutEffect(() => { latest.current = options; });

    const cancel = () => {
        const previous = pending.current;
        pending.current = null;
        if (previous) void invoke('cancel_connection', { token: previous.token }).catch(() => undefined);
        setSigningIn(false);
    };

    useLayoutEffect(() => {
        const identity = { account: accountChangeGeneration() };
        scope.current = identity;
        setSignedIn(false);
        setSigningIn(false);
        setError(null);
        const read = ++statusRead.current;
        const account = identity.account;
        if (options.active && options.nativeForm && options.available) {
            void invoke<{ signedIn: boolean }>('proton_native_status', { profileId: options.profileId })
                .then(status => {
                    if (scope.current === identity && read === statusRead.current && account === accountChangeGeneration()) {
                        setSignedIn(status.signedIn);
                    }
                }).catch(() => undefined);
        }
        return () => {
            scope.current = null;
            statusRead.current += 1;
            const previous = pending.current;
            pending.current = null;
            if (previous) void invoke('cancel_connection', { token: previous.token }).catch(() => undefined);
        };
    }, [options.active, options.nativeForm, options.available, options.profileId, accountEpoch]);

    const signIn = async () => {
        const form = latest.current;
        const identity = scope.current;
        const account = accountChangeGeneration();
        if (!identity || identity.account !== account || !form.active || !form.nativeForm || !form.available) return;
        if (pending.current?.account === account) return;
        cancel();
        const operation = { token: `proton_${crypto.randomUUID()}`, scope: identity, account };
        pending.current = operation;
        statusRead.current += 1;
        setError(null);
        setSigningIn(true);
        const current = () => pending.current === operation && scope.current === operation.scope
            && operation.account === accountChangeGeneration();
        try {
            const result = await invoke<{ email: string | null }>('proton_native_sign_in', {
                params: { profileId: form.profileId, connectToken: operation.token },
            });
            if (!current()) return;
            setSignedIn(true);
            if (result.email) {
                // Preserve edits made in this same form during the browser trip.
                const live = latest.current;
                live.onParamsChange({ ...live.params, username: result.email });
            }
        } catch (err) {
            if (!current()) return;
            const message = String(err);
            if (message.includes('PROTON_SIGN_IN_EXPIRED')) setError(t('connection.protonSignInExpired'));
            else if (!message.includes('CONNECT_CANCELLED')) setError(message);
        } finally {
            if (pending.current === operation) {
                pending.current = null;
                if (scope.current === operation.scope && operation.account === accountChangeGeneration()) setSigningIn(false);
            }
        }
    };

    const signOut = async () => {
        const identity = scope.current;
        const account = accountChangeGeneration();
        if (!identity || identity.account !== account) return;
        const read = ++statusRead.current;
        setError(null);
        try {
            await invoke('proton_native_sign_out', { profileId: latest.current.profileId });
            if (scope.current === identity && read === statusRead.current && account === accountChangeGeneration()) setSignedIn(false);
        } catch (err) {
            if (scope.current === identity && read === statusRead.current && account === accountChangeGeneration()) setError(String(err));
        }
    };

    return { signingIn, signedIn, error, signIn, signOut, cancel, clearError: () => setError(null) };
}
