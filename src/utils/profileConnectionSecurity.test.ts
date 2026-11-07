// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import { describe, expect, it, vi } from 'vitest';
import app from '../App.tsx?raw';
import { loadedCertificateCheck, loadedOption, loadedOptionOn, makeSafe } from './profileSecurity';
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

    it('forwards retained S3 cleartext consent from import to connect', () => {
        expect(payload('s3', { allowCleartextEndpoint: true }).allow_cleartext_endpoint).toBe(true);
        expect(payload('s3', { allow_cleartext_endpoint: true }).allow_cleartext_endpoint).toBe(true);
        const imported = makeSafe({
            host: 'minio.example.test', protocol: 's3',
            options: { endpoint: 'http://minio.example.test:9000', allowCleartextEndpoint: true },
        }, ['allowCleartextEndpoint']).profile;
        expect(payload('s3', imported.options).allow_cleartext_endpoint).toBe(true);
    });

    it.each([
        ['verify_cert', false],
        ['verifyCert', 'false'],
    ] as const)('returns the warning indicator for %s', async (key, value) => {
        const start = app.indexOf('connectionSecurity={isConnected ?');
        const expressionStart = app.indexOf('isConnected ?', start);
        const end = app.indexOf('\n            secureProtocol=', expressionStart);
        const expression = app.slice(expressionStart, end).trim().replace(/\}$/, '');
        const { code } = await transformWithOxc(`const result = ${expression};`, 'connection-security.ts');
        for (const protocol of ['ftp', 'ftps']) {
            const result = new Function('isConnected', 'liveConnectionParams', 'loadedCertificateCheck', `${code}\nreturn result;`)(
                true,
                { protocol, options: { [key]: value } },
                loadedCertificateCheck,
            );
            expect(result).toBe('warning');
        }
    });

    it.each([
        ['verify_cert', false],
        ['verifyCert', 'false'],
    ] as const)('logs the activity warning for %s', async (key, value) => {
        const start = app.indexOf('useEffect(() => {\n    const protocol = liveConnectionParams.protocol;');
        const end = app.indexOf('\n  // Auto-Update', start);
        const { code } = await transformWithOxc(app.slice(start, end), 'certificate-activity.ts');
        let effect!: () => void;
        const logActivity = vi.fn();
        new Function(
            'useEffect', 'liveConnectionParams', 'loadedCertificateCheck',
            'insecureCertPreviouslyEnabledRef', 'logActivity', 't', code,
        )(
            (fn: () => void) => { effect = fn; },
            { protocol: 'ftp', server: 'ftp.example.test', options: { [key]: value } },
            loadedCertificateCheck,
            { current: false },
            logActivity,
            (key: string) => key,
        );
        effect();
        expect(logActivity).toHaveBeenCalledWith(
            'INFO', 'activity.insecure_cert_enabled', 'success',
        );
    });

    it('carries reset details into the real file-panel restore result', async () => {
        const start = app.indexOf('const importAeroFtpKeystoreFile =');
        const end = app.indexOf('const handleOpenArchiveFromFile =', start);
        const { code } = await transformWithOxc(app.slice(start, end), 'file-panel-restore.ts');
        let dialog!: { onConfirm: (password: string) => Promise<void> };
        const setKeystoreImportResult = vi.fn();
        const reset = [{ name: 'lab', keys: ['trustUnknownHosts'] }];
        const context = {
            useCallback: (fn: unknown) => fn,
            setInputDialog: (value: typeof dialog) => { dialog = value; },
            t: (key: string) => key,
            notify: { warning: vi.fn(), success: vi.fn(), error: vi.fn() },
            invoke: vi.fn(async () => ({ imported: 1, skipped: 0, securityReset: reset })),
            refreshProfilesFromImportedKeystore: vi.fn(async () => undefined),
            setKeystoreImportResult,
        };
        const importFile = new Function(...Object.keys(context), `${code}\nreturn importAeroFtpKeystoreFile;`)(...Object.values(context));
        importFile('/tmp/backup.aeroftp-keystore');
        await dialog.onConfirm('password');
        expect(setKeystoreImportResult).toHaveBeenCalledWith(expect.objectContaining({ securityReset: reset }));
    });
});
