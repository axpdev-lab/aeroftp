#!/usr/bin/env npx tsx
/**
 * AeroFTP Third-Party License Notices Generator
 *
 * Scans locked dependency trees:
 *   - Cargo: src-tauri/Cargo.lock filtered by cargo tree --target all -e normal,build
 *   - npm: package-lock.json (production dependencies only, traversed by lock entries)
 *
 * Verifies licenses against GPL-3.0-or-later distribution compatibility,
 * and outputs THIRD-PARTY-NOTICES.md with full license texts, copyright lines,
 * Apache NOTICE files, deduplicated text sections, and standard SPDX texts
 * for packages without dedicated license files.
 *
 * Bundled in distributions: deb, rpm, AppImage, snap, DMG, MSI.
 * (Note: Flatpak packaging is maintained separately in the FlatPark repository).
 *
 * Standard license texts sourced from SPDX License List v3.26.0 (spdx.org/licenses).
 *
 * Usage:
 *   npx tsx scripts/gen-third-party-notices.ts --check
 *   npx tsx scripts/gen-third-party-notices.ts --write
 */

import { execFileSync } from 'node:child_process';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const ROOT = path.resolve(__dirname, '..');
const NOTICES_FILE = path.join(ROOT, 'THIRD-PARTY-NOTICES.md');
export const SPDX_STANDARD_LICENSES_FILE = path.join(__dirname, 'spdx-standard-licenses.json');

export interface ComponentInfo {
  name: string;
  version: string;
  license: string;
  repository?: string;
  source: 'cargo' | 'npm';
  licenseFiles: { filename: string; content: string }[];
  noticeFiles: { filename: string; content: string }[];
  authors?: string[];
  packageDir?: string;
}

/**
 * Byte-wise ASCII string comparator for deterministic sorting across platforms and locales.
 */
export const compareStrings = (a: string, b: string): number => (a < b ? -1 : a > b ? 1 : 0);

/**
 * Pinned table of name@version -> license for crates that do not declare
 * an SPDX license expression in their Cargo.toml manifest (e.g. using license_file).
 * If a crate is not listed here and lacks an SPDX license, generation fails.
 */
export const PINNED_CRATE_LICENSES: Record<string, string> = {
  'gopenpgp-sys@0.3.9': 'MIT',
  'proton-crypto@0.14.1': 'MIT',
  'proton-crypto-account@0.20.1': 'MIT',
  'proton-rpgp@0.9.0': 'MIT',
  'proton-srp@0.8.4': 'MIT',
  'sigstore-protobuf-specs-derive@0.0.1': 'Apache-2.0',
};

export interface NonFreeException {
  crateName: string;
  version: string;
  author: string;
  licenseName: string;
  sourceFile: string;
  facts: string;
}

/**
 * Pinned list of known non-free component exceptions confirmed by maintainer decision.
 * Generation fails if any component declared here disappears from the dependency tree,
 * or if any component containing non-free restrictions is not listed here.
 */
export const KNOWN_NON_FREE_EXCEPTIONS: NonFreeException[] = [
  {
    crateName: 'unrar_sys',
    version: '0.5.8',
    author: 'Alexander Roshal',
    licenseName: 'UnRAR License',
    sourceFile: 'vendor/unrar/license.txt',
    facts:
      'RAR extraction uses the UnRAR source code by Alexander Roshal (through the unrar crate, vendored unrar_sys), distributed under its own license, which allows it to be used and redistributed but forbids using it to re-create the RAR compression algorithm. AeroFTP only extracts RAR archives and never creates them. The UnRAR license is not a free-software license, and it is not GPL-compatible.',
  },
];

export interface FallbackAttribution {
  author: string;
  copyrightHolder?: string;
  year?: string;
  source: string;
}

/**
 * Pinned reviewed attributions for components that declare standard open-source licenses
 * without physical license texts or manifest author entries.
 */
export const PINNED_FALLBACK_ATTRIBUTIONS: Record<string, FallbackAttribution> = {
  'alloc-stdlib@0.2.4': {
    author: 'Daniel Reiter Horn <danielrh@dropbox.com>',
    copyrightHolder: 'Dropbox, Inc.',
    year: '2016',
    source: 'https://github.com/dropbox/rust-alloc-no-stdlib/blob/master/LICENSE (repository LICENSE: Copyright (c) 2016 Dropbox, Inc.; Cargo.toml authors: Daniel Reiter Horn)',
  },
  'cx448@0.1.1': {
    author: 'Friedel Ziegelmayer (port to RustCrypto), based on crate-crypto/x448 by Kevaundray Wedderburn',
    source: 'https://github.com/dignifiedquire/cx448 (README: based on crate-crypto/x448 by Kevaundray Wedderburn, BSD-3-Clause, and RustCrypto/elliptic-curves#1121)',
  },
  'dispatch2@0.3.1': {
    author: 'Mads Marquart <mads@marquart.dk>, Mary <mary@mary.zone>',
    source: 'https://github.com/madsmtm/objc2/blob/master/crates/dispatch2/Cargo.toml (Cargo.toml authors; no copyright notice in repo)',
  },
  'jni-macros@0.22.4': {
    author: 'jni team',
    source: 'https://github.com/jni-rs/jni-rs/blob/master/Cargo.toml (Cargo.toml authors in parent jni crate; no copyright notice in repo)',
  },
  'libappindicator-sys@0.9.0': {
    author: 'Mikkel Hougesen',
    source: 'https://github.com/Hougesen/libappindicator-rs (crate maintainer and repository owner; no copyright notice in repo)',
  },
  'objc2-app-kit@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-cloud-kit@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-core-data@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-core-foundation@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-core-graphics@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-core-image@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-core-location@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-core-services@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-core-text@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-core-wlan@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-encode@4.1.0': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-exception-helper@0.1.1': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-foundation@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-quartz-core@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-security@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-security-foundation@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-system-configuration@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-ui-kit@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-user-notifications@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'objc2-web-kit@0.3.2': {
    author: 'Mads Marquart <mads@marquart.dk>',
    source: 'https://github.com/madsmtm/objc2/blob/master/Cargo.toml (workspace maintainer and author in parent objc2 Cargo.toml; no copyright notice in repo)',
  },
  'r-efi@5.3.0': {
    author: 'David Rheinsberg, Red Hat, Inc., Microsoft Corporation',
    copyrightHolder: 'Red Hat, Inc., Microsoft Corporation, David Rheinsberg',
    year: '2017-2023',
    source: 'https://github.com/r-efi/r-efi/blob/master/AUTHORS (AUTHORS file COPYRIGHT section: Copyright (C) 2017-2023 Red Hat, Inc., 2019-2023 Microsoft Corporation, 2022-2023 David Rheinsberg)',
  },
  'r-efi@6.0.0': {
    author: 'David Rheinsberg, Red Hat, Inc., Microsoft Corporation',
    copyrightHolder: 'Red Hat, Inc., Microsoft Corporation, David Rheinsberg',
    year: '2017-2023',
    source: 'https://github.com/r-efi/r-efi/blob/master/AUTHORS (AUTHORS file COPYRIGHT section: Copyright (C) 2017-2023 Red Hat, Inc., 2019-2023 Microsoft Corporation, 2022-2023 David Rheinsberg)',
  },
  'russh-util@0.52.0': {
    author: 'Pierre-Étienne Meunier <pe@pijul.org>, Eugeny Pankov <e@ajenti.org>',
    source: 'https://github.com/warp-tech/russh/blob/main/Cargo.toml (Cargo.toml authors in parent russh crate; no copyright notice in repo)',
  },
  'rustls-platform-verifier-android@0.1.1': {
    author: 'The Rustls Project Developers',
    source: 'https://github.com/rustls/rustls-platform-verifier (parent repository README and rustls organization project contributors; no copyright notice in repo)',
  },
  'webview2-com@0.38.2': {
    author: 'Peter Atashian, wravery contributors',
    source: 'https://github.com/wravery/webview2-rs (repository author Peter Atashian and project contributors; no copyright notice in repo)',
  },
  'webview2-com-macros@0.8.1': {
    author: 'Peter Atashian, wravery contributors',
    source: 'https://github.com/wravery/webview2-rs (repository author Peter Atashian and project contributors; no copyright notice in repo)',
  },
  'webview2-com-sys@0.38.2': {
    author: 'Peter Atashian, wravery contributors',
    source: 'https://github.com/wravery/webview2-rs (repository author Peter Atashian and project contributors; no copyright notice in repo)',
  },
};

/**
 * Formats a unified-style diff highlighting the first divergence between expected and actual text.
 */
export function formatDiff(expected: string, actual: string, maxLines = 40): string {
  const expLines = expected.split('\n');
  const actLines = actual.split('\n');
  let firstDiff = -1;
  const maxLen = Math.max(expLines.length, actLines.length);
  for (let i = 0; i < maxLen; i++) {
    if (expLines[i] !== actLines[i]) {
      firstDiff = i;
      break;
    }
  }
  if (firstDiff === -1) return '';

  const start = Math.max(0, firstDiff - 2);
  const end = Math.min(maxLen, firstDiff + maxLines);
  const out: string[] = [];
  out.push(`--- existing (line ${firstDiff + 1})`);
  out.push(`+++ generated (line ${firstDiff + 1})`);
  for (let i = start; i < end; i++) {
    const e = expLines[i];
    const a = actLines[i];
    if (e === a) {
      out.push(` ${e ?? ''}`);
    } else {
      if (e !== undefined) out.push(`-${e}`);
      if (a !== undefined) out.push(`+${a}`);
    }
  }
  if (end < maxLen) {
    out.push('... (diff truncated)');
  }
  return out.join('\n');
}

// Known individual licenses compatible with GPL-3.0-or-later distribution
export const COMPATIBLE_SPDX_ATOMS = new Set([
  'MIT',
  'MIT-0',
  '0BSD',
  'ISC',
  'BSL-1.0',
  'CC0-1.0',
  'Unlicense',
  'Zlib',
  'Unicode-3.0',
  'Unicode-DFS-2016',
  'CDLA-Permissive-2.0',
  'bzip2-1.0.6',
  'bzip2-1.0.5',
  'WTFPL',
  'MPL-2.0',
  'Apache-2.0',
  'BSD-1-Clause',
  'BSD-2-Clause',
  'BSD-3-Clause',
  'BSD-3-Clause-Clear',
  'LGPL-2.1',
  'LGPL-2.1-or-later',
  'LGPL-2.1+',
  'LGPL-3.0',
  'LGPL-3.0-only',
  'LGPL-3.0-or-later',
  'LGPL-3.0+',
  'GPL-2.0-or-later',
  'GPL-2.0+',
  'GPL-3.0',
  'GPL-3.0-only',
  'GPL-3.0-or-later',
  'GPL-3.0+',
  'AGPL-3.0',
  'AGPL-3.0-only',
  'AGPL-3.0-or-later',
  'AGPL-3.0+',
]);

export const ALLOWED_EXCEPTIONS = new Set([
  'LLVM-exception',
  'GCC-exception-3.1',
  'Classpath-exception-2.0',
]);

const OPERATOR_TOKENS = new Set(['(', ')', 'AND', 'OR', 'WITH']);

export function isAtomGplCompatible(id: string, exception?: string): boolean {
  if (exception && !ALLOWED_EXCEPTIONS.has(exception)) return false;
  // GPL-2.0-only is strictly incompatible with GPL-3.0
  if (id === 'GPL-2.0' || id === 'GPL-2.0-only') return false;
  return COMPATIBLE_SPDX_ATOMS.has(id);
}

export function tokenizeSpdx(input: string): string[] {
  const normalized = input.replace(/\//g, ' OR ');
  const regex = /\(|\)|[a-zA-Z0-9_\-\.\+]+/g;
  const tokens: string[] = [];
  let lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = regex.exec(normalized)) !== null) {
    const gap = normalized.slice(lastIndex, m.index);
    if (!/^\s*$/.test(gap)) {
      // Unrecognized characters found in expression
      return [];
    }
    tokens.push(m[0]);
    lastIndex = regex.lastIndex;
  }
  const trailing = normalized.slice(lastIndex);
  if (!/^\s*$/.test(trailing)) {
    return [];
  }
  return tokens;
}

/**
 * Extracts exact individual SPDX license identifiers from an expression,
 * filtering out operators and exception names.
 */
export function extractSpdxLicenseIds(expr: string): string[] {
  const tokens = tokenizeSpdx(expr);
  const ids: string[] = [];
  for (let i = 0; i < tokens.length; i++) {
    const t = tokens[i];
    if (OPERATOR_TOKENS.has(t.toUpperCase())) continue;
    if (i > 0 && tokens[i - 1].toUpperCase() === 'WITH') continue;
    if (ALLOWED_EXCEPTIONS.has(t)) continue;
    ids.push(t);
  }
  return ids;
}

/**
 * Parses and evaluates an SPDX license expression for GPL-3.0-or-later compatibility.
 *   - 'OR': satisfied if any branch is compatible (dual-licensing).
 *   - 'AND': satisfied only if all conjuncts are compatible.
 *   - 'WITH': allowed only with recognized GPL-compatible exception clauses.
 *   - Parentheses: evaluated with standard operator precedence.
 * Requires complete token consumption, well-formed operands, and balanced parentheses.
 */
export function isGplCompatible(expr: string): boolean {
  if (!expr || expr === 'NO_LICENSE' || expr === 'Unknown') return false;
  const tokens = tokenizeSpdx(expr);
  if (tokens.length === 0) return false;

  let pos = 0;
  let hasError = false;

  function peek(): string | undefined {
    return tokens[pos];
  }
  function consume(): string | undefined {
    return tokens[pos++];
  }

  function parseOr(): boolean {
    let res = parseAnd();
    while (peek() && peek()!.toUpperCase() === 'OR') {
      consume();
      if (!peek() || peek() === ')') {
        hasError = true;
        return false;
      }
      const rhs = parseAnd();
      res = res || rhs;
    }
    return res;
  }

  function parseAnd(): boolean {
    let res = parseFactor();
    while (peek() && peek()!.toUpperCase() === 'AND') {
      consume();
      if (!peek() || peek() === ')') {
        hasError = true;
        return false;
      }
      const rhs = parseFactor();
      res = res && rhs;
    }
    return res;
  }

  function parseFactor(): boolean {
    const t = peek();
    if (!t) {
      hasError = true;
      return false;
    }
    if (t === '(') {
      consume();
      const val = parseOr();
      if (peek() !== ')') {
        hasError = true;
        return false;
      }
      consume(); // consume ')'
      return val;
    }
    if (t === ')' || OPERATOR_TOKENS.has(t.toUpperCase())) {
      hasError = true;
      return false;
    }
    const id = consume()!;
    let ex: string | undefined;
    if (peek() && peek()!.toUpperCase() === 'WITH') {
      consume();
      const next = peek();
      if (!next || next === '(' || next === ')' || OPERATOR_TOKENS.has(next.toUpperCase())) {
        hasError = true;
        return false;
      }
      ex = consume();
    }
    return isAtomGplCompatible(id, ex);
  }

  const result = parseOr();
  if (hasError || pos !== tokens.length) {
    return false;
  }
  return result;
}

// Folders whose license files describe no code that ships: tests, fixtures,
// examples, CI, and node_modules (its packages are components of their own).
const VENDORED_SKIP_DIR = /^(?:\.git|\.github|node_modules|tests?|testdata|fixtures?|examples?|benches)$/i;
// A license or notice file by name, not a source file named after one
// (copying.rs, copyright.mjs, copyright.pm, license.yml).
const VENDORED_LICENSE_FILE =
  /^(?:licen[sc]e|copying|unlicense|notice|copyright)(?:[-_][^.]*)?(?:\.(?:txt|md|markdown|rst|info-zip|apache|mit|bsd|gplv[23]|lgplv2\.1|unicode))?$/i;

export function readPackageFiles(
  dir: string,
  componentId: string,
  declaredLicenseFile?: string
): {
  licenseFiles: { filename: string; content: string }[];
  noticeFiles: { filename: string; content: string }[];
} {
  const licenseFiles: { filename: string; content: string }[] = [];
  const noticeFiles: { filename: string; content: string }[] = [];

  if (!fs.existsSync(dir)) {
    throw new Error(`Package directory for ${componentId} does not exist at ${dir}`);
  }

  // 1. If crate declared an explicit license_file, read and include it
  if (declaredLicenseFile) {
    const declaredPath = path.resolve(dir, declaredLicenseFile);
    if (!fs.existsSync(declaredPath)) {
      throw new Error(
        `Crate ${componentId} declared license_file "${declaredLicenseFile}" but the file does not exist at ${declaredPath}`
      );
    }
    const filename = path.basename(declaredLicenseFile);
    const content = fs.readFileSync(declaredPath, 'utf8').replace(/\r\n/g, '\n').trim();
    licenseFiles.push({ filename, content });
  }

  // 2. Scan direct entries in package directory
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  for (const entry of entries) {
    if (entry.isFile()) {
      const f = entry.name;
      const fullPath = path.join(dir, f);
      if (declaredLicenseFile && path.resolve(fullPath) === path.resolve(dir, declaredLicenseFile)) {
        continue;
      }
      if (/^(notice|copyright)/i.test(f)) {
        noticeFiles.push({ filename: f, content: fs.readFileSync(fullPath, 'utf8').replace(/\r\n/g, '\n').trim() });
      } else if (/^(licen[sc]e|copying|unlicense)/i.test(f)) {
        licenseFiles.push({ filename: f, content: fs.readFileSync(fullPath, 'utf8').replace(/\r\n/g, '\n').trim() });
      }
    } else if (entry.isDirectory() && /^licenses?$/i.test(entry.name)) {
      // 3. Scan REUSE / standard license subdirectories (LICENSES/, licenses/, license/)
      const subDir = path.join(dir, entry.name);
      try {
        const subEntries = fs.readdirSync(subDir, { withFileTypes: true });
        for (const sub of subEntries) {
          if (sub.isFile()) {
            const fullPath = path.join(subDir, sub.name);
            const filename = `${entry.name}/${sub.name}`;
            const content = fs.readFileSync(fullPath, 'utf8').replace(/\r\n/g, '\n').trim();
            if (content.length > 0) {
              if (/^(notice|copyright)/i.test(sub.name)) {
                noticeFiles.push({ filename, content });
              } else {
                licenseFiles.push({ filename, content });
              }
            }
          }
        }
      } catch (err: any) {
        throw new Error(`Failed to read licenses directory in ${componentId} at ${subDir}: ${err.message}`);
      }
    } else if (entry.isDirectory() && !VENDORED_SKIP_DIR.test(entry.name)) {
      // 4. Code a package vendors (a C library, Go modules, a polyfill) keeps
      // its license file in its own folder, at any depth: libssh2/COPYING,
      // go/vendor/golang.org/x/crypto/LICENSE, src/unicode_tables/LICENSE-UNICODE.
      const subDir = path.join(dir, entry.name);
      try {
        const walkVendored = (d: string, rel: string) => {
          for (const sub of fs.readdirSync(d, { withFileTypes: true })) {
            const filename = `${rel}/${sub.name}`;
            if (sub.isDirectory()) {
              if (!VENDORED_SKIP_DIR.test(sub.name)) walkVendored(path.join(d, sub.name), filename);
            } else if (sub.isFile() && VENDORED_LICENSE_FILE.test(sub.name)) {
              const content = fs.readFileSync(path.join(d, sub.name), 'utf8').replace(/\r\n/g, '\n').trim();
              if (content.length === 0) continue;
              if (/^(notice|copyright)/i.test(sub.name)) {
                noticeFiles.push({ filename, content });
              } else {
                licenseFiles.push({ filename, content });
              }
            }
          }
        };
        walkVendored(subDir, entry.name);
      } catch (err: any) {
        throw new Error(`Failed to read vendored files in ${componentId} at ${subDir}: ${err.message}`);
      }
    }
  }

  // An SPDX document (LICENSE.spdx) declares license identifiers and a
  // copyright line but carries no license text: it is a notice, and a package
  // that ships nothing else gets the standard texts of its license.
  const spdxDocuments = licenseFiles.filter((f) => /^SPDXVersion:/.test(f.content));
  for (const f of spdxDocuments) {
    licenseFiles.splice(licenseFiles.indexOf(f), 1);
    noticeFiles.push(f);
  }

  licenseFiles.sort((a, b) => compareStrings(a.filename, b.filename));
  noticeFiles.sort((a, b) => compareStrings(a.filename, b.filename));
  return { licenseFiles, noticeFiles };
}

/**
 * Scans locked package sources (header comments of source files, README, and AUTHORS/COPYRIGHT/NOTICE files)
 * for copyright notices. Deduplicates notices and returns them sorted deterministically.
 * Excludes files already collected by readPackageFiles.
 */
export function findPackageCopyrightNotices(
  packageDir?: string,
  excludedFiles: string[] = []
): { notice: string; file: string }[] {
  if (!packageDir || !fs.existsSync(packageDir)) return [];
  const files: { full: string; rel: string }[] = [];

  // Every file the package ships, at any depth, as the property test in
  // test-third-party-notices.mjs greps it; nested node_modules are other
  // components and are scanned as themselves.
  function walk(currentDir: string, relBase: string) {
    if (!fs.existsSync(currentDir)) return;
    const entries = fs.readdirSync(currentDir, { withFileTypes: true });
    for (const e of entries) {
      const rel = relBase ? `${relBase}/${e.name}` : e.name;
      const full = path.join(currentDir, e.name);
      if (e.isFile()) {
        if (excludedFiles && excludedFiles.includes(rel)) {
          continue;
        }
        files.push({ full, rel });
      } else if (e.isDirectory() && e.name !== 'node_modules' && e.name !== '.git') {
        walk(full, rel);
      }
    }
  }

  walk(packageDir, '');
  files.sort((a, b) => compareStrings(a.rel, b.rel));

  const notices: { notice: string; file: string }[] = [];
  const seenNotice = new Set<string>();

  for (const f of files) {
    let buf: Buffer;
    try {
      buf = fs.readFileSync(f.full);
    } catch (err: any) {
      throw new Error(`Failed to read package file at ${f.full}: ${err.message}`);
    }
    // Binary files (a NUL byte near the start, as grep -I decides) hold no notice.
    if (buf.subarray(0, 8000).includes(0)) continue;
    const lines = buf.toString('utf8').split(/\r?\n/);

    // In a machine-readable copyright file (REUSE .reuse/dep5, debian/copyright)
    // a `Copyright:` field names holders on its line and on the indented lines
    // that continue it.
    const isCopyrightFormat = /(?:^|\/)(?:\.reuse\/dep5|debian\/copyright)$/.test(f.rel);
    let inCopyrightField = false;
    for (const rawLine of lines) {
      const field = isCopyrightFormat ? rawLine.match(/^Copyright:\s*(.*)$/i) : null;
      const continued = inCopyrightField && /^\s+\S/.test(rawLine) ? rawLine.trim() : null;
      inCopyrightField = field !== null || continued !== null;
      const fieldValue = field ? field[1].trim() : continued;
      if (fieldValue) {
        const notice = /^(?:copyright\b|\(c\)|©)/i.test(fieldValue) ? fieldValue : `Copyright ${fieldValue}`;
        if (/[A-Za-z0-9]/.test(fieldValue) && !seenNotice.has(notice)) {
          seenNotice.add(notice);
          notices.push({ notice, file: f.rel });
        }
        continue;
      }
      let line = rawLine
        .replace(/^\s*(?:\/\/[\/!]?|\/\*+|\*+|\#+|<!--)\s*/, '')
        .replace(/(?:\*\/|-->)\s*$/, '')
        .trim();
      if (!line) continue;

      if (/the above copyright notice/i.test(line)) continue;
      if (/authors or copyright holders/i.test(line)) continue;
      if (/see the copyright file/i.test(line)) continue;
      if (/retains? all copyright/i.test(line)) continue;
      if (/subject to the following conditions/i.test(line)) continue;
      if (/copyright (?:notice|license|holders?|owner)/i.test(line)) continue;

      // A notice at the start of the line, or a dated one inside it
      // ("/*! regenerator-runtime -- Copyright (c) 2014-present, Facebook").
      const match =
        line.match(/^SPDX-FileCopyrightText:\s*\S.*/) ??
        line.match(/^(?:(?:\([cC]\)|©)\s+[0-9]{4}|[Cc]opyright\s+(?:\([cC]\)|©|[0-9]{4}|by\s+[A-Z]|[A-Z])).*/) ??
        line.match(/(?:copyright\s*(?:\([cC]\)|©)\s*[0-9]{4}|copyright\s+[0-9]{4}|\([cC]\)\s*[0-9]{4}|©\s*[0-9]{4})\s*\S.*/i);
      if (match) {
        // A notice inside a minified line ends where its comment does.
        let cleaned = match[0].replace(/\s*(?:\*\/|-->).*$/, '').trim();
        cleaned = cleaned.replace(/\s*\([A-Za-z0-9\.\s\-]+ [Ll]icense\)\.?$/, '');
        cleaned = cleaned.replace(/\s+See the\b.*/i, '');
        cleaned = cleaned.trim();
        if (cleaned.length > 0 && !seenNotice.has(cleaned)) {
          seenNotice.add(cleaned);
          notices.push({ notice: cleaned, file: f.rel });
        }
      }
    }
  }

  notices.sort((a, b) => compareStrings(a.notice, b.notice) || compareStrings(a.file, b.file));
  return notices;
}

export function collectCargoDependencies(): ComponentInfo[] {
  const cargoToml = path.join(ROOT, 'src-tauri', 'Cargo.toml');

  // Exact active production graph (normal + build dependencies across all target platforms)
  const treeRaw = execFileSync(
    'cargo',
    ['tree', '--locked', '--prefix', 'none', '--target', 'all', '-e', 'normal,build', '--manifest-path', cargoToml],
    { maxBuffer: 64 * 1024 * 1024, encoding: 'utf8' }
  );

  const activeCrates = new Set<string>();
  for (const line of treeRaw.split('\n')) {
    const match = line.trim().match(/^([a-zA-Z0-9_\-]+)\s+v([0-9\.\-+a-zA-Z]+)/);
    if (match) {
      const [, name, version] = match;
      if (name !== 'aeroftp' && name !== 'aeroftp-peer-l0') {
        activeCrates.add(`${name}@${version}`);
      }
    }
  }

  const metaRaw = execFileSync(
    'cargo',
    ['metadata', '--locked', '--format-version', '1', '--manifest-path', cargoToml],
    { maxBuffer: 64 * 1024 * 1024, encoding: 'utf8' }
  );
  const meta = JSON.parse(metaRaw);
  const packages: any[] = meta.packages || [];
  const results: ComponentInfo[] = [];

  for (const pkg of packages) {
    const key = `${pkg.name}@${pkg.version}`;
    if (!activeCrates.has(key)) continue;

    let license = pkg.license;
    if (!license) {
      if (PINNED_CRATE_LICENSES[key]) {
        license = PINNED_CRATE_LICENSES[key];
      } else {
        throw new Error(
          `Crate ${key} has no declared SPDX license and is not registered in PINNED_CRATE_LICENSES`
        );
      }
    }

    const crateDir = path.dirname(pkg.manifest_path);
    const { licenseFiles, noticeFiles } = readPackageFiles(crateDir, key, pkg.license_file);

    results.push({
      name: pkg.name,
      version: pkg.version,
      license,
      repository: pkg.repository || (pkg.homepage || undefined),
      source: 'cargo',
      licenseFiles,
      noticeFiles,
      authors: pkg.authors,
      packageDir: crateDir,
    });
  }

  return results.sort((a, b) => compareStrings(a.name, b.name) || compareStrings(a.version, b.version));
}

export function collectNpmDependencies(
  pkgLockPath = path.join(ROOT, 'package-lock.json'),
  baseDir = ROOT
): ComponentInfo[] {
  const lock = JSON.parse(fs.readFileSync(pkgLockPath, 'utf8'));
  const packages = lock.packages || {};
  const results: ComponentInfo[] = [];

  // Traversal walks exact lockfile package paths, including all production/runtime
  // packages and transitive dependencies, while excluding devDependencies (dev: true).
  // Package paths identify specific nested versions (e.g. nested type or utility packages).
  for (const [pkgPath, pkgInfo] of Object.entries<any>(packages)) {
    if (pkgPath === '' || pkgInfo.dev) continue;

    const name = pkgInfo.name || pkgPath.replace(/^.*node_modules\//, '');
    const version = pkgInfo.version || 'unknown';
    const depDir = path.join(baseDir, pkgPath);
    const componentId = `${name}@${version}`;
    const { licenseFiles, noticeFiles } = readPackageFiles(depDir, componentId);

    results.push({
      name,
      version,
      license: pkgInfo.license || 'Unknown',
      repository: pkgInfo.homepage || undefined,
      source: 'npm',
      licenseFiles,
      noticeFiles,
      packageDir: depDir,
    });
  }

  return results.sort((a, b) => compareStrings(a.name, b.name) || compareStrings(a.version, b.version));
}

interface LicenseTextGroup {
  text: string;
  components: { name: string; version: string; filename: string; source: string; isLicenseText: boolean }[];
}

export function groupLicenseTexts(components: ComponentInfo[]): {
  groupedTexts: LicenseTextGroup[];
  componentsWithoutFiles: ComponentInfo[];
} {
  const textMap = new Map<string, { name: string; version: string; filename: string; source: string; isLicenseText: boolean }[]>();
  const componentsWithoutFiles: ComponentInfo[] = [];

  for (const c of components) {
    const usableLicenseFiles = c.licenseFiles
      .map((f) => ({ ...f, content: f.content.replace(/\r\n/g, '\n') }))
      .filter((f) => f.content.trim().length > 0);
    if (usableLicenseFiles.length === 0) {
      componentsWithoutFiles.push(c);
    }

    const allFiles = [
      ...usableLicenseFiles.map((f) => ({ ...f, isLicenseText: true })),
      ...c.noticeFiles
        .map((f) => ({ ...f, content: f.content.replace(/\r\n/g, '\n'), isLicenseText: false }))
        .filter((f) => f.content.trim().length > 0),
    ];
    for (const f of allFiles) {
      const content = f.content.trim();
      if (!textMap.has(content)) {
        textMap.set(content, []);
      }
      textMap.get(content)!.push({
        name: c.name,
        version: c.version,
        filename: f.filename,
        source: c.source,
        isLicenseText: f.isLicenseText,
      });
    }
  }

  const groupedTexts: LicenseTextGroup[] = [];
  for (const [text, comps] of textMap) {
    // Sort components within group deterministically
    comps.sort(
      (a, b) =>
        compareStrings(a.name, b.name) ||
        compareStrings(a.version, b.version) ||
        compareStrings(a.filename, b.filename)
    );
    groupedTexts.push({ text, components: comps });
  }

  // Sort groups deterministically by their first component name, version, and filename
  groupedTexts.sort((a, b) => {
    const ca = a.components[0];
    const cb = b.components[0];
    return (
      compareStrings(ca.name, cb.name) ||
      compareStrings(ca.version, cb.version) ||
      compareStrings(ca.filename, cb.filename)
    );
  });

  return { groupedTexts, componentsWithoutFiles };
}

/**
 * The supplier and copyright line of the SPDX document a package ships in
 * place of a license text (LICENSE.spdx), or null without one.
 */
export function spdxDocumentAttribution(comp: ComponentInfo): { author: string | null; notice: string | null; file: string } | null {
  const doc = comp.noticeFiles.find((f) => /^SPDXVersion:/.test(f.content));
  if (!doc) return null;
  const field = (name: string) => doc.content.match(new RegExp(`^${name}:\\s*(.+)$`, 'm'))?.[1].trim() ?? null;
  const author = field('PackageSupplier')?.replace(/^(?:Organization|Person):\s*/, '') || null;
  const copyright = field('PackageCopyrightText');
  const notice = copyright && /[A-Za-z0-9]/.test(copyright) && !/^NOASSERTION|^NONE$/.test(copyright)
    ? (/^(?:copyright\b|\(c\)|©)/i.test(copyright) ? copyright : `Copyright ${copyright}`)
    : null;
  return { author, notice, file: doc.filename };
}

/**
 * Code a package bundles under a license it does not declare, found by the
 * SPDX-License-Identifier header of the bundled file when no license file of
 * its own ships with it: qrcode.react (ISC) bundles the QR Code generator
 * (MIT) in lib/index.js. Returns one entry per license, with the copyright
 * lines of the header and the files that carry it. A file inside a folder
 * whose license file was collected is covered by that file. An identifier
 * with no standard text (a typo such as "Apache-2.inner") is not a license
 * the notices can reproduce and is left out.
 */
export function findBundledCodeUnderOtherLicenses(
  comp: ComponentInfo,
  knownLicenseIds: Set<string>
): { license: string; notices: string[]; files: string[] }[] {
  const declared = new Set(extractSpdxLicenseIds(comp.license));
  const licensedFolders = [...comp.licenseFiles, ...comp.noticeFiles]
    .map((f) => path.posix.dirname(f.filename))
    .filter((d) => d !== '.');
  const found = new Map<string, { notices: string[]; files: string[] }>();
  const walk = (dir: string, rel: string) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const entryRel = rel ? `${rel}/${entry.name}` : entry.name;
      if (entry.isDirectory()) {
        if (!VENDORED_SKIP_DIR.test(entry.name)) walk(path.join(dir, entry.name), entryRel);
        continue;
      }
      if (!entry.isFile()) continue;
      if (licensedFolders.some((d) => entryRel.startsWith(`${d}/`))) continue;
      const buf = fs.readFileSync(path.join(dir, entry.name));
      if (buf.subarray(0, 8000).includes(0)) continue;
      const lines = buf.toString('utf8').split(/\r?\n/);
      lines.forEach((line, i) => {
        const m = line.match(/SPDX-License-Identifier:\s*(.+?)\s*(?:\*\/|-->)?\s*$/);
        if (!m) return;
        let ids: string[];
        try {
          ids = extractSpdxLicenseIds(m[1]);
        } catch {
          return;
        }
        const offered = ids.filter((id) => knownLicenseIds.has(id));
        const isOr = !/\bAND\b/.test(m[1]) && /\bOR\b|\//.test(m[1]);
        const undeclared = offered.filter((id) => !declared.has(id));
        if (undeclared.length === 0 || (isOr && undeclared.length < offered.length)) return;
        // The copyright lines of the same header: the lines above, up to the
        // start of the comment.
        const notices: string[] = [];
        for (let j = i - 1; j >= 0 && j >= i - 10; j--) {
          const text = lines[j].replace(/^\s*(?:\/\*+!?|\*+|\/\/+|#+)\s?/, '').trim();
          if (/^(?:copyright\b|\(c\)|©)/i.test(text)) notices.unshift(text);
          if (/^\s*\/\*/.test(lines[j])) break;
        }
        for (const id of isOr ? undeclared.slice(0, 1) : undeclared) {
          if (!found.has(id)) found.set(id, { notices: [], files: [] });
          const entryFound = found.get(id)!;
          for (const n of notices) if (!entryFound.notices.includes(n)) entryFound.notices.push(n);
          if (!entryFound.files.includes(entryRel)) entryFound.files.push(entryRel);
        }
      });
    }
  };
  walk(comp.packageDir, '');
  return [...found.keys()].sort(compareStrings).map((license) => ({ license, ...found.get(license)! }));
}

export function hasCopyrightHolderInFiles(comp: ComponentInfo): boolean {
  const boilerplateRegex =
    /above copyright|copyright holders|copyright notice|copyright owner|copyright license/i;
  // A keyword followed by a year (after an optional (c) or ©), anywhere on the line.
  const datedRegex = /(?:copyrights?|\(c\)|©)\s*(?:\(c\)|©)?\s*[`'"\[\(<]*[0-9]{4}/i;
  // Without a year, the keyword must begin the line (after a comment leader)
  // and be followed by a name: any name after an explicit (c) or ©
  // ("Copyright (c) zune-image developers", "Copyright (c) .NET Foundation"),
  // a capitalized one after the bare keyword, which keeps prose such as
  // "Copyright has been waived" out. A bare "(c)" at the start
  // of a line is a list item in license texts (Apache-2.0 4(c) "(c) You must
  // retain ..."), so it counts only with a year, above.
  const lead = String.raw`^\s*(?:(?:\/\/+|\/\*+|\*+|#+|--+|;+)\s*)?`;
  const namedRegex = new RegExp(
    lead +
      String.raw`(?:[Pp]ortions(?:\s+of)?\s+|[Oo]riginal\s+[Ww]ork:?\s*)?(?:(?:[Cc]opyrights?|COPYRIGHTS?)\s*(?:\([cC]\)|©)\s*[\x60'"\[\(<]*[A-Za-z0-9.]|(?:[Cc]opyrights?|COPYRIGHTS?|©|SPDX-FileCopyrightText:|PackageCopyrightText:)\s*[\x60'"\[\(<]*[A-Z0-9])`
  );
  // Statements that name the holders without a name after the keyword.
  const statementRegex = new RegExp(
    lead +
      String.raw`(?:(?:\d+[.)]\s+)?all\s+copyrights?\s+to\s|copyrights?\s+in\s.+\s(?:are|is)\s+(?:retained|owned|held)\s+by\s)`,
    'i'
  );

  // A notice broken after its marker ("..., Copyright (c)" then "2015 The Rust
  // Project Developers") is read with the line that continues it.
  const joinBrokenNotices = (content: string) =>
    content
      .split(/\r?\n/)
      .map((l, i, all) =>
        /(?:copyrights?|\(c\)|©)\s*(?:\(c\)|©)?\s*$/i.test(l) && i + 1 < all.length ? `${l} ${all[i + 1].trim()}` : l
      );
  return [...comp.licenseFiles, ...comp.noticeFiles].some((f) =>
    joinBrokenNotices(f.content)
      .some(
        (l) =>
          !boilerplateRegex.test(l) && (datedRegex.test(l) || namedRegex.test(l) || statementRegex.test(l))
      )
  );
}

export function generateNoticesMarkdown(
  cargoDeps: ComponentInfo[],
  npmDeps: ComponentInfo[],
  spdxLicensesFile = SPDX_STANDARD_LICENSES_FILE,
  nonFreeExceptions: NonFreeException[] = KNOWN_NON_FREE_EXCEPTIONS
): string {
  // Verify all pinned fallback attributions declare a non-empty source
  for (const [key, attr] of Object.entries(PINNED_FALLBACK_ATTRIBUTIONS)) {
    if (!attr.source || attr.source.trim().length === 0) {
      throw new Error(`Entry ${key} in PINNED_FALLBACK_ATTRIBUTIONS must have a non-empty 'source' field`);
    }
  }

  const sortedCargoDeps = [...cargoDeps].sort(
    (a, b) => compareStrings(a.name, b.name) || compareStrings(a.version, b.version)
  );
  const sortedNpmDeps = [...npmDeps].sort(
    (a, b) => compareStrings(a.name, b.name) || compareStrings(a.version, b.version)
  );

  // Verify declared non-free exceptions are present when analyzing the full dependency tree
  if (
    nonFreeExceptions.length > 0 &&
    (sortedCargoDeps.length > 10 || sortedCargoDeps.some((c) => c.name === 'tauri'))
  ) {
    for (const exc of nonFreeExceptions) {
      const found = sortedCargoDeps.some((c) => c.name === exc.crateName && c.version === exc.version);
      if (!found) {
        throw new Error(
          `Known non-free exception crate ${exc.crateName}@${exc.version} is declared but missing from Cargo dependencies`
        );
      }
    }
  }

  // Scan all components for unlisted non-free restriction clauses (spanning potential line breaks)
  const nonFreePattern = /(re-create[\s\S]*?compression algorithm|cannot be used to develop[\s\S]*?rar)/i;
  for (const comp of [...sortedCargoDeps, ...sortedNpmDeps]) {
    const isDeclared = nonFreeExceptions.some(
      (exc) => exc.crateName === comp.name && exc.version === comp.version
    );
    if (!isDeclared) {
      for (const f of [...comp.licenseFiles, ...comp.noticeFiles]) {
        if (nonFreePattern.test(f.content)) {
          throw new Error(
            `Component ${comp.name}@${comp.version} (file: ${f.filename}) contains non-free restriction clauses but is not registered in KNOWN_NON_FREE_EXCEPTIONS`
          );
        }
      }
    }
  }

  const lines: string[] = [];
  lines.push('# Third-Party Software Notices and Information');
  lines.push('');
  lines.push('This application incorporates third-party software components licensed under open-source and respective third-party terms.');
  lines.push('This document provides the required copyright notices, permission notices, and license texts');
  lines.push('for all production dependencies bundled with AeroFTP distribution packages:');
  lines.push('- **Linux packages:** deb, rpm, AppImage, snap');
  lines.push('- **macOS & Windows packages:** DMG, MSI (bundled resources)');
  lines.push('- *(Flatpak packaging is maintained separately in the FlatPark repository; `/app/share/doc/...` is available when installed)*');
  lines.push('');
  lines.push('AeroFTP is licensed under the **GNU General Public License v3.0 or later** (GPL-3.0-or-later).');
  lines.push('All bundled components are distributed in accordance with their respective licensing terms.');
  lines.push('');

  const allComponents = [...sortedCargoDeps, ...sortedNpmDeps];
  const compMap = new Map<string, ComponentInfo>();
  for (const c of allComponents) {
    compMap.set(`${c.source}:${c.name}@${c.version}`, c);
  }
  const { groupedTexts, componentsWithoutFiles } = groupLicenseTexts(allComponents);

  const activeExceptions = nonFreeExceptions.filter((exc) =>
    sortedCargoDeps.some((c) => c.name === exc.crateName && c.version === exc.version)
  );

  if (activeExceptions.length > 0) {
    lines.push('---');
    lines.push('');
    lines.push('## Licensing Exceptions');
    lines.push('');
    for (const exc of activeExceptions) {
      let entryIndex = -1;
      for (let i = 0; i < groupedTexts.length; i++) {
        const match = groupedTexts[i].components.some(
          (c) => c.name === exc.crateName && c.filename === exc.sourceFile
        );
        if (match) {
          entryIndex = i + 1;
          break;
        }
      }
      if (entryIndex === -1) {
        throw new Error(
          `Could not locate notice entry for declared non-free exception ${exc.crateName} (file: ${exc.sourceFile})`
        );
      }
      lines.push(
        `- **UnRAR (\`${exc.crateName}\`):** ${exc.facts} For the full text of the UnRAR license, see [Notice Entry ${entryIndex}](#notice-${exc.crateName.replace(/_/g, '-')}).`
      );
    }
    lines.push('');
  }

  lines.push('---');
  lines.push('');
  lines.push('## Summary of Components');
  lines.push('');
  lines.push(`- **Rust (Cargo) Production Crates:** ${sortedCargoDeps.length}`);
  lines.push(`- **Node.js / Web (npm) Production Packages:** ${sortedNpmDeps.length}`);
  lines.push(`- **Total Bundled Components:** ${sortedCargoDeps.length + sortedNpmDeps.length}`);
  lines.push('');
  lines.push('---');
  lines.push('');
  lines.push('## Rust Components (Cargo)');
  lines.push('');
  lines.push('| Crate | Version | License (SPDX) | Repository / Homepage |');
  lines.push('|---|---|---|---|');
  for (const dep of sortedCargoDeps) {
    const repo = dep.repository ? `[Link](${dep.repository})` : 'n/a';
    lines.push(`| ${dep.name} | ${dep.version} | ${dep.license} | ${repo} |`);
  }
  lines.push('');
  lines.push('---');
  lines.push('');
  lines.push('## Frontend & Web Components (npm Production Dependencies)');
  lines.push('');
  lines.push('| Package | Version | License (SPDX) | Repository / Homepage |');
  lines.push('|---|---|---|---|');
  for (const dep of sortedNpmDeps) {
    const repo = dep.repository ? `[Link](${dep.repository})` : 'n/a';
    lines.push(`| ${dep.name} | ${dep.version} | ${dep.license} | ${repo} |`);
  }
  lines.push('');
  lines.push('---');
  lines.push('');
  lines.push('## Third-Party License Texts, Copyright Notices, and Files');
  lines.push('');
  lines.push('The following sections reproduce the verbatim license texts, copyright notices, and Apache NOTICE files');
  lines.push('from the bundled dependencies as extracted from their source distributions.');
  lines.push('');

  function makeCodeBlock(content: string, lang = 'text'): string {
    const matches = content.match(/`+/g);
    let maxBackticks = 2;
    if (matches) {
      for (const m of matches) {
        if (m.length > maxBackticks) maxBackticks = m.length;
      }
    }
    const fence = '`'.repeat(maxBackticks + 1);
    return `${fence}${lang}\n${content}\n${fence}`;
  }

  const seenAnchorIds = new Set<string>();
  let groupIndex = 1;
  for (const group of groupedTexts) {
    const newAnchors: string[] = [];
    // A component's anchor leads to its license text, never to a notice file
    // or an SPDX document that only names the license.
    for (const comp of group.components.filter((c) => c.isLicenseText)) {
      const anchorId = `notice-${comp.name.replace(/_/g, '-')}`;
      if (!seenAnchorIds.has(anchorId)) {
        seenAnchorIds.add(anchorId);
        newAnchors.push(`<a id="${anchorId}"></a>`);
      }
    }
    if (newAnchors.length > 0) {
      lines.push(newAnchors.join('\n'));
      lines.push('');
    }
    lines.push(`### Notice Entry ${groupIndex}`);
    lines.push('');
    lines.push('**Used by component(s):**');
    for (const comp of group.components) {
      lines.push(`- \`${comp.name}@${comp.version}\` (${comp.source}, file: \`${comp.filename}\`)`);
      const fullComp = compMap.get(`${comp.source}:${comp.name}@${comp.version}`);
      if (fullComp && fullComp.licenseFiles.length > 0 && !hasCopyrightHolderInFiles(fullComp)) {
        const excluded = [...fullComp.licenseFiles, ...fullComp.noticeFiles].map((f) => f.filename);
        const pkgNotices = findPackageCopyrightNotices(fullComp.packageDir, excluded);
        if (pkgNotices.length > 0) {
          for (const n of pkgNotices) {
            lines.push(`  - ${n.notice} (${n.file})`);
          }
        } else {
          lines.push(`  - No copyright notice is shipped in the package`);
        }
      }
    }
    lines.push('');
    lines.push(makeCodeBlock(group.text));
    lines.push('');
    groupIndex++;
  }

  if (componentsWithoutFiles.length > 0) {
    if (!fs.existsSync(spdxLicensesFile)) {
      throw new Error(
        `Standard SPDX licenses file is required but missing at ${spdxLicensesFile}`
      );
    }

    const standardLicenses: Record<string, string> = JSON.parse(
      fs.readFileSync(spdxLicensesFile, 'utf8')
    );

    lines.push('---');
    lines.push('');
    lines.push('## Standard License Texts for Components without Dedicated License Files');
    lines.push('');
    lines.push('The following crates declare standard open-source licenses in their crate manifests without bundling');
    lines.push('a dedicated physical license text file in their published crate archives.');
    lines.push('In accordance with licensing requirements, the standard verbatim text for each applicable license is');
    lines.push(
      'provided below. Authors are listed as declared in package manifests or reviewed attributions; copyright notices, where available, reproduce upstream repository declarations.'
    );
    lines.push('');

    // Map each component to its required standard license texts using exact parsed SPDX identifiers
    const licenseToComponents = new Map<string, ComponentInfo[]>();
    for (const comp of componentsWithoutFiles) {
      const key = `${comp.name}@${comp.version}`;
      const authorStr = (comp.authors && comp.authors.length > 0)
        ? comp.authors.join(', ')
        : PINNED_FALLBACK_ATTRIBUTIONS[key]?.author ?? spdxDocumentAttribution(comp)?.author;
      if (!authorStr) {
        throw new Error(
          `Component ${key} has no manifest authors and is missing reviewed attribution in PINNED_FALLBACK_ATTRIBUTIONS`
        );
      }

      const ids = extractSpdxLicenseIds(comp.license);
      if (ids.length === 0) {
        throw new Error(
          `Component ${comp.name}@${comp.version} has no extractable license identifier in '${comp.license}'`
        );
      }
      const hasAnd = comp.license.includes(' AND ');
      const isOrExpr = !hasAnd && (comp.license.includes(' OR ') || comp.license.includes('/'));
      const mappedIds = ids.map((id) => (id === 'LGPL-2.1' || id === 'LGPL-2.1+' ? 'LGPL-2.1-or-later' : id));
      const validStandardIds = mappedIds.filter((id) => Boolean(standardLicenses[id]));

      let idsToInclude: string[];
      if (isOrExpr && validStandardIds.length > 0) {
        idsToInclude = validStandardIds;
      } else {
        idsToInclude = mappedIds;
      }

      for (const id of idsToInclude) {
        if (!standardLicenses[id]) {
          throw new Error(
            `Component ${comp.name}@${comp.version} requires standard license text for '${id}', but no entry exists in ${spdxLicensesFile}`
          );
        }
        if (!licenseToComponents.has(id)) {
          licenseToComponents.set(id, []);
        }
        licenseToComponents.get(id)!.push(comp);
      }
    }

    // Sort license keys deterministically
    const sortedLicenseKeys = Array.from(licenseToComponents.keys()).sort(compareStrings);
    for (const licKey of sortedLicenseKeys) {
      const comps = licenseToComponents.get(licKey)!;
      // Sort components within group deterministically
      comps.sort((a, b) => compareStrings(a.name, b.name) || compareStrings(a.version, b.version));

      const standardAnchors: string[] = [];
      for (const comp of comps) {
        const anchorId = `notice-${comp.name.replace(/_/g, '-')}`;
        if (!seenAnchorIds.has(anchorId)) {
          seenAnchorIds.add(anchorId);
          standardAnchors.push(`<a id="${anchorId}"></a>`);
        }
      }
      if (standardAnchors.length > 0) {
        lines.push(standardAnchors.join('\n'));
        lines.push('');
      }
      lines.push(`### Standard License: ${licKey}`);
      lines.push('');
      lines.push('Authors for the following components as listed in package manifests or reviewed attributions:');
      for (const comp of comps) {
        const key = `${comp.name}@${comp.version}`;
        const spdxDoc = spdxDocumentAttribution(comp);
        const author = (comp.authors && comp.authors.length > 0)
          ? comp.authors.join(', ')
          : PINNED_FALLBACK_ATTRIBUTIONS[key]?.author ?? spdxDoc?.author;
        lines.push(`- \`${comp.name}@${comp.version}\` - SPDX: \`${comp.license}\` (authors: ${author})`);

        const excluded = [...comp.licenseFiles, ...comp.noticeFiles].map((f) => f.filename);
        const pkgNotices = findPackageCopyrightNotices(comp.packageDir, excluded);
        if (spdxDoc?.notice) {
          lines.push(`  - ${spdxDoc.notice} (${spdxDoc.file})`);
        }
        if (pkgNotices.length > 0) {
          for (const n of pkgNotices) {
            lines.push(`  - ${n.notice} (${n.file})`);
          }
        } else if (spdxDoc?.notice) {
          // The copyright line of the SPDX document is listed above.
        } else if (comp.noticeFiles.length > 0) {
          lines.push(`  - Notice file shipped: ${comp.noticeFiles.map((f) => f.filename).join(', ')}`);
        } else {
          lines.push(`  - No copyright notice is shipped in the package`);
        }
      }
      lines.push('');

      let licText = standardLicenses[licKey];
      const hasPlaceholders =
        /Copyright \([cC]\) <(?:year|YEAR)> <(?:copyright holders|COPYRIGHT HOLDERS|author|authors|AUTHOR|AUTHORS)>/i.test(licText) ||
        /<(?:copyright holders|author)>/i.test(licText);
      if (hasPlaceholders) {
        const attributionLines: string[] = [];
        for (const comp of comps) {
          const key = `${comp.name}@${comp.version}`;
          const pinned = PINNED_FALLBACK_ATTRIBUTIONS[key];
          if (!pinned && !key.startsWith('test-')) {
            throw new Error(`Component ${key} requires an entry in PINNED_FALLBACK_ATTRIBUTIONS`);
          }
          if (pinned && (!pinned.source || pinned.source.trim().length === 0)) {
            throw new Error(`Component ${key} has no source specified in PINNED_FALLBACK_ATTRIBUTIONS`);
          }
          const author = pinned?.author || ((comp.authors && comp.authors.length > 0) ? comp.authors.join(', ') : 'Unknown Author');
          const holder = pinned?.copyrightHolder || author;

          const excluded = [...comp.licenseFiles, ...comp.noticeFiles].map((f) => f.filename);
          const pkgNotices = findPackageCopyrightNotices(comp.packageDir, excluded);

          if (pinned?.year) {
            attributionLines.push(`Copyright (c) ${pinned.year} ${holder} (${comp.name})`);
          } else if (pkgNotices.length > 0) {
            for (const n of pkgNotices) {
              attributionLines.push(`${n.notice} (${comp.name})`);
            }
          } else if (comp.noticeFiles.length > 0) {
            attributionLines.push(`Notice file shipped: ${comp.noticeFiles.map((f) => f.filename).join(', ')} (${comp.name})`);
          } else {
            const srcStr = pinned?.source ? ` per ${pinned.source}` : '';
            attributionLines.push(`No copyright notice is shipped upstream; authors${srcStr}: ${author} (${comp.name})`);
          }
        }
        licText = licText.replace(
          /Copyright \([cC]\) <(?:year|YEAR)> <(?:copyright holders|COPYRIGHT HOLDERS|author|authors|AUTHOR|AUTHORS)>/gi,
          attributionLines.join('\n')
        );
        if (/<(?:copyright holders|author)>/i.test(licText) || /Copyright \([cC]\) <(?:year|YEAR)>/i.test(licText)) {
          throw new Error(`Standard license text for ${licKey} contains unpopulated placeholders (<copyright holders>, <year>, or <author>)`);
        }
      }

      lines.push(makeCodeBlock(licText));
      lines.push('');
    }
  }

  // Code bundled under a license its package does not declare.
  const bundledComponents = allComponents.filter((c) => c.packageDir);
  if (bundledComponents.length > 0) {
    if (!fs.existsSync(spdxLicensesFile)) {
      throw new Error(`Standard SPDX licenses file is required but missing at ${spdxLicensesFile}`);
    }
    const standardLicenses: Record<string, string> = JSON.parse(fs.readFileSync(spdxLicensesFile, 'utf8'));
    const byLicense = new Map<string, string[]>();
    for (const comp of bundledComponents) {
      for (const b of findBundledCodeUnderOtherLicenses(comp, new Set(Object.keys(standardLicenses)))) {
        if (!byLicense.has(b.license)) byLicense.set(b.license, []);
        byLicense.get(b.license)!.push(
          `- \`${comp.name}@${comp.version}\` (${comp.source}, SPDX: \`${comp.license}\`) bundles code under ${b.license} in ${b.files.map((f) => `\`${f}\``).join(', ')}`,
          ...(b.notices.length > 0 ? b.notices.map((n) => `  - ${n}`) : ['  - No copyright notice is shipped with the bundled code'])
        );
      }
    }
    if (byLicense.size > 0) {
      lines.push('## Bundled Code Under Other Licenses');
      lines.push('');
      lines.push('These components bundle code from another project under a license they do not declare themselves. The bundled files name that license in their SPDX-License-Identifier header and ship no license file of their own, so the standard text is reproduced with the copyright lines of the header.');
      lines.push('');
      for (const licKey of [...byLicense.keys()].sort(compareStrings)) {
        lines.push(`### Bundled Code: ${licKey}`);
        lines.push('');
        lines.push(...byLicense.get(licKey)!);
        lines.push('');
        lines.push(makeCodeBlock(standardLicenses[licKey]));
        lines.push('');
      }
    }
  }

  const output = lines.join('\n') + '\n';
  if (output.includes('<copyright holders>') || output.includes('see repository maintainers')) {
    throw new Error('Generated notices contain unpopulated placeholders (<copyright holders> or see repository maintainers)');
  }

  // Exclude fenced code blocks from anchor validation so license texts containing id="..." don't trigger false positives.
  // Match any markdown fenced block starting with 3 or more backticks and ending with a matching-length closing fence.
  const proseOutsideCodeBlocks = output.replace(/(?:^|\n)(`{3,})[^\n]*\n[\s\S]*?\n\1(?:\n|$)/g, '\n');
  const idMatches = Array.from(proseOutsideCodeBlocks.matchAll(/<a id="([^"]+)">/g)).map((m) => m[1]);
  const seenIds = new Set<string>();
  for (const id of idMatches) {
    if (seenIds.has(id)) {
      throw new Error(`Duplicate anchor id="${id}" detected in generated notices`);
    }
    seenIds.add(id);
  }

  return output;
}

function main() {
  const args = process.argv.slice(2);
  const isCheck = args.includes('--check');
  const isWrite = args.includes('--write');

  if (!isCheck && !isWrite) {
    console.error('Usage: gen-third-party-notices.ts [--check | --write]');
    process.exit(1);
  }

  const cargoDeps = collectCargoDependencies();
  const npmDeps = collectNpmDependencies();

  // Audit every dependency license against GPL-3.0 compatibility
  const incompatible: ComponentInfo[] = [];
  for (const dep of [...cargoDeps, ...npmDeps]) {
    if (!isGplCompatible(dep.license)) {
      incompatible.push(dep);
    }
  }

  if (incompatible.length > 0) {
    console.error(`[ERROR] Found ${incompatible.length} components with GPL-incompatible licenses:`);
    for (const inc of incompatible) {
      console.error(`  - [${inc.source}] ${inc.name}@${inc.version}: ${inc.license}`);
    }
    process.exit(1);
  }

  const generated = generateNoticesMarkdown(cargoDeps, npmDeps);

  if (isWrite) {
    fs.writeFileSync(NOTICES_FILE, generated, 'utf8');
    console.log(`Successfully written ${cargoDeps.length} Cargo and ${npmDeps.length} npm notices to ${NOTICES_FILE}`);
  } else if (isCheck) {
    if (!fs.existsSync(NOTICES_FILE)) {
      console.error(`ERROR: ${NOTICES_FILE} does not exist. Run with --write to generate it.`);
      process.exit(1);
    }
    const existing = fs.readFileSync(NOTICES_FILE, 'utf8').replace(/\r\n/g, '\n');
    if (existing !== generated) {
      console.error(`ERROR: ${NOTICES_FILE} is out of date.`);
      console.error(formatDiff(existing, generated, 40));
      console.error(`Run 'npm run gen:licenses' to update it.`);
      process.exit(1);
    }
    console.log(`OK: ${NOTICES_FILE} is up to date (${cargoDeps.length} Cargo, ${npmDeps.length} npm).`);
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  main();
}
