// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import table from '../shared/profile-security-relaxations.json';
import { canonicalOptionsForEdit, endpointStaysOnThisMachine, loadedCertificateCheck, loadedOption, loadedOptionOn, makeSafe, securityRelaxations } from './profileSecurity';

interface Case {
    name: string;
    profile: { host?: string; port?: number; providerId?: string; protocol?: string; options?: Record<string, unknown> };
    relaxed: string[];
}

describe('profile security relaxations (shared table)', () => {
    const cases = (table as unknown as { cases: Case[] }).cases;

    it('has the cases the backend runs too', () => {
        expect(cases.length).toBeGreaterThanOrEqual(10);
    });

    it.each(cases.map(c => [c.name, c] as const))('%s', (_name, c) => {
        expect(securityRelaxations(c.profile).map(r => r.key)).toEqual(c.relaxed);
    });
});

describe('makeSafe', () => {
    it('removes only what is not kept and leaves the input untouched', () => {
        const profile = {
            host: 'files.example.com',
            options: { verifyCert: false, trustPasvAddress: true, trustUnknownHosts: true, timeout: 30 },
        };
        const { profile: safe, removed } = makeSafe(profile, ['trustPasvAddress']);
        expect(removed).toEqual(['verifyCert', 'trustUnknownHosts']);
        expect(safe.options).toEqual({ trustPasvAddress: true, timeout: 30, verifyCert: true, trustUnknownHosts: false });
        expect(profile.options.verifyCert).toBe(false);
    });

    it('removes every spelling and stores the safe scheme where leaving it out is not safe', () => {
        const snake = makeSafe({ host: 'dav.example.com', options: { verify_cert: false } });
        expect(snake.removed).toEqual(['verifyCert']);
        expect(snake.profile.options).toEqual({ verifyCert: true });
        const port80 = makeSafe({ host: 'dav.example.com', options: { webdavScheme: 'http', tls_mode: 'HTTP' } });
        expect(port80.removed).toEqual(['webdavScheme']);
        expect(port80.profile.options).toEqual({ webdavScheme: 'https' });
        const automatic = makeSafe({ host: 'dav.example.com', port: 80, protocol: 'webdav', options: { webdavScheme: 'auto' } });
        expect(automatic.removed).toEqual(['webdavScheme']);
        expect(automatic.profile.options).toEqual({ webdavScheme: 'https' });
    });

    it('rewrites a WebDAV host written as http:// to https://', () => {
        const { profile, removed } = makeSafe({ host: 'http://dav.example.com:8080', protocol: 'webdav', options: {} });
        expect(removed).toEqual(['webdavScheme']);
        expect(profile.host).toBe('https://dav.example.com:8080');
        expect(profile.options).toEqual({ webdavScheme: 'https' });
    });

    it('resets the WebDAV scheme stored as ftpsMode and a webdavs http:// host', () => {
        const ftps = makeSafe({ host: 'dav.example.com', protocol: 'webdav', options: { ftpsMode: 'http' } });
        expect(ftps.removed).toEqual(['webdavScheme']);
        expect(ftps.profile.options).toEqual({ webdavScheme: 'https' });
        const webdavs = makeSafe({ host: 'http://dav.example.com', protocol: 'webdavs', options: {} });
        expect(webdavs.removed).toEqual(['webdavScheme']);
        expect(webdavs.profile.host).toBe('https://dav.example.com');
        expect(webdavs.profile.options).toEqual({ webdavScheme: 'https' });
    });

    it('returns the same profile when nothing relaxes a check', () => {
        const profile = { host: 'files.example.com', options: { timeout: 30 } };
        expect(makeSafe(profile)).toEqual({ profile, removed: [] });
    });
});

describe('endpointStaysOnThisMachine', () => {
    it('reads the host the request reaches', () => {
        for (const endpoint of ['http://127.0.0.1:9000', 'localhost:8080', 'http://[::1]:1900', 'user:pw@127.0.0.1']) {
            expect(endpointStaysOnThisMachine(endpoint)).toBe(true);
        }
        for (const endpoint of ['http://evil.example#@127.0.0.1', 'http://evil.example\\@127.0.0.1', 'http://localhost.evil.example', 'nas.local', '']) {
            expect(endpointStaysOnThisMachine(endpoint)).toBe(false);
        }
    });
});

// CodeRabbit on #1524: the GUI connects with the value the loader ends up
// with, the one the import guard compares, so two spellings cannot disagree.
describe('loadedOption', () => {
    it('takes the spelling the loader applies last', () => {
        const options = { trustPasvAddress: false, trust_pasv_address: true };
        expect(loadedOption(options, 'trustPasvAddress')).toBe(true);
        expect(loadedOptionOn(options, 'trustPasvAddress')).toBe(true);
        expect(loadedOptionOn({ trustPasvAddress: true, trust_pasv_address: 'false' }, 'trustPasvAddress')).toBe(false);
    });

    it('skips values the loader ignores and normalizes aliases for an edit', () => {
        const options = { trustPasvAddress: true, trust_pasv_address: null, verify_cert: false };
        expect(loadedOption(options, 'trustPasvAddress')).toBe(true);
        expect(canonicalOptionsForEdit(options)).toEqual({ trustPasvAddress: true, verifyCert: false });
        expect(loadedCertificateCheck(canonicalOptionsForEdit(options))).toBe(false);
        expect(canonicalOptionsForEdit({ tlsMode: 'explicit' }, 'ftp')).toEqual({ tlsMode: 'explicit' });
        const before = { trustPasvAddress: true, trust_pasv_address: null };
        const after = { trustPasvAddress: true, trust_pasv_address: true };
        expect(loadedOptionOn(before, 'trustPasvAddress')).toBe(true);
        expect(loadedOptionOn(after, 'trustPasvAddress')).toBe(true);
    });

    it('reads a kept string value as the providers do', () => {
        expect(loadedOptionOn({ trustPasvAddress: 'true' }, 'trustPasvAddress')).toBe(true);
        expect(loadedOptionOn({ trustPasvAddress: 'TRUE' }, 'trustPasvAddress')).toBe(false);
        expect(loadedOptionOn({}, 'trustPasvAddress')).toBe(false);
        expect(loadedCertificateCheck({ verifyCert: 'false' })).toBe(false);
        expect(loadedCertificateCheck({ verifyCert: true, verify_cert: false })).toBe(false);
        expect(loadedCertificateCheck({ verifyCert: 'False' })).toBe(true);
        expect(loadedCertificateCheck(undefined)).toBe(true);
    });
});
