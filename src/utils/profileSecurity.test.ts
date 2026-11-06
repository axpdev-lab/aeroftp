// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import table from '../shared/profile-security-relaxations.json';
import { endpointStaysOnThisMachine, makeSafe, securityRelaxations } from './profileSecurity';

interface Case {
    name: string;
    profile: { host?: string; providerId?: string; options?: Record<string, unknown> };
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
        expect(safe.options).toEqual({ trustPasvAddress: true, timeout: 30 });
        expect(profile.options.verifyCert).toBe(false);
    });

    it('removes every spelling and stores the safe scheme where leaving it out is not safe', () => {
        const snake = makeSafe({ host: 'dav.example.com', options: { verify_cert: false } });
        expect(snake.removed).toEqual(['verifyCert']);
        expect(snake.profile.options).toEqual({});
        const port80 = makeSafe({ host: 'dav.example.com', options: { webdavScheme: 'http', tls_mode: 'HTTP' } });
        expect(port80.removed).toEqual(['webdavScheme']);
        expect(port80.profile.options).toEqual({ webdavScheme: 'https' });
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
