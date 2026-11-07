// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import { describe, expect, it } from 'vitest';
import app from '../App.tsx?raw';
import { loadedCertificateCheck, loadedOption, loadedOptionOn } from './profileSecurity';
import { scpFallbackConnectParam } from './scpFallback';

const buildStart = app.indexOf('const buildProviderParams =');
const start = app.indexOf('const providerParams = {', buildStart);
const end = app.indexOf('\n    };', start);
if (buildStart < 0 || start < 0 || end < 0) throw new Error('Missing provider connection payload');
const { code } = await transformWithOxc(`${app.slice(start, end + 7)}\nproviderParams;`, 'provider-payload.ts');

const payload = (protocol: string, options: Record<string, unknown>) => {
    const context = {
        protocol,
        effectiveParams: {
            providerId: 'custom', savedServerId: 'profile', server: 'dav.example.test', port: 80,
            username: 'user', password: 'secret', options,
        },
        initialPath: null,
        s3Location: { endpoint: options.endpoint ?? null, signingRegion: 'us-east-1' },
        filenApiKey: null,
        scpFallbackConnectParam,
        loadedCertificateCheck,
        loadedOption,
        loadedOptionOn,
    };
    return new Function(...Object.keys(context), `${code}\nreturn providerParams;`)(...Object.values(context)) as Record<string, unknown>;
};

describe('security options in the normal GUI connection payload', () => {
    it('honors WebDAV scheme aliases and effective certificate state', () => {
        expect(payload('webdav', { tls_mode: 'https', verify_cert: false })).toMatchObject({
            tls_mode: 'https', verify_cert: false,
        });
        expect(payload('webdav', { webdavScheme: 'https', verifyCert: 'false' }).verify_cert).toBe(false);
    });

    it('forwards retained S3 cleartext consent', () => {
        expect(payload('s3', { allowCleartextEndpoint: true }).allow_cleartext_endpoint).toBe(true);
        expect(payload('s3', { allow_cleartext_endpoint: true }).allow_cleartext_endpoint).toBe(true);
    });

    it('uses the same effective certificate state in the indicator and activity warning', () => {
        expect(app).toContain('const verifyCert = loadedCertificateCheck(opts);');
        expect(app).toContain('!loadedCertificateCheck(liveConnectionParams.options)');
    });

    it('reports security resets through both file-panel restore outcomes', () => {
        const start = app.indexOf('const importAeroFtpKeystoreFile =');
        const end = app.indexOf('const handleOpenArchiveFromFile =', start);
        const restore = app.slice(start, end);
        expect(start).toBeGreaterThanOrEqual(0);
        expect(end).toBeGreaterThan(start);
        expect(restore).toMatch(/!!result\.securityReset\?\.length/);
        expect(restore).toMatch(/securityReset: result\.securityReset/);
        expect(restore).toMatch(/keystoreImportSummary\(result, null, t\)\.text/);
    });
});
