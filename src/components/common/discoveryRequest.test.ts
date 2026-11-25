// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { ConnectionParams } from '../../types';
import {
    discoveryRequestParams,
    discoveryRequestResetKey,
    type DiscoveryEditContext,
} from './discoveryRequest';

const BASE: ConnectionParams = {
    server: 's3.example',
    username: 'AKIAEXAMPLE',
    password: 'super-secret-key',
    port: 443,
    providerId: 'aws',
    options: {
        bucket: 'chosen-bucket',
        region: 'eu-central-1',
        endpoint: 'https://s3.eu-central-1.example',
        pathStyle: false,
        sessionToken: 'FwoGZXIvYXdzEXAMPLE',
        roleArn: 'arn:aws:iam::111111111111:role/first',
        roleExternalId: 'external-one',
        roleSessionName: 'aeroftp-session',
        roleDurationSeconds: 900,
        roleMfaSerial: 'arn:aws:iam::111111111111:mfa/device',
        roleMfaTokenCode: '123456',
    },
};

const withOption = (key: string, value: unknown): ConnectionParams => ({
    ...BASE,
    options: { ...BASE.options, [key]: value },
});

/**
 * One mutation per field the request carries, keyed by the wire name so the
 * coverage check below can compare the two lists directly. `edit` is the
 * edit-session context passed alongside the form values.
 */
const EDIT: DiscoveryEditContext = { profileId: 'srv_saved_1', hasStoredSessionToken: true };
const MUTATIONS: Record<string, { params: ConnectionParams; edit?: DiscoveryEditContext }> = {
    providerId: { params: { ...BASE, providerId: 'minio' } },
    server: { params: { ...BASE, server: 'other.example' } },
    port: { params: { ...BASE, port: 9000 } },
    username: { params: { ...BASE, username: 'AKIAOTHER' } },
    password: { params: { ...BASE, password: 'a-different-secret' } },
    profile_id: { params: BASE, edit: { profileId: 'srv_saved_2', hasStoredSessionToken: true } },
    has_stored_session_token: { params: BASE, edit: { profileId: 'srv_saved_1', hasStoredSessionToken: false } },
    region: { params: withOption('region', 'us-east-1') },
    endpoint: { params: withOption('endpoint', 'https://s3.us-east-1.example') },
    path_style: { params: withOption('pathStyle', true) },
    verify_cert: { params: withOption('verifyCert', false) },
    session_token: { params: withOption('sessionToken', 'FwoGZXIvYXdzOTHER') },
    role_arn: { params: withOption('roleArn', 'arn:aws:iam::222222222222:role/second') },
    role_external_id: { params: withOption('roleExternalId', 'external-two') },
    role_session_name: { params: withOption('roleSessionName', 'other-session') },
    role_duration_seconds: { params: withOption('roleDurationSeconds', 3600) },
    role_mfa_serial: { params: withOption('roleMfaSerial', 'arn:aws:iam::222222222222:mfa/device') },
    role_mfa_token_code: { params: withOption('roleMfaTokenCode', '654321') },
};

describe('discovery reset key', () => {
    it('covers every input the request actually sends', () => {
        // The defect this replaces: the request carried sixteen fields and the
        // key hashed three. This is the anti-drift check, so a field added to
        // the request without a mutation here fails instead of silently going
        // unwatched.
        const sent = Object.keys(discoveryRequestParams('s3', BASE, EDIT))
            .filter((field) => field !== 'protocol' && field !== 'bucket');
        expect(new Set(Object.keys(MUTATIONS))).toEqual(new Set(sent));
    });

    it('changes when any one of those inputs changes', () => {
        const base = discoveryRequestResetKey('s3', BASE, EDIT);
        for (const [field, mutation] of Object.entries(MUTATIONS)) {
            expect(
                discoveryRequestResetKey('s3', mutation.params, mutation.edit ?? EDIT),
                `changing ${field} leaves the previous account's targets selectable`,
            ).not.toBe(base);
        }
    });

    it('changing only the role ARN invalidates the listed targets', () => {
        // The case that needs no race at all: assume a different role and the
        // buckets of the previous one stay on screen, and stay selectable.
        expect(discoveryRequestResetKey('s3', MUTATIONS.role_arn.params))
            .not.toBe(discoveryRequestResetKey('s3', BASE));
    });

    it('does not reset on the bucket, which is the field being edited', () => {
        // Hashing the picker's own value would clear the list on every
        // keystroke, and on the auto-select that writes a single result back.
        expect(discoveryRequestResetKey('s3', withOption('bucket', 'another-bucket')))
            .toBe(discoveryRequestResetKey('s3', BASE));
    });

    it('is stable for an unchanged form and never carries a secret', () => {
        const key = discoveryRequestResetKey('s3', BASE);
        expect(discoveryRequestResetKey('s3', BASE)).toBe(key);
        expect(key).not.toContain('super-secret-key');
        expect(key).not.toContain('FwoGZXIvYXdz');
        expect(key).not.toContain('123456');
    });

    it('ignores the username on kdrive, where it is a fixed placeholder', () => {
        // kDrive authenticates with the token in the password field; the
        // username is always 'api-token' and identifies no account.
        expect(discoveryRequestParams('kdrive', BASE).username).toBe('api-token');
        expect(discoveryRequestResetKey('kdrive', MUTATIONS.username.params))
            .toBe(discoveryRequestResetKey('kdrive', BASE));
        expect(discoveryRequestResetKey('kdrive', MUTATIONS.password.params))
            .not.toBe(discoveryRequestResetKey('kdrive', BASE));
    });

    it('separates the protocols sharing the form', () => {
        expect(discoveryRequestResetKey('s3', BASE)).not.toBe(discoveryRequestResetKey('backblaze', BASE));
    });
});

describe('discovery for a saved STS profile being edited', () => {
    it('carries the edited profile id and its stored-token flag', () => {
        const request = discoveryRequestParams('s3', BASE, { profileId: 'srv_sts_1', hasStoredSessionToken: true });
        expect(request.profile_id).toBe('srv_sts_1');
        expect(request.has_stored_session_token).toBe(true);
    });

    it('prefers the connected saved server id over the edit context', () => {
        const request = discoveryRequestParams(
            's3',
            { ...BASE, savedServerId: 'srv_conn' },
            { profileId: 'srv_sts_1', hasStoredSessionToken: true },
        );
        expect(request.profile_id).toBe('srv_conn');
    });

    it('sends no profile id for an ad-hoc form', () => {
        const request = discoveryRequestParams('s3', BASE);
        expect(request.profile_id).toBeUndefined();
        expect(request.has_stored_session_token).toBeUndefined();
    });

    it('invalidates the listed targets when the edited profile changes', () => {
        expect(discoveryRequestResetKey('s3', BASE, { profileId: 'srv_a' }))
            .not.toBe(discoveryRequestResetKey('s3', BASE, { profileId: 'srv_b' }));
    });
});

describe('discovery on the Filen Desktop S3 preset (#958)', () => {
    const FILEN: ConnectionParams = {
        server: '',
        username: '',
        password: '',
        providerId: 'filen-desktop-s3',
        options: { endpoint: 'https://local.s3.filen.io:1800', region: 'filen', pathStyle: true, verifyCert: false },
    };

    it('lists buckets with the default keys the connect would use', () => {
        const request = discoveryRequestParams('s3', FILEN);
        expect(request.username).toBe('admin');
        expect(request.password).toBe('admin');
    });

    it('keeps keys the user typed', () => {
        const request = discoveryRequestParams('s3', { ...FILEN, username: 'mine', password: 'secret' });
        expect(request.username).toBe('mine');
        expect(request.password).toBe('secret');
    });

    it('does not verify the self-signed certificate of the local bridge', () => {
        expect(discoveryRequestParams('s3', FILEN).verify_cert).toBe(false);
    });

    it('leaves other presets without default keys and verifying certificates', () => {
        const request = discoveryRequestParams('s3', { ...BASE, username: '', password: '', options: { bucket: 'b' } });
        expect(request.username).toBe('');
        expect(request.password).toBe('');
        expect(request.verify_cert).toBe(true);
    });
});
