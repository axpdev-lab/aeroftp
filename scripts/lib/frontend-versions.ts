// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';

export interface PackageManifest {
  dependencies?: Record<string, string>;
  devDependencies?: Record<string, string>;
}

export interface PackageLock {
  packages?: Record<string, { version?: string }>;
}

export function cleanVer(v: string): string {
  return v.replace(/^[\^~>=<]+/, '');
}

export function loadPackageManifest(rootDir: string): PackageManifest | null {
  try {
    const pkgPath = resolve(rootDir, 'package.json');
    if (existsSync(pkgPath)) {
      return JSON.parse(readFileSync(pkgPath, 'utf-8'));
    }
  } catch {
    // Ignore error
  }
  return null;
}

export function loadPackageLock(rootDir: string): PackageLock | null {
  try {
    const lockPath = resolve(rootDir, 'package-lock.json');
    if (existsSync(lockPath)) {
      return JSON.parse(readFileSync(lockPath, 'utf-8'));
    }
  } catch {
    // Ignore error
  }
  return null;
}

/**
 * Resolves the installed build version of a frontend package.
 * Prefers the locked version from package-lock.json (or node_modules/<name>/package.json)
 * over the bare manifest range minimum in package.json.
 */
export function resolveInstalledVersion(
  name: string,
  fallbackRange?: string,
  rootDir: string = process.cwd(),
  packageLock?: PackageLock | null,
  manifest?: PackageManifest | null,
): string {
  const lock = packageLock !== undefined ? packageLock : loadPackageLock(rootDir);
  const locked = lock?.packages?.[`node_modules/${name}`]?.version;
  if (typeof locked === 'string' && locked) {
    return locked;
  }
  if (packageLock === undefined) {
    try {
      const modPkgPath = resolve(rootDir, `node_modules/${name}/package.json`);
      if (existsSync(modPkgPath)) {
        const modPkg = JSON.parse(readFileSync(modPkgPath, 'utf-8'));
        if (typeof modPkg.version === 'string' && modPkg.version) {
          return modPkg.version;
        }
      }
    } catch {
      // Ignore error
    }
  }
  const pkg = manifest !== undefined ? manifest : loadPackageManifest(rootDir);
  const raw = fallbackRange ?? pkg?.dependencies?.[name] ?? pkg?.devDependencies?.[name] ?? '?';
  return cleanVer(raw);
}
