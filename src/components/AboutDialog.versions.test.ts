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

    // Read from the repo files, never written here: a Vite bump changes the
    // lockfile, and these checks must hold for any version it resolves.
    const viteLock = lock?.packages?.['node_modules/vite']?.version;

    // Whether `version` is inside the caret `range` npm admits: the same
    // major (the same minor under 0.x) and not below the range's minimum.
    // npm may lock a later compatible version without touching the range.
    const satisfiesCaret = (version: string, range: string): boolean => {
        const min = /^\^(\d+)\.(\d+)\.(\d+)$/.exec(range);
        const got = /^(\d+)\.(\d+)\.(\d+)$/.exec(version);
        if (!min || !got) return false;
        const [lo, at] = [min.slice(1).map(Number), got.slice(1).map(Number)];
        if (at[0] !== lo[0] || (lo[0] === 0 && at[1] !== lo[1])) return false;
        for (let i = 0; i < 3; i++) {
            if (at[i] !== lo[i]) return at[i] > lo[i];
        }
        return true;
    };

    it('lockfile resolves Vite to a concrete version', () => {
        expect(viteLock).toMatch(/^\d+\.\d+\.\d+$/);
    });

    it('package.json vite dependency is aligned with lockfile', () => {
        const viteManifest = pkg?.devDependencies?.['vite'];
        expect(viteManifest).toMatch(/^\^/);
        expect(satisfiesCaret(viteLock ?? '', viteManifest ?? '')).toBe(true);
    });

    it('a locked version is aligned when the caret range admits it', () => {
        expect(satisfiesCaret('8.3.8', '^8.3.8')).toBe(true);
        expect(satisfiesCaret('8.3.9', '^8.3.8')).toBe(true);
        expect(satisfiesCaret('8.4.0', '^8.3.8')).toBe(true);
        expect(satisfiesCaret('8.3.7', '^8.3.8')).toBe(false);
        expect(satisfiesCaret('9.0.0', '^8.3.8')).toBe(false);
        expect(satisfiesCaret('0.3.1', '^0.2.0')).toBe(false);
        expect(satisfiesCaret('0.2.5', '^0.2.0')).toBe(true);
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

    it('shared resolver falls back to node_modules when packageLock is null (missing lockfile)', () => {
        // When the lockfile cannot be loaded (returns null), the resolver should inspect node_modules
        // to discover the real installed version instead of defaulting to the manifest range minimum.
        const mockManifest = {
            dependencies: {
                vite: '^8.0.0',
            },
            devDependencies: {},
        };

        const resolved = resolveInstalledVersion('vite', undefined, rootDir, null, mockManifest);
        const installedVite = JSON.parse(
            readFileSync(resolve(rootDir, 'node_modules/vite/package.json'), 'utf-8'),
        ).version;
        expect(resolved).toBe(installedVite);
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

        // Vite must report the version the lockfile resolves for the build
        expect(frontDeps.vite).toBe(viteLock);

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

    it('AboutDialog component declares Third-Party Licenses action invoking openThirdPartyLicenses', () => {
        const aboutDialogPath = resolve(__dirname, 'AboutDialog.tsx');
        const aboutDialogSrc = readFileSync(aboutDialogPath, 'utf-8');

        expect(aboutDialogSrc).toContain('openThirdPartyLicenses');
        expect(aboutDialogSrc).toContain("t('about.thirdPartyLicenses')");
    });
});
