// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { readFileSync, existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

/**
 * Regression test for CodeRabbit finding on PR #1550 (vite version reporting):
 * Ensures that frontend dependency versions displayed in the About dialog
 * and copied technical details reflect the actual resolved build version
 * from package-lock.json (or installed node_modules), rather than the
 * manifest range minimum in package.json.
 */
describe('AboutDialog frontend dependency versions', () => {
    const pkgPath = resolve(__dirname, '../../package.json');
    const lockPath = resolve(__dirname, '../../package-lock.json');
    const viteConfigPath = resolve(__dirname, '../../vite.config.ts');

    it('manifest and lockfile are present and readable', () => {
        expect(existsSync(pkgPath)).toBe(true);
        expect(existsSync(lockPath)).toBe(true);
        expect(existsSync(viteConfigPath)).toBe(true);
    });

    const pkg = JSON.parse(readFileSync(pkgPath, 'utf-8'));
    const lock = JSON.parse(readFileSync(lockPath, 'utf-8'));
    const viteConfig = readFileSync(viteConfigPath, 'utf-8');

    it('vite.config.ts resolves installed versions rather than bare manifest strings', () => {
        // vite.config.ts must inspect package-lock.json or node_modules
        expect(viteConfig).toContain('package-lock.json');
        expect(viteConfig).toContain('resolveInstalledVersion');
        expect(viteConfig).toContain('__FRONTEND_VERSIONS__');
    });

    it('lockfile resolves Vite 8.3.7', () => {
        const viteLock = lock.packages?.['node_modules/vite']?.version;
        expect(viteLock).toBe('8.3.7');
    });

    it('package.json vite dependency is aligned with lockfile', () => {
        const viteManifest = pkg.devDependencies?.['vite'];
        expect(viteManifest).toBe('^8.3.7');
    });

    it('resolves concrete build versions without range prefixes for all AboutDialog frontend deps', () => {
        const cleanVer = (v: string) => v.replace(/^[\^~>=<]+/, '');

        function resolveInstalledVersion(name: string, fallbackRange?: string): string {
            const locked = lock.packages?.[`node_modules/${name}`]?.version;
            if (typeof locked === 'string' && locked) {
                return locked;
            }
            const raw = fallbackRange ?? pkg.dependencies?.[name] ?? pkg.devDependencies?.[name] ?? '?';
            return cleanVer(raw);
        }

        const frontDeps: Record<string, string> = {
            react: resolveInstalledVersion('react'),
            typescript: resolveInstalledVersion('typescript'),
            tailwindcss: resolveInstalledVersion('tailwindcss'),
            monaco: resolveInstalledVersion('monaco-editor', pkg.dependencies?.['monaco-editor'] ?? pkg.dependencies?.['@monaco-editor/react']),
            vite: resolveInstalledVersion('vite'),
        };

        // Vite must report the build version 8.3.7, not the stale 8.3.6
        expect(frontDeps.vite).toBe('8.3.7');

        // All frontend deps must be concrete semver, no range prefixes or question marks
        for (const [dep, ver] of Object.entries(frontDeps)) {
            expect(ver, `dependency ${dep} should not be ?`).not.toBe('?');
            expect(ver, `dependency ${dep} should not have ^ prefix`).not.toMatch(/^\^/);
            expect(ver, `dependency ${dep} should not have ~ prefix`).not.toMatch(/^~/);
            expect(ver, `dependency ${dep} should be a valid semver string`).toMatch(/^\d+\.\d+\.\d+/);
        }
    });

    it('AboutDialog component declares FRONTEND_DEPS matching expected keys', () => {
        const aboutDialogPath = resolve(__dirname, 'AboutDialog.tsx');
        const aboutDialogSrc = readFileSync(aboutDialogPath, 'utf-8');

        expect(aboutDialogSrc).toContain('FRONTEND_DEPS');
        expect(aboutDialogSrc).toContain("__FRONTEND_VERSIONS__");
        expect(aboutDialogSrc).toContain("{ name: 'Vite', version: _fv.vite }");
    });
});
