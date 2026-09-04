// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { endpointNeedsCleartextConsent } from './registry';

describe('cleartext endpoint consent', () => {
    it('asks for consent on plain HTTP that reaches a network', () => {
        expect(endpointNeedsCleartextConsent('http://minio.example.com:9000')).toBe(true);
        expect(endpointNeedsCleartextConsent('http://192.168.1.10:9000')).toBe(true);
        expect(endpointNeedsCleartextConsent('http://10.0.0.5')).toBe(true);
    });

    it('does not ask on loopback, which never reaches a network', () => {
        expect(endpointNeedsCleartextConsent('http://localhost:9000')).toBe(false);
        expect(endpointNeedsCleartextConsent('http://127.0.0.1:9000')).toBe(false);
        expect(endpointNeedsCleartextConsent('http://127.5.6.7:8000/path')).toBe(false);
        expect(endpointNeedsCleartextConsent('http://[::1]:9000')).toBe(false);
    });

    it('does not ask on https, wherever it points', () => {
        expect(endpointNeedsCleartextConsent('https://s3.example.com')).toBe(false);
        expect(endpointNeedsCleartextConsent('https://127.0.0.1:1800')).toBe(false);
    });

    it('asks on an mDNS name, which is on a shared network segment', () => {
        // The boundary against the certificate-side predicate, which accepts
        // `.local` on purpose. Trusting a local bridge's self-signed certificate
        // and nobody being able to read the traffic are different questions.
        expect(endpointNeedsCleartextConsent('http://minio.local:9000')).toBe(true);
        expect(endpointNeedsCleartextConsent('http://nas.local')).toBe(true);
    });

    it('reads the host, not the path or the userinfo', () => {
        // A bucket or path segment named "localhost" must not disarm the check.
        expect(endpointNeedsCleartextConsent('http://s3.example.com/localhost')).toBe(true);
        expect(endpointNeedsCleartextConsent('http://user:pw@s3.example.com')).toBe(true);
        expect(endpointNeedsCleartextConsent('http://user@127.0.0.1:9000')).toBe(false);
    });

    it('says no when there is nothing to judge', () => {
        // An absent endpoint is AWS, which is https by construction.
        expect(endpointNeedsCleartextConsent(undefined)).toBe(false);
        expect(endpointNeedsCleartextConsent('')).toBe(false);
        expect(endpointNeedsCleartextConsent('   ')).toBe(false);
    });

    it('is case-insensitive on the scheme', () => {
        expect(endpointNeedsCleartextConsent('HTTP://minio.example.com')).toBe(true);
        expect(endpointNeedsCleartextConsent('HTTPS://minio.example.com')).toBe(false);
    });
});
