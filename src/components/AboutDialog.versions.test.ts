// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
    loadPackageLock,
    loadPackageManifest,
    resolveInstalledVersion,
} from '../../scripts/lib/frontend-versions';

/**
 * Regression test for CodeRabbit and Greptile findings on PR #1550:
 * - CodeRabbit: Ensures that frontend dependency versions displayed in the About
 *   dialog reflect the actual resolved build version from package-lock.json rather
 *   than the manifest range minimum in package.json.
 * - Greptile (review 4518750101): Ensures the production resolver itself is tested
 *   with different manifest and resolved versions to pin lockfile precedence and fallback.
 */
describe('AboutDialog frontend dependency versions', () => {
    const rootDir = resolve(__dirname, '../..');
    const pkgPath = resolve(rootDir, 'package.json');
    const lockPath = resolve(rootDir, 'package-lock.json');
    const viteConfigPath = resolve(rootDir, 'vite.config.ts');

    it('manifest and lockfile are present and readable', () => {
        expect(existsSync(pkgPath)).toBe(true);
        expect(existsSync(lockPath)).toBe(true);
        expect(existsSync(viteConfigPath)).toBe(true);
    });

    const pkg = loadPackageManifest(rootDir);
    const lock = loadPackageLock(rootDir);
    const viteConfig = readFileSync(viteConfigPath, 'utf-8');

    it('vite.config.ts imports and delegates to the shared frontend-versions resolver', () => {
        expect(viteConfig).toContain('./scripts/lib/frontend-versions');
        expect(viteConfig).toContain('resolveInstalledVersion');
        expect(viteConfig).toContain('__FRONTEND_VERSIONS__');
    });

    it('lockfile resolves Vite 8.3.7', () => {
        const viteLock = lock?.packages?.['node_modules/vite']?.version;
        expect(viteLock).toBe('8.3.7');
    });

    it('package.json vite dependency is aligned with lockfile', () => {
        const viteManifest = pkg?.devDependencies?.['vite'];
        expect(viteManifest).toBe('^8.3.7');
    });

    it('shared resolver prioritizes lockfile version when manifest range differs', () => {
        // Greptile finding: test different manifest and resolved versions
        const mockManifest = {
            dependencies: {},
            devDependencies: {
                vite: '^8.3.0',
            },
        };
        const mockLock = {
            packages: {
                'node_modules/vite': { version: '8.3.7' },
            },
        };

        const resolved = resolveInstalledVersion('vite', undefined, rootDir, mockLock, mockManifest);
        expect(resolved).toBe('8.3.7');
    });

    it('shared resolver falls back to cleaned manifest version when package is not in lockfile', () => {
        const mockManifest = {
            dependencies: {
                'unlocked-lib': '^2.4.1',
            },
            devDependencies: {},
        };
        const mockLock = {
            packages: {},
        };

        const resolved = resolveInstalledVersion('unlocked-lib', undefined, rootDir, mockLock, mockManifest);
        expect(resolved).toBe('2.4.1');
    });

    it('shared resolver does not inspect node_modules on disk when packageLock is explicitly provided', () => {
        // Even if 'vite' is installed on disk in node_modules, passing an explicit empty lockfile
        // must not trigger the node_modules fallback and must fall back to the manifest range.
        const mockManifest = {
            dependencies: {
                vite: '^8.3.0',
            },
            devDependencies: {},
        };
        const mockLock = {
            packages: {},
        };

        const resolved = resolveInstalledVersion('vite', undefined, rootDir, mockLock, mockManifest);
        expect(resolved).toBe('8.3.0');
    });

    it('resolves concrete build versions without range prefixes for all AboutDialog frontend deps using real repo files', () => {
        const frontDeps: Record<string, string> = {
            react: resolveInstalledVersion('react', undefined, rootDir, lock, pkg),
            typescript: resolveInstalledVersion('typescript', undefined, rootDir, lock, pkg),
            tailwindcss: resolveInstalledVersion('tailwindcss', undefined, rootDir, lock, pkg),
            monaco: resolveInstalledVersion(
                'monaco-editor',
                pkg?.dependencies?.['monaco-editor'] ?? pkg?.dependencies?.['@monaco-editor/react'],
                rootDir,
                lock,
                pkg,
            ),
            vite: resolveInstalledVersion('vite', undefined, rootDir, lock, pkg),
        };

        // Vite must report the build version 8.3.7
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
        expect(aboutDialogSrc).toContain('__FRONTEND_VERSIONS__');
        expect(aboutDialogSrc).toContain("{ name: 'Vite', version: _fv.vite }");
    });
});
