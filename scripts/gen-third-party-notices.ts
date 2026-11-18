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
  licenseFiles: { filename: string; content: string; vendored?: boolean }[];
  noticeFiles: { filename: string; content: string; vendored?: boolean }[];
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
  'whisper-rs-sys@0.15.0': {
    author: 'the whisper-rs project (codeberg.org/tazz4843/whisper-rs)',
    source: 'https://codeberg.org/tazz4843/whisper-rs (Cargo.toml repository; the crate declares no authors, and its only license file, whisper.cpp/LICENSE, is the license of the vendored whisper.cpp)',
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
  // Permissive texts met in bundled source headers (libssh2): the FSF
  // Unlimited License, the curl license (an MIT/X variant) and the SAX
  // public domain dedication.
  'FSFULLR',
  'curl',
  'SAX-PD-2.0',
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

/**
 * The choices an SPDX expression offers, each the terms it requires together:
 * "MIT OR (Apache-2.0 AND BSD-3-Clause)" gives [["MIT"], ["Apache-2.0",
 * "BSD-3-Clause"]], and a conjunct binds every choice, whatever whitespace
 * separates it ("(MIT OR Apache-2.0)<tab>AND<tab>BSD-3-Clause-Clear" gives two
 * choices, both with BSD-3-Clause-Clear). A term keeps its exception
 * ("Apache-2.0 WITH LLVM-exception"). Null when the expression does not parse.
 */
export function spdxAlternatives(expr: string): string[][] | null {
  const tokens = tokenizeSpdx(expr);
  if (tokens.length === 0) return null;
  let pos = 0;
  const isOp = (t: string | undefined, op: string) => t !== undefined && t.toUpperCase() === op;
  const and = (a: string[][], b: string[][]) => a.flatMap((x) => b.map((y) => [...new Set([...x, ...y])]));
  function parseOr(): string[][] | null {
    let res = parseAnd();
    while (res && isOp(tokens[pos], 'OR')) {
      pos++;
      const rhs = parseAnd();
      res = rhs && [...res, ...rhs];
    }
    return res;
  }
  function parseAnd(): string[][] | null {
    let res = parseFactor();
    while (res && isOp(tokens[pos], 'AND')) {
      pos++;
      const rhs = parseFactor();
      res = rhs && and(res, rhs);
    }
    return res;
  }
  function parseFactor(): string[][] | null {
    const t = tokens[pos];
    if (t === undefined || t === ')') return null;
    if (t === '(') {
      pos++;
      const inner = parseOr();
      if (!inner || tokens[pos] !== ')') return null;
      pos++;
      return inner;
    }
    if (OPERATOR_TOKENS.has(t.toUpperCase())) return null;
    pos++;
    if (isOp(tokens[pos], 'WITH')) {
      const ex = tokens[pos + 1];
      if (ex === undefined || OPERATOR_TOKENS.has(ex.toUpperCase())) return null;
      pos += 2;
      return [[`${t} WITH ${ex}`]];
    }
    return [[t]];
  }
  const result = parseOr();
  return result && pos === tokens.length ? result : null;
}

// Folders whose license files describe no code that ships: tests, fixtures,
// examples, CI, and node_modules (its packages are components of their own).
const VENDORED_SKIP_DIR = /^(?:\.git|\.github|node_modules|tests?|testdata|fixtures?|examples?|benches)$/i;
// A license or notice file by name, not a source file named after one
// (copying.rs, copyright.mjs, copyright.pm, license.yml, dompurify's
// src/license_header, a lowercase name after the separator).
const VENDORED_LICENSE_FILE =
  /^(?:licen[sc]e|copying|unlicense|notice|copyright)(?:[-_][^.]*)?(?:\.(?:txt|md|markdown|rst|info-zip|apache|mit|bsd|gplv[23]|lgplv2\.1|unicode|spdx))?$/i;
// A source or data file by its extension: never a license or notice text.
const SOURCE_FILE = /\.(?:[cm]?[jt]sx?|rs|c|h|cc|cpp|hpp|py|rb|go|java|kt|swift|sh|pl|pm|php|json|ya?ml|toml|lock|css|map)$/i;
// Permission text in a source header (see findBundledCodeUnderOtherLicenses).
const PERMISSION_TEXT =
  /permission to use|permission (?:is|are) (?:hereby )?granted|redistribution and use|\byou may (?:freely )?(?:use|copy|modify|distribute|redistribute)\b|\bmay be (?:freely )?(?:used|copied|modified|distributed|redistributed)\b/i;
// A REUSE license folder (LICENSES/MIT.txt, LICENSES/BSD-3-Clause.txt) names
// its files after the license, not with a license word.
const LICENSE_DIR = /^licen[sc]es?$/i;
// Inside such a folder a file is license text when it is a text document;
// a source folder named license/ (src/license/mod.rs) holds code, and a
// README there describes the folder.
const LICENSE_DIR_TEXT_FILE = /^(?!(?:readme|changelog|index|makefile)(?:\.|$))(?:[^.]+(?:\.[^.]+)*\.(?:txt|md|markdown|rst)|[^.]+)$/i;
// One rule for a license folder at the root and inside vendored code:
// LICENSES/MIT.txt is a text, LICENSES/README.md and LICENSES/build.rs are not.
const isLicenseDirText = (name: string) => LICENSE_DIR_TEXT_FILE.test(name) && !SOURCE_FILE.test(name);

// Directory entries in one order on every platform: Windows returns them in
// the order the file system keeps, so a walk that lists files or holders in
// enumeration order would generate different notices from the same contents.
const readDirSorted = (dir: string): fs.Dirent[] =>
  fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => compareStrings(a.name, b.name));

export function readPackageFiles(
  dir: string,
  componentId: string,
  declaredLicenseFile?: string
): {
  licenseFiles: { filename: string; content: string; vendored?: boolean }[];
  noticeFiles: { filename: string; content: string; vendored?: boolean }[];
} {
  const licenseFiles: { filename: string; content: string; vendored?: boolean }[] = [];
  const noticeFiles: { filename: string; content: string; vendored?: boolean }[] = [];

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
    // The declared file is the crate's own text, so it must be inside the
    // crate: `cargo package` copies a license-file from outside the package
    // into its root, so a registry crate always has it there. A path that
    // leaves the crate (`../LICENSE`, an absolute path, a symlink to
    // elsewhere) would read a text the crate does not ship; it fails, and a
    // license shared by a workspace is copied into the crate as cargo does.
    const inside = path.relative(fs.realpathSync(dir), fs.realpathSync(declaredPath));
    if (inside === '..' || inside.startsWith(`..${path.sep}`) || path.isAbsolute(inside)) {
      throw new Error(
        `Crate ${componentId} declared license_file "${declaredLicenseFile}", which is outside the crate directory ${dir}: copy the license into the crate, as cargo package does, instead of reading it from elsewhere`
      );
    }
    if (!fs.statSync(declaredPath).isFile()) {
      throw new Error(`Crate ${componentId} declared license_file "${declaredLicenseFile}", which is not a file at ${declaredPath}`);
    }
    const filename = path.basename(declaredLicenseFile);
    const content = fs.readFileSync(declaredPath, 'utf8').replace(/\r\n/g, '\n').trim();
    licenseFiles.push({ filename, content });
  }

  // 2. Scan direct entries in package directory
  const entries = readDirSorted(dir);
  for (const entry of entries) {
    if (entry.isFile()) {
      const f = entry.name;
      const fullPath = path.join(dir, f);
      if (declaredLicenseFile && path.resolve(fullPath) === path.resolve(dir, declaredLicenseFile)) {
        continue;
      }
      // A source file named after a license word (license.js, copyright.rs)
      // is code; LICENSE.Apache_2.0, LICENSE-MIT and NOTICES.md stay texts.
      if (SOURCE_FILE.test(f)) continue;
      if (/^(notice|copyright)/i.test(f)) {
        noticeFiles.push({ filename: f, content: fs.readFileSync(fullPath, 'utf8').replace(/\r\n/g, '\n').trim() });
      } else if (/^(licen[sc]e|copying|unlicense)/i.test(f)) {
        licenseFiles.push({ filename: f, content: fs.readFileSync(fullPath, 'utf8').replace(/\r\n/g, '\n').trim() });
      }
    } else if (entry.isDirectory() && LICENSE_DIR.test(entry.name)) {
      // 3. Scan REUSE / standard license subdirectories (LICENSES/, licenses/,
      // license/, licence/) with the rule of a nested one (isLicenseDirText).
      const subDir = path.join(dir, entry.name);
      try {
        const subEntries = readDirSorted(subDir);
        for (const sub of subEntries) {
          if (sub.isFile() && isLicenseDirText(sub.name)) {
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
          for (const sub of readDirSorted(d)) {
            const filename = `${rel}/${sub.name}`;
            if (sub.isDirectory()) {
              if (!VENDORED_SKIP_DIR.test(sub.name)) walkVendored(path.join(d, sub.name), filename);
            } else if (
              sub.isFile() &&
              ((VENDORED_LICENSE_FILE.test(sub.name) && !/^(?:licen[sc]e|copying)[-_][a-z]+$/.test(sub.name)) ||
                (LICENSE_DIR.test(path.basename(d)) && isLicenseDirText(sub.name))) &&
              // The declared license_file is the package's own, read by step 1.
              !(declaredLicenseFile && path.resolve(d, sub.name) === path.resolve(dir, declaredLicenseFile))
            ) {
              const content = fs.readFileSync(path.join(d, sub.name), 'utf8').replace(/\r\n/g, '\n').trim();
              if (content.length === 0) continue;
              // A vendored file describes the vendored code, not the package.
              if (/^(notice|copyright)/i.test(sub.name)) {
                noticeFiles.push({ filename, content, vendored: true });
              } else {
                licenseFiles.push({ filename, content, vendored: true });
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

// A copyright line with a template slot is a template, not a notice: a
// generator script's "Copyright 1995-$YEAR The OpenSSL Project Authors", a
// format string's "Copyright (c) 1993-%d Alexander Roshal", a license
// template's "Copyright (c) <year> <owner>". Only attribution lists drop it;
// a license text is reproduced as shipped, placeholders included.
const TEMPLATE_NOTICE =
  /\$\{?[A-Za-z_]+\}?|%[0-9]*[dsu]\b|<(?:year|yyyy|owner|authors?|copyright holders?)>|\[(?:year|yyyy|fullname|owner|name of copyright owner)\]|\{(?:year|yyyy|owner|authors?)\}/i;

/**
 * Scans locked package sources (header comments of source files, README, and AUTHORS/COPYRIGHT/NOTICE files)
 * for copyright notices. Deduplicates notices and returns them sorted deterministically.
 * Excludes files already collected by readPackageFiles.
 */
// A notice made only of its marker and years, with no holder.
const HOLDERLESS_NOTICE = /^(?:copyright\s*)?(?:(?:\([cC]\)|©)\s*)?[0-9][0-9,\-\s]*$/i;
// The first words of license prose, which never continue a holder.
const HOLDER_CONTINUATION_PROSE = /^(?:this|the|these|permission|licensed|all rights|redistribution|you|see|under|written|portions|based|it|use|any)\b/i;

// A notice that ends at its years continues with its holder on the next line
// of the same comment ("Copyright (C) 1987,88,...,2006" then "Free Software
// Foundation, Inc."); the license prose after it is no holder.
function withHolderContinuation(notice: string, nextLine: string | undefined): string {
  if (!HOLDERLESS_NOTICE.test(notice) || nextLine === undefined) return notice;
  const next = nextLine.trim();
  if (!/^[A-Z]/.test(next) || /^(?:copyright\b|\(c\)|©)/i.test(next) || HOLDER_CONTINUATION_PROSE.test(next)) return notice;
  return `${notice} ${next}`;
}

export function findPackageCopyrightNotices(
  packageDir?: string,
  excludedFiles: string[] = [],
  excludedDirs: string[] = []
): { notice: string; file: string }[] {
  if (!packageDir || !fs.existsSync(packageDir)) return [];
  const files: { full: string; rel: string }[] = [];

  // Every file the package ships, at any depth, as the property test in
  // test-third-party-notices.mjs greps it; nested node_modules are other
  // components and are scanned as themselves.
  function walk(currentDir: string, relBase: string) {
    if (!fs.existsSync(currentDir)) return;
    const entries = readDirSorted(currentDir);
    for (const e of entries) {
      const rel = relBase ? `${relBase}/${e.name}` : e.name;
      const full = path.join(currentDir, e.name);
      if (e.isFile()) {
        if (excludedFiles && excludedFiles.includes(rel)) {
          continue;
        }
        files.push({ full, rel });
      } else if (e.isDirectory() && e.name !== 'node_modules' && e.name !== '.git' && !excludedDirs.includes(rel)) {
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
    for (let li = 0; li < lines.length; li++) {
      const rawLine = lines[li];
      const field = isCopyrightFormat ? rawLine.match(/^Copyright:\s*(.*)$/i) : null;
      const continued = inCopyrightField && /^\s+\S/.test(rawLine) ? rawLine.trim() : null;
      inCopyrightField = field !== null || continued !== null;
      const fieldValue = field ? field[1].trim() : continued;
      if (fieldValue) {
        const notice = /^(?:copyright\b|\(c\)|©)/i.test(fieldValue) ? fieldValue : `Copyright ${fieldValue}`;
        if (/[A-Za-z0-9]/.test(fieldValue) && !TEMPLATE_NOTICE.test(notice) && !seenNotice.has(notice)) {
          seenNotice.add(notice);
          notices.push({ notice, file: f.rel });
        }
        continue;
      }
      const closes = stripsCloser(rawLine, false);
      let line = rawLine.replace(COMMENT_LEADER, '');
      if (closes) line = line.replace(COMMENT_CLOSER_AT_END, '');
      line = line.trim();
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
        let cleaned = (closes ? match[0].replace(FROM_COMMENT_CLOSER, '') : match[0]).replace(/(?:\\n)?"[);,\s]*$/, '').trim();
        // A notice inside a string literal or a source map ends at its first
        // escaped line break; one in a README or a man page is read as text:
        // a Markdown link keeps its words, a `<br>` and entities go, and roff
        // escapes (`2011\-2014`, `\.`) are the characters they stand for.
        cleaned = cleaned
          .replace(/\\[rn].*$/, '')
          .replace(/\[([^\]]+)\]\([^)\s]+\)/g, '$1')
          .replace(/<\/?(?:br|b|i|em|strong|p|span|sub|sup)\b[^<>@]*>/gi, '')
          .replace(/&lt;/g, '<')
          .replace(/&gt;/g, '>')
          .replace(/&amp;/g, '&')
          .replace(/\\([-.\\])/g, '$1')
          .trim();
        cleaned = cleaned.replace(/\s*\([A-Za-z0-9\.\s\-]+ [Ll]icense\)\.?$/, '');
        cleaned = cleaned.replace(/\s+See the\b.*/i, '');
        cleaned = cleaned.trim();
        const nextLine = lines[li + 1];
        cleaned = withHolderContinuation(
          cleaned,
          nextLine === undefined
            ? undefined
            : stripsCloser(nextLine, false)
              ? nextLine.replace(COMMENT_LEADER, '').replace(COMMENT_CLOSER_AT_END, '')
              : nextLine.replace(COMMENT_LEADER, '')
        );
        if (cleaned.length > 0 && !TEMPLATE_NOTICE.test(cleaned) && !seenNotice.has(cleaned)) {
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

// Whether npm installs a package whose `os` or `cpu` list is `allowed` on
// `value` (process.platform, process.arch): an entry names what it supports,
// `!name` what it excludes, and no list supports everything.
export function npmInstallsHere(allowed: string[] | undefined, value: string): boolean {
  if (!allowed || allowed.length === 0) return true;
  if (allowed.includes(`!${value}`)) return false;
  const named = allowed.filter((a) => !a.startsWith('!'));
  return named.length === 0 || named.includes(value);
}

// A manifest's author as npm writes it, "Name <email> (url)" or { name,
// email, url }, kept as Cargo keeps its authors: the name with its email, as
// "Name <email>". A url names no one, and an author with no name is none.
export function npmAuthors(manifest: { author?: unknown }): string[] | undefined {
  const a = manifest.author;
  let name = '';
  let email = '';
  if (typeof a === 'string') {
    const m = a.match(/^\s*([^<(]*?)\s*(?:<([^>]*)>)?\s*(?:\(([^)]*)\))?\s*$/);
    // Another order of the parts is kept as written.
    name = m ? m[1] : a.trim();
    email = m?.[2]?.trim() ?? '';
  } else if (a && typeof a === 'object' && !Array.isArray(a)) {
    const o = a as { name?: unknown; email?: unknown };
    if (typeof o.name === 'string') name = o.name.trim();
    if (typeof o.email === 'string') email = o.email.trim();
  }
  if (name === '') return undefined;
  return [email ? `${name} <${email}>` : name];
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
    // The files read are the installed package's: a node_modules left from
    // another lockfile would give its license files the locked version's name.
    // An alias is installed under its own directory name, so the installed
    // manifest is checked against the locked package name, not the directory.
    const installedManifest = path.join(depDir, 'package.json');
    // Every locked production package ships on some platform, and its license
    // files exist only in the install: the lockfile records the license id,
    // not the texts or the holders. A package missing from the install stops
    // the generation and is named, never dropped. npm leaves out an optional
    // one under --omit=optional or when its os/cpu do not match this platform.
    if (!fs.existsSync(depDir)) {
      if (pkgInfo.optional || pkgInfo.devOptional) {
        const only = [pkgInfo.os && `os ${pkgInfo.os.join(', ')}`, pkgInfo.cpu && `cpu ${pkgInfo.cpu.join(', ')}`].filter(Boolean);
        throw new Error(
          !npmInstallsHere(pkgInfo.os, process.platform) || !npmInstallsHere(pkgInfo.cpu, process.arch)
            ? `${pkgPath} locks the optional ${componentId}, which npm installs only for ${only.join(' and ')}, so this platform cannot read its license files: the notices cover every platform's packages, so the package needs a reviewed source for them before it ships`
            : `${pkgPath} locks the optional ${componentId}, which is not installed: the notices cover every platform's packages, so install it (npm ci without --omit=optional), then generate the notices again`
        );
      }
      throw new Error(`${pkgPath} is not installed, but package-lock.json locks ${componentId}: run npm ci, then generate the notices again`);
    }
    if (!fs.existsSync(installedManifest)) {
      throw new Error(
        `${pkgPath} has no package.json, but package-lock.json locks ${name}@${version}: run npm ci, then generate the notices again`
      );
    }
    const installed = JSON.parse(fs.readFileSync(installedManifest, 'utf8').replace(/^\uFEFF/, ''));
    if (installed.name !== name || installed.version !== version) {
      throw new Error(
        `${pkgPath} is installed as ${installed.name}@${installed.version}, but package-lock.json locks ${name}@${version}: run npm ci, then generate the notices again`
      );
    }
    const { licenseFiles, noticeFiles } = readPackageFiles(depDir, componentId);

    results.push({
      name,
      version,
      license: pkgInfo.license || 'Unknown',
      repository: pkgInfo.homepage || undefined,
      source: 'npm',
      authors: npmAuthors(installed),
      licenseFiles,
      noticeFiles,
      packageDir: depDir,
    });
  }

  return results.sort((a, b) => compareStrings(a.name, b.name) || compareStrings(a.version, b.version));
}

// A shipped file carries a standard license text when it holds this share of
// the text's word 6-grams, its attribution lines left out. A standard text
// inside a longer one (BSD-2-Clause in BSD-3-Clause, 0BSD in ISC, MIT-0 in
// MIT, from 0.94 to 0.97) has seam 6-grams where the longer text inserts its
// extra clause before the disclaimer, so the longer text alone holds none of
// them while a file that ships the shorter text (alone or beside the longer
// one) holds them. Between other texts the share is at most 0.82.
const LICENSE_TEXT_SHARE = 0.84;

function termShingles(text: string): Set<string> {
  const words = licenseTermsOf(text).toLowerCase().split(/[^a-z0-9]+/).filter(Boolean);
  const shingles = new Set<string>();
  for (let i = 0; i + 6 <= words.length; i++) shingles.add(words.slice(i, i + 6).join(' '));
  return shingles;
}

function shareOf(part: Set<string>, whole: Set<string>): number {
  let n = 0;
  for (const x of part) if (whole.has(x)) n++;
  return part.size === 0 ? 0 : n / part.size;
}

// The replaceable parts of a standard text, as its SPDX template marks them
// (`<alt match=".+">`): a file names its own holder there ("Neither the name
// of Example Corp nor ..."). Only these may differ from the standard words.
const STANDARD_TEXT_SLOTS: Record<string, string[]> = {
  'BSD-2-Clause': ['the copyright holders and contributors', 'the copyright holder or contributors'],
  'BSD-3-Clause': ['the copyright holders and contributors', 'the copyright holder or contributors', 'the copyright holder'],
  ISC: ['the author'],
  MIT: ['the authors or copyright holders'],
};
// Words a shipped text may insert inside one sentence of a standard text
// ("this permission notice (including the next paragraph) shall be
// included"), and the words a slot may hold. Between two sentences any text
// may stand: a license with an added clause still carries every standard one.
const SENTENCE_INSERT_WORDS = 8;
const SLOT_WORDS = 16;
const URL_TEXT = /<?\bhttps?:\/\/[^\s>)]+>?|\bwww\.[^\s>)]+/gi;

// Reviewed spellings of one standard word: the verb after a slot agrees
// with the holder it names ("THE AUTHORS DISCLAIM", rustls-webpki and
// untrusted, for ISC's "THE AUTHOR DISCLAIMS").
const EQUIVALENT_TERM_WORDS: Record<string, string> = { disclaims: 'disclaim' };

// The words of a license text as the terms comparison reads them: no
// attribution lines and no URLs, without list markers (`1.`, `(a)`) or
// single letters, which shipped texts number differently.
function termWords(text: string): string[] {
  return licenseTermsOf(text.replace(URL_TEXT, ' '))
    .toLowerCase()
    .split(/[^a-z0-9]+/)
    .filter((w) => w.length > 1 && !/^[0-9]+$/.test(w))
    .map((w) => EQUIVALENT_TERM_WORDS[w] ?? w);
}

// The sentences of a standard text's terms, each a list of words, with null
// where a slot stands. The title lines before the terms, and what follows
// the terms (the appendix on how to apply them, an exhibit), are no terms.
function standardSentences(id: string, text: string): (string | null)[][] {
  const terms = text.split(/END OF TERMS AND CONDITIONS|How to Apply These Terms|^\s*APPENDIX\b|^\s*Exhibit A\b/im)[0];
  const lines = terms.split('\n');
  let start = 0;
  while (start < lines.length && lines[start].trim().split(/\s+/).filter(Boolean).length < 10) start++;
  const slots = (STANDARD_TEXT_SLOTS[id] ?? []).map((s) => s.split(' ')).sort((a, b) => b.length - a.length);
  return licenseTermsOf(lines.slice(start).join('\n'))
    .split(/(?<=[.;:])\s+/)
    .map((sentence) => {
      const words = termWords(sentence);
      const out: (string | null)[] = [];
      for (let i = 0; i < words.length; ) {
        const slot = slots.find((s) => s.every((w, k) => words[i + k] === w));
        if (slot) {
          if (out[out.length - 1] !== null) out.push(null);
          i += slot.length;
        } else out.push(words[i++]);
      }
      return out;
    })
    .filter((s) => s.some((w) => w !== null));
}

/**
 * Whether a shipped file carries the whole standard text: every sentence of
 * its terms, in order, each word of a sentence within a few inserted words of
 * the one before, and other words only in a slot. Similarity finds the
 * candidate texts; this decides coverage, so a text with a clause removed
 * (Apache-2.0 without section 4(a)) still gets the standard text.
 */
export function carriesWholeStandardText(id: string, standardText: string, fileText: string): boolean {
  const file = termWords(fileText);
  const at = new Map<string, number[]>();
  file.forEach((w, i) => {
    if (!at.has(w)) at.set(w, []);
    at.get(w)!.push(i);
  });
  let from = 0;
  for (const sentence of standardSentences(id, standardText)) {
    // The file positions where the sentence so far can end, earliest first.
    let ends: number[] | null = null;
    let gap = SENTENCE_INSERT_WORDS;
    for (const word of sentence) {
      if (word === null) {
        gap = SLOT_WORDS;
        continue;
      }
      const positions = (at.get(word) ?? []).filter((p) => p >= from);
      if (ends === null) ends = positions;
      else {
        // Both lists ascend: a position follows when an end lies in
        // [p - gap - 1, p - 1].
        const next: number[] = [];
        let k = 0;
        for (const p of positions) {
          while (k < ends.length && ends[k] < p - gap - 1) k++;
          if (k < ends.length && ends[k] < p) next.push(p);
        }
        ends = next;
      }
      gap = SENTENCE_INSERT_WORDS;
      if (ends.length === 0) return false;
    }
    from = ends![0] + 1;
  }
  return true;
}

// How the text of a license with no standard text names itself, reviewed
// against the files that ship it: a grant without that name (a custom-zip
// license next to MIT AND bzip2-1.0.6) is another license, not this one.
const NON_STANDARD_LICENSE_TITLES: Record<string, RegExp> = {
  // libbz2-rs-sys: "The original program, "bzip2", the associated library "libbzip2", ..."
  'bzip2-1.0.6': /"bzip2",\s+the\s+associated\s+library\s+"libbzip2"/,
  // webpki-roots, webpki-root-certs.
  'CDLA-Permissive-2.0': /Community\s+Data\s+License\s+Agreement\s*-\s*Permissive\s*[-,]\s*Version\s+2\.0/i,
};

// Whether a text names a license: its SPDX identifier as a word
// ("(BSD-1-Clause)", "SPDX-License-Identifier: bzip2-1.0.6"), or the
// reviewed title of a license with no standard text.
export function namesLicense(id: string, text: string): boolean {
  return (
    new RegExp(`(?<![A-Za-z0-9.+-])${escapeRegExp(id)}(?![A-Za-z0-9+-]|\\.[0-9])`, 'i').test(text) ||
    (NON_STANDARD_LICENSE_TITLES[id]?.test(text) ?? false)
  );
}

// The 6-grams of each standard text, and the seam 6-grams that distinguish a
// shorter text from the longer standard texts that contain it, built once per
// standard licenses file.
const standardShingles = new WeakMap<
  Record<string, string>,
  { texts: Map<string, Set<string>>; seams: Map<string, Set<string>> }
>();

/**
 * The terms of each choice that a package's own license files leave out, for
 * the choices those files cover best. Empty when the files cover a whole
 * choice (MIT OR Apache-2.0 with LICENSE-MIT). A term is covered by a file that
 * carries its whole standard text; a term with no standard text (bzip2-1.0.6)
 * is covered by a file that carries none of the standard texts, grants
 * permission itself and names that license (namesLicense), not one that only
 * names licenses (siphasher's COPYING) nor a grant for another work.
 * The choices whose uncovered terms all have a standard text come first; with
 * none, the best covered ones are returned and the caller names the missing
 * text.
 */
function uncoveredLicenseTerms(
  choices: string[][],
  ownTexts: string[],
  standardLicenses: Record<string, string>
): string[][] {
  if (!standardShingles.has(standardLicenses)) {
    const texts = new Map(Object.entries(standardLicenses).map(([id, text]) => [id, termShingles(text)]));
    const seams = new Map<string, Set<string>>();
    for (const [id, text] of texts) {
      const longer = [...texts].filter(([other, o]) => other !== id && shareOf(text, o) >= LICENSE_TEXT_SHARE).map(([, o]) => o);
      if (longer.length > 0) {
        seams.set(id, new Set([...text].filter((x) => longer.every((o) => !o.has(x)))));
      }
    }
    standardShingles.set(standardLicenses, { texts, seams });
  }
  const { texts: standard, seams } = standardShingles.get(standardLicenses)!;
  const files = ownTexts.map(termShingles);
  // Similarity names the standard texts a file is a version of; a file covers
  // a term only when it carries that text whole (carriesWholeStandardText).
  const similar = (id: string, file: Set<string>) =>
    shareOf(standard.get(id)!, file) >= LICENSE_TEXT_SHARE &&
    (!seams.has(id) || shareOf(seams.get(id)!, file) >= 0.5);
  const carriesAt = (id: string, i: number) =>
    similar(id, files[i]) && carriesWholeStandardText(id, standardLicenses[id], ownTexts[i]);
  // A term with no standard text is covered by a file that grants permission
  // itself and names that license. A version of a standard text with a clause
  // removed is still that text, never a grant of its own.
  const grantsOwn = files.map(
    (f, i) => PERMISSION_TEXT.test(ownTexts[i]) && ![...standard.keys()].some((other) => similar(other, f))
  );
  // Each such term takes a file of its own: one grant is the text of one
  // license, even when it names two. The terms are matched to the files that
  // name them so that as many as possible get one (augmenting paths): a file
  // naming both is left to the term no other file names.
  const left = choices.map((terms) => {
    const own = terms.flatMap((term, ti) =>
      term.split(' WITH ').filter((id) => !standard.has(id)).map((id) => ({ ti, files: ownTexts.map((text, k) => k).filter((k) => grantsOwn[k] && namesLicense(id, ownTexts[k])) }))
    );
    const fileOf = new Map<number, number>();
    const assign = (n: number, seen: Set<number>): boolean => {
      for (const k of own[n].files) {
        if (seen.has(k)) continue;
        seen.add(k);
        if (!fileOf.has(k) || assign(fileOf.get(k)!, seen)) {
          fileOf.set(k, n);
          return true;
        }
      }
      return false;
    };
    const unmatched = new Set(own.map((_, n) => n).filter((n) => !assign(n, new Set())).map((n) => own[n].ti));
    return terms.filter(
      (term, ti) => unmatched.has(ti) || !term.split(' WITH ').every((id) => !standard.has(id) || files.some((_, i) => carriesAt(id, i)))
    );
  });
  if (left.some((terms) => terms.length === 0)) return [];
  const reproducible = left
    .map((terms, i) => i)
    .filter((i) => left[i].every((term) => term.split(' WITH ').every((id) => standard.has(id))));
  const pool = reproducible.length > 0 ? reproducible : left.map((_, i) => i);
  const most = Math.max(...pool.map((i) => choices[i].length - left[i].length));
  return pool.filter((i) => choices[i].length - left[i].length === most).map((i) => left[i]);
}

interface LicenseTextGroup {
  text: string;
  components: { name: string; version: string; filename: string; source: string; isLicenseText: boolean; vendored: boolean }[];
}

export function groupLicenseTexts(components: ComponentInfo[]): {
  groupedTexts: LicenseTextGroup[];
  componentsWithoutFiles: ComponentInfo[];
} {
  const textMap = new Map<string, { name: string; version: string; filename: string; source: string; isLicenseText: boolean; vendored: boolean }[]>();
  const componentsWithoutFiles: ComponentInfo[] = [];

  for (const c of components) {
    const usableLicenseFiles = c.licenseFiles
      .map((f) => ({ ...f, content: f.content.replace(/\r\n/g, '\n') }))
      .filter((f) => f.content.trim().length > 0);
    // Only the package's own license files cover its license: a vendored
    // library's COPYING does not, and the package then gets the standard text.
    if (!usableLicenseFiles.some((f) => !f.vendored)) {
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
        vendored: Boolean(f.vendored),
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
  const doc = comp.noticeFiles.find((f) => !f.vendored && /^SPDXVersion:/.test(f.content));
  if (!doc) return null;
  // A field holds one line, or several between <text> and </text>.
  const field = (name: string) => {
    const m = doc.content.match(new RegExp(`^${name}:[ \\t]*(<text>[\\s\\S]*?</text>|.+)$`, 'm'));
    return m ? m[1].replace(/^<text>|<\/text>$/g, '').replace(/\s+/g, ' ').trim() || null : null;
  };
  // NOASSERTION and NONE name no supplier, as for the copyright field.
  const supplier = field('PackageSupplier');
  const author = supplier && !/^(?:NOASSERTION|NONE)$/i.test(supplier) ? supplier.replace(/^(?:Organization|Person):\s*/, '') || null : null;
  const copyright = field('PackageCopyrightText');
  const notice = copyright && /[A-Za-z0-9]/.test(copyright) && !/^NOASSERTION|^NONE$/.test(copyright)
    ? (/^(?:copyright\b|\(c\)|©)/i.test(copyright) ? copyright : `Copyright ${copyright}`)
    : null;
  return { author, notice, file: doc.filename };
}

/**
 * The folders of a package's vendored license files: their notices belong to
 * the vendored code and are listed with its license, not with the package's.
 */
export function vendoredFolders(comp: ComponentInfo): string[] {
  return [...new Set([...comp.licenseFiles, ...comp.noticeFiles].filter((f) => f.vendored).map((f) => vendoredRoot(f.filename)))].sort(compareStrings);
}

/**
 * The folder of the vendored code a vendored license file describes: its own
 * folder, or for a file in a REUSE license folder (vendor/lib/LICENSES/MIT.txt)
 * the folder that holds it (vendor/lib).
 */
export function vendoredRoot(filename: string): string {
  const dir = path.posix.dirname(filename);
  return LICENSE_DIR.test(path.posix.basename(dir)) && dir.includes('/') ? path.posix.dirname(dir) : dir;
}

/**
 * What to say when the package's own files hold no copyright notice: with
 * vendored code, whose notices are listed with that code's license, the
 * package still ships notices, only not its own.
 */
export function noOwnNoticeLine(comp: ComponentInfo): string {
  const dirs = vendoredFolders(comp);
  return dirs.length > 0
    ? `No copyright notice of its own; the notices of its vendored code (${dirs.map((d) => `\`${d}/\``).join(', ')}) are listed with that code's license`
    : 'No copyright notice is shipped in the package';
}

/**
 * The holder a copyright line names, without the keyword, the years and a
 * trailing "All rights reserved": "Copyright (c) 2019-2024 Tauri Programme" is
 * "Tauri Programme".
 */
export function holderOfNotice(notice: string): string {
  return notice
    .replace(/^\s*(?:(?:copyright|\(c\)|©)\s*)+/i, '')
    .replace(/^(?:[0-9]{4}(?:\s*[-,–]\s*(?:[0-9]{4}|present))*[-,–\s]*)+/i, '')
    .replace(/\.?\s*all rights reserved\.?.*$/i, '')
    .replace(/[.,;\s]+$/, '')
    .trim();
}

/** A holder compared without case, spacing or a trailing period. */
export function holderKey(holder: string): string {
  return holder.toLowerCase().replace(/\s+/g, ' ').replace(/[.,;\s]+$/, '').trim().replace(/^the /, '');
}

/**
 * The holders a text names, as holderKey gives them: from its copyright lines,
 * and from a credits list that puts the notice after the name ("- Intel
 * Corporation (Copyright 2012-2021)", aws-lc's LICENSE).
 */
export function holdersOf(text: string): string[] {
  const holders: string[] = [];
  for (const raw of text.split('\n')) {
    const l = raw.replace(COMMENT_LEADER, '').replace(/^SPDX-FileCopyrightText:\s*/i, 'Copyright ').trim();
    const credit = l.match(/^(?:[-*]\s+)?(.+?)\s*\((?:copyright|\(c\)|©)\s*[0-9]{4}[^)]*\)$/i);
    if (credit) holders.push(holderKey(credit[1]));
    else if (/^(?:copyright\b|\(c\)|©)/i.test(l)) holders.push(holderKey(holderOfNotice(l)));
  }
  return holders.filter((h) => h.length > 0);
}

/**
 * SPDX-License-Identifier values in shipped headers that name no license, each
 * with where it was found and what it stands for. They are left out of the
 * bundled-code scan; any other identifier without a standard text fails it.
 */
export const UNREPRODUCIBLE_HEADER_IDS: Record<string, string> = {
  'Apache-2.inner': 'muda 0.19 (src/menu.rs, src/items/*.rs): a typo of Apache-2.0, which muda declares (Apache-2.0 OR MIT)',
};

/**
 * Reviewed lines that carry an SPDX-License-Identifier tag in a header's
 * position but only mention it (`// Example: SPDX-License-Identifier: MIT
 * (preferred)`), keyed by `<package>@<version>:<file>:<line>` (the line as
 * shipped, trimmed) with the reason, so a listed mention never hides a
 * malformed header of the same file or of another version. A tag that does
 * not parse fails the generation unless its line is listed here: it is never
 * skipped by guessing from the words around it, because that guess would also
 * skip real headers (measured: a rule on the comment leader alone skipped 258
 * of the 3727 tag lines of the dependency tree).
 */
export const KNOWN_SPDX_TAG_MENTIONS: Record<string, string> = {};

// A comment leader: C and C++ (`//`, `/*`, ` *`), shell and Python (`#`),
// Lisp and INI (`;`), Lua, SQL and Haskell (`--`, `{-`), TeX (`%`), ARM
// assembly (`@`), DCL (`$!`), HTML (`<!--`), OCaml (`(*`), batch and m4
// (`REM --`, `dnl`).
const COMMENT_LEADER_ALTERNATIVES = String.raw`(?:\/\/+!?|\/\*+!?|\*+\/?|#+|;+|--+|%+|@(?=\s)|\$!|<!--|\{-|\(\*|(?:[Rr][Ee][Mm]|[Dd][Nn][Ll])\b(?:\s*--)?)`;
const COMMENT_LEADER = new RegExp(String.raw`^\s*${COMMENT_LEADER_ALTERNATIVES}`);

// The block comments among those leaders, with their closers: C (`/* */`),
// OCaml (`(* *)`), Haskell (`{- -}`), HTML (`<!-- -->`). Every place that
// strips or finds a comment's end uses this one list.
const COMMENT_BLOCKS: [open: string, close: string][] = [
  ['/*', '*/'],
  ['(*', '*)'],
  ['{-', '-}'],
  ['<!--', '-->'],
];
const escapeRegExp = (text: string) => text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
const COMMENT_CLOSER_ALTERNATIVES = `(?:${COMMENT_BLOCKS.map(([, close]) => escapeRegExp(close)).join('|')})`;
const COMMENT_CLOSER = new RegExp(COMMENT_CLOSER_ALTERNATIVES);
const COMMENT_CLOSER_AT_END = new RegExp(`${COMMENT_CLOSER_ALTERNATIVES}\\s*$`);
// A closer and what follows it on the line (a minified line goes on with code).
const FROM_COMMENT_CLOSER = new RegExp(`\\s*${COMMENT_CLOSER_ALTERNATIVES}.*$`);
const BLOCK_OPENER_AT_START = new RegExp(`^\\s*(?:${COMMENT_BLOCKS.map(([open]) => escapeRegExp(open)).join('|')})`);
// A line comment's leader. Its text runs to the end of the line, so a closer
// inside it is text (`// files under src/*/generated`), unless the line sits
// inside a block comment, which that closer ends.
const LINE_COMMENT_LEADER = /^\s*(?:\/\/|#(?![A-Za-z_[!])|;|--(?!>)|%|@(?=\s)|\$!|(?:[Rr][Ee][Mm]|[Dd][Nn][Ll])\b)/;
const stripsCloser = (line: string, inBlock: boolean) => inBlock || !LINE_COMMENT_LEADER.test(line);
// A closer ends a block comment only outside a line comment: `// files under
// src/*/generated` is one line of a `//` run, not the end of a block.
const closesBlock = (line: string) => COMMENT_CLOSER.test(line) && stripsCloser(line, false);

// The C block comments a line opens, read left to right: a `/*` inside a
// string literal of code (`"src/*.c"`) or after `//` opens nothing, and one
// inside a comment the line already opened is text (`/* matches "src/*" */`).
// `last` is the last opener, `open` whether a comment is still open at the end.
function cCommentOpeners(line: string): { last: number; open: boolean } {
  let last = -1;
  let inComment = false;
  let quote = '';
  for (let k = 0; k < line.length; k++) {
    const c = line[k];
    if (inComment) {
      if (c === '*' && line[k + 1] === '/') {
        inComment = false;
        k++;
      }
    } else if (quote) {
      if (c === '\\') k++;
      else if (c === quote) quote = '';
    } else if (c === '"' || c === "'" || c === '`') {
      quote = c;
    } else if (c === '/' && line[k + 1] === '/') {
      break;
    } else if (c === '/' && line[k + 1] === '*') {
      last = k;
      inComment = true;
      k++;
    }
  }
  return { last, open: inComment };
}

/**
 * Whether a line that carries an SPDX-License-Identifier tag at `at` is a
 * license header: a comment line in a source file (its leader decides, not
 * the words before the tag), or the tag after punctuation only. In a document
 * (Markdown, reStructuredText) a list item is prose, and only the line-start
 * rule applies. Measured on the dependency tree: all 3727 tag lines that
 * tokenize pass, ARM `@`, DCL `$!`, `REM --` and unstarred blocks included.
 */
export function isSpdxHeaderLine(line: string, rel: string, at: number): boolean {
  const doc = /\.(?:md|markdown|rst)$/i.test(rel);
  if (doc && /^\s*(?:[-+*]|\d+[.)])\s/.test(line)) return false;
  if (!doc && COMMENT_LEADER.test(line)) return true;
  return /^[^A-Za-z0-9]*$/.test(line.slice(0, at));
}

/**
 * Code a package bundles under a license it does not declare, found by the
 * SPDX-License-Identifier header of the bundled file: qrcode.react (ISC)
 * bundles the QR Code generator (MIT) in lib/index.js. Returns one entry per
 * license expression (an exception stays with its license, as in
 * "Apache-2.0 WITH LLVM-exception"), with the holder lines of the header, the
 * header itself when it carries its own permission text, and the files. A
 * license file in the same folder does not cover a header: which license a
 * text grants is not known here, and libssh2's ChaCha files are BSD-2-Clause
 * beside its BSD-3-Clause COPYING. A REUSE folder names its texts by
 * identifier, so a header in vendored code under a license with no standard
 * text is covered by the text that code's LICENSES/ folder ships. A header
 * under a license the package
 * declares gives an entry marked declared, with its holders and files only: the
 * package's own text covers it, and its holders are listed with the package.
 * A license with no standard text fails the generation; a known upstream typo
 * (UNREPRODUCIBLE_HEADER_IDS) is no license and is left out.
 */
export function findBundledCodeUnderOtherLicenses(
  comp: ComponentInfo,
  knownLicenseIds: Set<string>,
  knownMentions: Record<string, string> = KNOWN_SPDX_TAG_MENTIONS
): { license: string; notices: string[]; files: string[]; headers: { text: string; files: string[] }[]; declared?: true }[] {
  const declared = new Set(extractSpdxLicenseIds(comp.license));
  // The license texts a REUSE folder ships, by identifier (the file name) and
  // the folder of the code they cover: the vendored code's folder, or the
  // whole package for its own LICENSES/ at the root.
  const shippedTexts = comp.licenseFiles
    .filter((f) => LICENSE_DIR.test(path.posix.basename(path.posix.dirname(f.filename))))
    .map((f) => ({ id: path.posix.basename(f.filename).replace(/\.(?:txt|md|markdown|rst)$/i, ''), root: f.vendored ? vendoredRoot(f.filename) : '' }));
  const found = new Map<string, { notices: string[]; files: string[]; headers: { text: string; files: string[] }[] }>();
  const vendoredDirs = vendoredFolders(comp);
  // The comment styles isSpdxHeaderLine accepts, so a header block keeps its
  // copyright and permission lines in each of them (`REM --`, `$!`, `@`).
  const isCommentLine = (l: string) => COMMENT_LEADER.test(l);
  // The tag's expression ends before a comment closer (`*/`, `*)`, `-}`,
  // `-->`), and code after the closer is no part of it (`BSD-2-Clause */ int
  // y;`). In a line comment a closer is text: only one at the end is dropped.
  const SPDX_TAG = new RegExp(`SPDX-License-Identifier:\\s*(.+?)\\s*(?:${COMMENT_CLOSER_ALTERNATIVES}.*)?$`);
  const SPDX_TAG_IN_LINE_COMMENT = new RegExp(`SPDX-License-Identifier:\\s*(.+?)\\s*${COMMENT_CLOSER_ALTERNATIVES}?\\s*$`);
  const stripLeader = (l: string, inBlock: boolean) => {
    const text = l.replace(COMMENT_LEADER, '').replace(/^\s/, '');
    return (stripsCloser(l, inBlock) ? text.replace(FROM_COMMENT_CLOSER, '') : text).trimEnd();
  };
  const walk = (dir: string, rel: string) => {
    for (const entry of readDirSorted(dir)) {
      const entryRel = rel ? `${rel}/${entry.name}` : entry.name;
      if (entry.isDirectory()) {
        if (!VENDORED_SKIP_DIR.test(entry.name)) walk(path.join(dir, entry.name), entryRel);
        continue;
      }
      if (!entry.isFile()) continue;
      const buf = fs.readFileSync(path.join(dir, entry.name));
      if (buf.subarray(0, 8000).includes(0)) continue;
      const lines = buf.toString('utf8').split(/\r?\n/);
      lines.forEach((line, i) => {
        const m = line.match(closesBlock(line) ? SPDX_TAG : SPDX_TAG_IN_LINE_COMMENT);
        if (!m) return;
        // The choices of the header's expression. One whose licenses the
        // package declares covers the code: its holders are still kept, as
        // declared. Otherwise the first choice whose texts all exist is
        // reproduced, and with none the generation fails: a license the
        // notices cannot reproduce is never dropped. A known upstream typo is
        // no license and is left out (UNREPRODUCIBLE_HEADER_IDS).
        // Text with characters no SPDX expression has (a backtick, a quote, a
        // backslash: a README or a string literal that mentions the tag) is no
        // header. A header made only of SPDX tokens that does not parse fails.
        // A header is a comment line that carries the tag, whatever words come
        // before it on that line (`/* Copyright 2020 Foo. SPDX-License-Identifier:
        // BSD-2-Clause */`), or the tag at the start of a line, after nothing
        // but punctuation (a LICENSE file, an unstarred block comment). A
        // sentence that mentions it is no header ("Documentation:
        // SPDX-License-Identifier: MIT (preferred)"), nor a list item of a
        // document.
        if (!isSpdxHeaderLine(line, entryRel, m.index!)) return;
        if (tokenizeSpdx(m[1]).length === 0) return;
        const parsed = spdxAlternatives(m[1]);
        if (!parsed) {
          const mentionKey = `${comp.name}@${comp.version}:${entryRel}:${line.trim()}`;
          if (mentionKey in knownMentions) return;
          throw new Error(
            `${comp.name}@${comp.version} has a malformed SPDX-License-Identifier in ${entryRel}: '${m[1]}'. If the line only mentions the tag, add ${JSON.stringify(mentionKey)} with the reason to KNOWN_SPDX_TAG_MENTIONS`
          );
        }
        // A known upstream typo is dropped from its choice term by term, and
        // the other terms of that choice still go through the policy:
        // `Apache-2.inner AND GPL-2.0-only` requires GPL-2.0-only. A choice
        // made only of typos leaves no term to check.
        const isTypo = (t: string) => t.split(' WITH ')[0] in UNREPRODUCIBLE_HEADER_IDS;
        const nonTypo = parsed.map((terms) => terms.filter((t) => !isTypo(t))).filter((terms) => terms.length > 0);
        // Bundled code is distributed with AeroFTP like the package that ships
        // it, so a header's license goes through the policy main applies to a
        // package's declared license, whichever way it is covered (declared,
        // a shipped text, its own entry): only a choice compatible with GPL-3.0
        // is taken, and a header with none fails.
        const alternatives = nonTypo.filter((terms) => isGplCompatible(terms.join(' AND ')));
        if (nonTypo.length > 0 && alternatives.length === 0) {
          throw new Error(
            `${comp.name}@${comp.version} bundles ${entryRel} under '${m[1]}', and no choice of it is compatible with GPL-3.0: review the dependency before shipping it`
          );
        }
        // A term is covered by the package's declared license. A license with no
        // standard text is also covered by the text the package ships beside the
        // vendored code (libssh2/LICENSES/FSFULLR.txt for
        // libssh2/docs/INSTALL-AUTOTOOLS.md), which the notices reproduce with
        // that code; one with a standard text keeps its bundled-code entry, so a
        // header with its own terms is still reproduced.
        const shippedTerm = (t: string) =>
          t.split(' WITH ').every(
            (id) => !knownLicenseIds.has(id) && shippedTexts.some((x) => x.id === id && (x.root === '' || entryRel.startsWith(`${x.root}/`)))
          );
        const declaredTerm = (t: string) => declared.has(t.split(' WITH ')[0]);
        const undeclaredOf = (terms: string[]) => terms.filter((t) => !declaredTerm(t) && !shippedTerm(t));
        // A header whose only license is a known upstream typo is no license:
        // its terms are left out, its holders are kept, as declared.
        const typoOnly = alternatives.length === 0;
        const covering = typoOnly ? [m[1].trim()] : alternatives.find((terms) => undeclaredOf(terms).length === 0);
        let chosen: string[];
        // Terms covered only by a text shipped beside the code: a header with
        // terms of its own is still reproduced, as declared, and its holders
        // are kept unless the code sits in a vendored folder, whose holders
        // are listed with that folder.
        let shippedOnly = false;
        if (typoOnly) {
          chosen = covering!;
        } else if (covering) {
          chosen = covering.filter(declaredTerm);
          if (chosen.length === 0) {
            chosen = covering;
            shippedOnly = true;
          }
        } else {
          const reproducible = alternatives.find((terms) =>
            undeclaredOf(terms).every((t) => t.split(' WITH ').every((id) => knownLicenseIds.has(id)))
          );
          if (!reproducible) {
            const missing = undeclaredOf(alternatives[0]).flatMap((t) => t.split(' WITH ')).find((id) => !knownLicenseIds.has(id));
            throw new Error(
              `${comp.name}@${comp.version} bundles ${entryRel} under '${m[1]}', but no standard text exists for '${missing}': add it to the standard license texts, or to UNREPRODUCIBLE_HEADER_IDS when the identifier is an upstream typo`
            );
          }
          chosen = undeclaredOf(reproducible);
        }
        // The whole comment block around the identifier, up to its start and
        // end, never into the next header.
        // Inside a block comment the block runs from its opening to its
        // closing delimiter, whether or not its lines are starred, however
        // far apart they are. A C opener may follow code on its line, but not
        // inside a string literal of that code (`"src/*.c"`), nor inside the
        // comment it opens (cCommentOpeners); the others open a line, since
        // `(*` and `{-` also occur in code (`f(*p)`).
        let open = -1;
        let openAt = -1;
        let pair: [string, string] = COMMENT_BLOCKS[0];
        for (const [opener, closer] of COMMENT_BLOCKS) {
          const openerOf = (l: string) => {
            if (opener === '/*') return cCommentOpeners(l);
            if (!l.trimStart().startsWith(opener)) return { last: -1, open: false };
            const last = l.lastIndexOf(opener);
            return { last, open: !l.slice(last).includes(closer) };
          };
          for (let k = i; k >= 0; k--) {
            const opens = openerOf(lines[k]);
            if (opens.last !== -1 && (k === i || opens.open)) {
              if (k > open) {
                open = k;
                openAt = opens.last;
                pair = [opener, closer];
              }
              break;
            }
            if (k < i && lines[k].includes(closer)) break;
          }
        }
        let close = -1;
        if (open !== -1) {
          const [opener, closer] = pair;
          for (let k = i; k < lines.length; k++) {
            if ((k === open ? lines[k].slice(openAt + opener.length) : lines[k]).includes(closer)) {
              close = k;
              break;
            }
          }
        }
        // A comment that opens and closes on its own line is whole: a run of
        // them is one header (`(* Copyright ... *)` above `(* SPDX ... *)`),
        // as a run of line comments is.
        const wholeLine = (l: string) => BLOCK_OPENER_AT_START.test(l) && COMMENT_CLOSER_AT_END.test(l);
        // A run of line comments ends where the comment style changes, at
        // any length: `//` above `#![attr]` and `//!` docs, a `//` header
        // above `#include`. A whole-line comment joins any run.
        const leaderOf = (l: string) => (l.match(COMMENT_LEADER)?.[0] ?? '').trim();
        const tagLeader = leaderOf(line);
        const inRun = (l: string) => isCommentLine(l) && (tagLeader === '' || wholeLine(l) || leaderOf(l) === tagLeader);
        let start = i;
        let end = i;
        if (open !== -1 && close !== -1 && !(open === i && close === i && wholeLine(lines[i]))) {
          start = open;
          end = close;
        } else {
          while (start > 0 && (wholeLine(lines[start]) || !BLOCK_OPENER_AT_START.test(lines[start])) && inRun(lines[start - 1])) {
            if (closesBlock(lines[start - 1]) && !wholeLine(lines[start - 1])) break;
            start--;
          }
          while (
            (wholeLine(lines[end]) || !closesBlock(lines[end])) &&
            end + 1 < lines.length &&
            inRun(lines[end + 1]) &&
            (wholeLine(lines[end + 1]) || !BLOCK_OPENER_AT_START.test(lines[end + 1]))
          ) {
            end++;
          }
        }
        const block = lines.slice(start, end + 1).map((l) => stripLeader(l, open !== -1));
        // Only a line that names a holder is a notice: "copyright notice and
        // this permission notice appear in all copies" is license prose.
        const notices: string[] = [];
        for (let bi = 0; bi < block.length; bi++) {
          let text = block[bi].trim();
          const spdxText = text.match(/^SPDX-FileCopyrightText:\s*(\S.*)$/);
          if (spdxText) text = /^(?:copyright\b|\(c\)|©)/i.test(spdxText[1]) ? spdxText[1] : `Copyright ${spdxText[1]}`;
          text = withHolderContinuation(text, block[bi + 1]);
          if (/^(?:copyright\b|\(c\)|©)/i.test(text) && namesCopyrightHolder(text)) {
            notices.push(text);
          }
        }
        // A header with its own permission text is reproduced as shipped.
        const blockText = block.join('\n').replace(/^\n+|\n+$/g, '');
        // Permission text, in the wordings headers grant it with: "Permission
        // to use", "Permission is hereby granted", "Redistribution and use",
        // "You may use, copy, modify and distribute", "may be freely used".
        // A notice that only points to a license ("Licensed under the Apache
        // License", "you may not use this file except") grants nothing itself.
        const carriesTerms = !typoOnly && PERMISSION_TEXT.test(blockText);
        if (shippedOnly && vendoredDirs.some((d) => entryRel.startsWith(`${d}/`))) notices.length = 0;
        // A covered header adds nothing when it names no holder and carries no
        // terms of its own (typoOnly never reproduces its terms).
        if ((shippedOnly || typoOnly) && !carriesTerms && notices.length === 0) return;
        for (const license of chosen) {
          const key = `${covering ? 'declared' : 'other'}\u0000${license}`;
          if (!found.has(key)) found.set(key, { notices: [], files: [], headers: [] });
          const entryFound = found.get(key)!;
          for (const n of notices) if (!entryFound.notices.includes(n)) entryFound.notices.push(n);
          if (!entryFound.files.includes(entryRel)) entryFound.files.push(entryRel);
          if (carriesTerms) {
            const same = entryFound.headers.find((h) => h.text === blockText);
            if (same) same.files.push(entryRel);
            else entryFound.headers.push({ text: blockText, files: [entryRel] });
          }
        }
      });
    }
  };
  walk(comp.packageDir, '');
  return [...found.keys()].sort(compareStrings).map((key) => {
    const [kind, license] = key.split('\u0000');
    return { license, ...found.get(key)!, ...(kind === 'declared' ? { declared: true } : {}) };
  });
}

/**
 * An attribution line: a copyright line that names a holder, a template slot
 * for one ("Copyright (c) <year> <copyright holders>"), or a bare marker whose
 * holder follows on the next line. A line that starts with the word but states
 * a condition ("Copyright notices must remain visible in the user interface")
 * is a term.
 */
function isAttributionLine(line: string): boolean {
  if (!/^\s*copyright\b/i.test(line)) return false;
  return TEMPLATE_NOTICE.test(line) || /^\s*copyrights?\s*(?:\(c\)|©)?\s*$/i.test(line) || namesCopyrightHolder(line);
}

/** The permission text of a license or header, without its attribution lines, the SPDX tag or line breaks. */
export function licenseTermsOf(text: string): string {
  return text
    .split('\n')
    .filter((l) => !isAttributionLine(l) && !/^\s*(?:all rights reserved|spdx-license-identifier:)/i.test(l))
    .join(' ')
    .replace(/\s+/g, ' ')
    .trim();
}

/**
 * Fails on a placeholder left in a standard text's terms. The GPL-family
 * appendix after the terms ("How to Apply These Terms") is a template for
 * other works and keeps its placeholders verbatim.
 */
export function assertNoTermsPlaceholders(licKey: string, licText: string): void {
  const terms = licText.split(/END OF TERMS AND CONDITIONS|How to Apply These Terms/i)[0];
  if (/<(?:copyright holders?|author|owner)>/i.test(terms) || /Copyright \([cC]\) <(?:year|YEAR)>/i.test(terms)) {
    throw new Error(`Standard license text for ${licKey} contains unpopulated placeholders (<copyright holders>, <year>, or <author>)`);
  }
}

// The first words of license prose: a bare "Copyright (c)" followed by
// "Permission is hereby granted" names no holder on the next line.
const LICENSE_PROSE_START =
  /^\s*(?:permission\b|redistribution\b|all rights\b|this\b|the\b|licensed\b|you may\b|subject to\b|everyone\b|in no event\b|use of\b|provided\b)/i;

const HOLDER_BOILERPLATE =
  /above copyright|copyright holders|copyright notice|copyright owner|copyright license/i;
// A keyword followed by a year (after an optional (c) or ©), anywhere on the line.
const HOLDER_DATED = /(?:copyrights?|\(c\)|©)\s*(?:\(c\)|©)?\s*[`'"\[\(<]*[0-9]{4}/i;
// Without a year, the keyword must begin the line (after a comment leader)
// and be followed by a name: any name after an explicit (c) or ©
// ("Copyright (c) zune-image developers", "Copyright (c) .NET Foundation"),
// a capitalized one after the bare keyword, which keeps prose such as
// "Copyright has been waived" out. A bare "(c)" at the start
// of a line is a list item in license texts (Apache-2.0 4(c) "(c) You must
// retain ..."), so it counts only with a year, above.
const HOLDER_LEAD = String.raw`^\s*(?:${COMMENT_LEADER_ALTERNATIVES}\s*)?`;
const HOLDER_NAMED = new RegExp(
  HOLDER_LEAD +
    String.raw`(?:[Pp]ortions(?:\s+of)?\s+|[Oo]riginal\s+[Ww]ork:?\s*)?(?:(?:[Cc]opyrights?|COPYRIGHTS?)\s*(?:\([cC]\)|©)\s*[\x60'"\[\(<]*[A-Za-z0-9.]|(?:[Cc]opyrights?|COPYRIGHTS?|©|SPDX-FileCopyrightText:|PackageCopyrightText:)\s*[\x60'"\[\(<]*[A-Z0-9])`
);
// A condition the keyword starts ("Copyright MUST remain visible", "Copyright
// may not be removed") names nobody. The word after the keyword is a
// condition only in lower case or in capitals: in Title Case it is a name
// ("Copyright Will Smith").
const CONDITION_WORDS = ['must', 'shall', 'should', 'will', 'cannot', 'is', 'are', 'remain', 'remains', 'may not', 'may be'];
const HOLDER_CONDITION = new RegExp(
  HOLDER_LEAD +
    String.raw`(?:[Cc]opyrights?|COPYRIGHTS?)\s+(?:${[...CONDITION_WORDS, ...CONDITION_WORDS.map((w) => w.toUpperCase())].map((w) => w.replace(' ', String.raw`\s+`)).join('|')})\b`
);
// Statements that name the holders without a name after the keyword.
const HOLDER_STATEMENT = new RegExp(
  HOLDER_LEAD +
    String.raw`(?:(?:\d+[.)]\s+)?all\s+copyrights?\s+to\s|copyrights?\s+in\s.+\s(?:are|is)\s+(?:retained|owned|held)\s+by\s)`,
  'i'
);

/**
 * Whether one line names a copyright holder. A template line ("Copyright (c)
 * <year> <owner>") names nobody, nor does prose about notices ("the above
 * copyright notice").
 */
export function namesCopyrightHolder(line: string): boolean {
  return !HOLDER_BOILERPLATE.test(line) && !HOLDER_CONDITION.test(line) && !TEMPLATE_NOTICE.test(line) && (HOLDER_DATED.test(line) || HOLDER_NAMED.test(line) || HOLDER_STATEMENT.test(line));
}

export function hasCopyrightHolderInFiles(comp: ComponentInfo): boolean {
  // A notice broken after its marker ("..., Copyright (c)" then "2015 The Rust
  // Project Developers") is read with the line that continues it.
  const joinBrokenNotices = (content: string) =>
    content
      .split(/\r?\n/)
      .map((l, i, all) =>
        /(?:copyrights?|\(c\)|©)\s*(?:\(c\)|©)?\s*$/i.test(l) && i + 1 < all.length && !LICENSE_PROSE_START.test(all[i + 1])
          ? `${l} ${all[i + 1].trim()}`
          : l
      );
  // The holders of vendored code are not the package's holders.
  return [...comp.licenseFiles, ...comp.noticeFiles].filter((f) => !f.vendored).some((f) =>
    joinBrokenNotices(f.content)
      .some(namesCopyrightHolder)
  );
}

/**
 * Fails on a holder placeholder left in what the notices generate (a fallback
 * text, a holder line). The blocks reproduced as shipped are left out: an
 * upstream header or license file that keeps `<copyright holders>` is quoted,
 * not filled in by us.
 */
export function assertNoGeneratedPlaceholders(output: string, verbatimBlocks: string[]): void {
  let generated = output;
  for (const block of verbatimBlocks) generated = generated.split(block).join('');
  if (generated.includes('<copyright holders>') || generated.includes('see repository maintainers')) {
    throw new Error('Generated notices contain unpopulated placeholders (<copyright holders> or see repository maintainers)');
  }
}

/** The notices markdown. */
// The anchor of a licensing exception's own license text.
const exceptionAnchorId = (exc: NonFreeException) => `exception-${exc.crateName.replace(/_/g, '-')}`;
// The text an exception names is the one of the registered crate version.
const isExceptionText = (exc: NonFreeException, c: LicenseTextGroup['components'][number]) =>
  c.source === 'cargo' && c.name === exc.crateName && c.version === exc.version && c.filename === exc.sourceFile;

export function generateNoticesMarkdown(
  cargoDeps: ComponentInfo[],
  npmDeps: ComponentInfo[],
  spdxLicensesFile = SPDX_STANDARD_LICENSES_FILE,
  nonFreeExceptions: NonFreeException[] = KNOWN_NON_FREE_EXCEPTIONS
): string {
  return renderNotices(cargoDeps, npmDeps, spdxLicensesFile, nonFreeExceptions).output;
}

/**
 * The notices markdown and the blocks it reproduces as shipped, the ones
 * assertNoGeneratedPlaceholders leaves out: a check of the committed file
 * leaves out the same blocks, never every fenced one, because the standard
 * texts the notices fill in are fenced too.
 */
export function renderNotices(
  cargoDeps: ComponentInfo[],
  npmDeps: ComponentInfo[],
  spdxLicensesFile = SPDX_STANDARD_LICENSES_FILE,
  nonFreeExceptions: NonFreeException[] = KNOWN_NON_FREE_EXCEPTIONS
): { output: string; verbatimBlocks: string[] } {
  // Verify all pinned fallback attributions declare a non-empty source
  for (const [key, attr] of Object.entries(PINNED_FALLBACK_ATTRIBUTIONS)) {
    if (!attr.source || attr.source.trim().length === 0) {
      throw new Error(`Entry ${key} in PINNED_FALLBACK_ATTRIBUTIONS must have a non-empty 'source' field`);
    }
    // A copyright line names its holder: a year alone would print the author as one.
    if (attr.year && !attr.copyrightHolder?.trim()) {
      throw new Error(`Entry ${key} in PINNED_FALLBACK_ATTRIBUTIONS has a year but no copyrightHolder`);
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
  lines.push('for the Rust (Cargo) and Node.js (npm) production dependencies that AeroFTP distribution packages include:');
  lines.push('- **Linux packages:** deb, rpm, AppImage, snap');
  lines.push('- **macOS & Windows packages:** DMG, MSI (bundled resources)');
  lines.push('- *(Flatpak packaging is maintained separately in the FlatPark repository; `/app/share/doc/...` is available when installed)*');
  lines.push('');
  lines.push('AeroFTP is licensed under the **GNU General Public License v3.0 or later** (GPL-3.0-or-later).');
  lines.push('The components listed here are distributed in accordance with their respective licensing terms.');
  lines.push('System libraries that are not Cargo or npm dependencies (for example WebKitGTK, which the AppImage bundles as a');
  lines.push('fallback) are not covered by this document.');
  lines.push('');

  const allComponents = [...sortedCargoDeps, ...sortedNpmDeps];
  const compMap = new Map<string, ComponentInfo>();
  for (const c of allComponents) {
    compMap.set(`${c.source}:${c.name}@${c.version}`, c);
  }
  // The bundled headers of each package, read once: the package listing takes
  // the holders of a license the package declares, the bundled-code section
  // the others.
  // One read of the standard-text table per render: the header
  // classification, the standard texts and the bundled-code section all use it.
  let standardLicenses: Record<string, string> | undefined;
  const readStandardLicenses = (): Record<string, string> => {
    if (!standardLicenses) {
      if (!fs.existsSync(spdxLicensesFile)) {
        throw new Error(`Standard SPDX licenses file is required but missing at ${spdxLicensesFile}`);
      }
      standardLicenses = JSON.parse(fs.readFileSync(spdxLicensesFile, 'utf8')) as Record<string, string>;
    }
    return standardLicenses;
  };
  const bundledCache = new Map<ComponentInfo, ReturnType<typeof findBundledCodeUnderOtherLicenses>>();
  let knownLicenseIds: Set<string> | undefined;
  const bundledOf = (comp: ComponentInfo) => {
    if (!comp.packageDir) return [];
    if (!knownLicenseIds) knownLicenseIds = new Set(Object.keys(readStandardLicenses()));
    if (!bundledCache.has(comp)) bundledCache.set(comp, findBundledCodeUnderOtherLicenses(comp, knownLicenseIds));
    return bundledCache.get(comp)!;
  };
  const listedDeclaredHolders = new Set<string>();
  const listedVendoredRoots = new Map<string, string>();
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
        const match = groupedTexts[i].components.some((c) => isExceptionText(exc, c));
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
        `- **UnRAR (\`${exc.crateName}\`):** ${exc.facts} For the full text of the UnRAR license, see [Notice Entry ${entryIndex}](#${exceptionAnchorId(exc)}).`
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
  lines.push(`- **Total Listed Components:** ${sortedCargoDeps.length + sortedNpmDeps.length}`);
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

  // Texts reproduced as shipped (license files, source headers) keep their
  // placeholders; the final check reads only what the notices generate.
  const verbatimBlocks: string[] = [];
  function makeVerbatimBlock(content: string): string {
    const block = makeCodeBlock(content);
    verbatimBlocks.push(block);
    return block;
  }

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

  // A file carries a license when it holds the standard text of a license its
  // component declares, or grants permission itself; a file that only names
  // the licenses (siphasher's COPYING) takes no anchor, which then goes to the
  // standard texts reproduced for the component below.
  const carriesALicense = (c: LicenseTextGroup['components'][number], text: string) => {
    if (PERMISSION_TEXT.test(text)) return true;
    const comp = compMap.get(`${c.source}:${c.name}@${c.version}`);
    if (!comp) return true;
    const standard = readStandardLicenses();
    return extractSpdxLicenseIds(comp.license)
      .map((id) => id.replace(/^LGPL-2\.1\+$/, 'LGPL-2.1-or-later'))
      .some((id) => id in standard && uncoveredLicenseTerms([[id]], [text], standard).length === 0);
  };
  const seenAnchorIds = new Set<string>();
  let groupIndex = 1;
  for (const group of groupedTexts) {
    const newAnchors: string[] = [];
    // A licensing exception links to the vendored text it names
    // (vendor/unrar/license.txt), under an anchor of its own.
    for (const exc of activeExceptions.filter((e) => group.components.some((c) => isExceptionText(e, c)))) {
      newAnchors.push(`<a id="${exceptionAnchorId(exc)}"></a>`);
    }
    // A component's anchor leads to its own license text, never to a notice
    // file or an SPDX document that only names the license, nor to a vendored
    // library's license: a package whose only license text is vendored
    // (whisper-rs-sys, whisper.cpp/LICENSE) is anchored at the standard text
    // of the license it declares.
    for (const comp of group.components.filter((c) => c.isLicenseText && !c.vendored && carriesALicense(c, group.text))) {
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
      if (fullComp && comp.vendored && fullComp.packageDir) {
        // The notices of vendored code, from its own folder, listed once: a
        // REUSE folder gives the same code several license texts.
        const dir = vendoredRoot(comp.filename);
        const rootKey = `${comp.source}:${comp.name}@${comp.version}:${dir}`;
        if (listedVendoredRoots.has(rootKey)) {
          lines.push(`  - The notices of \`${dir}/\` are listed with \`${listedVendoredRoots.get(rootKey)}\``);
          continue;
        }
        listedVendoredRoots.set(rootKey, comp.filename);
        const inDir = [...fullComp.licenseFiles, ...fullComp.noticeFiles]
          .filter((f) => vendoredRoot(f.filename) === dir)
          .map((f) => f.filename.slice(dir.length + 1));
        const nested = vendoredFolders(fullComp)
          .filter((d) => d.startsWith(`${dir}/`))
          .map((d) => d.slice(dir.length + 1));
        for (const n of findPackageCopyrightNotices(path.join(fullComp.packageDir, dir), inDir, nested)) {
          lines.push(`  - ${n.notice} (${dir}/${n.file})`);
        }
        continue;
      }
      if (fullComp && fullComp.licenseFiles.some((f) => !f.vendored) && hasCopyrightHolderInFiles(fullComp)) {
        // The package's files name its holders, so its files are not scanned:
        // the holders of bundled headers under its own license are listed
        // here, once per package.
        const compKey = `${comp.source}:${comp.name}@${comp.version}`;
        if (!listedDeclaredHolders.has(compKey)) {
          listedDeclaredHolders.add(compKey);
          // A holder the package's own files already name is attributed there:
          // the same complete holder, not a name inside a longer one ("Alice"
          // is not "Alice Smith").
          const ownHolders = new Set(fullComp.licenseFiles.filter((f) => !f.vendored).flatMap((f) => holdersOf(f.content)));
          const byNotice = new Map<string, string[]>();
          for (const b of bundledOf(fullComp).filter((x) => x.declared)) {
            for (const n of b.notices.filter((x) => !ownHolders.has(holderKey(holderOfNotice(x))))) {
              if (!byNotice.has(n)) byNotice.set(n, []);
              if (!byNotice.get(n)!.includes(b.license)) byNotice.get(n)!.push(b.license);
            }
          }
          for (const [n, licenses] of byNotice) {
            lines.push(`  - ${n} (in a source header under ${licenses.sort(compareStrings).join(', ')})`);
          }
          // The files name a holder, and the sources may name others (an
          // untagged src/lib.rs by a second contributor): each of their
          // notices is listed once, leaving out the holders the files name
          // and those of the bundled headers, listed above or with the
          // bundled code under their own license.
          const named = new Set([...ownHolders, ...bundledOf(fullComp).flatMap((b) => b.notices).map((n) => holderKey(holderOfNotice(n)))]);
          const excluded = [...fullComp.licenseFiles, ...fullComp.noticeFiles].map((f) => f.filename);
          for (const n of findPackageCopyrightNotices(fullComp.packageDir, excluded, vendoredFolders(fullComp))) {
            if (!named.has(holderKey(holderOfNotice(n.notice)))) lines.push(`  - ${n.notice} (${n.file})`);
          }
        }
      }
      if (fullComp && fullComp.licenseFiles.some((f) => !f.vendored) && !hasCopyrightHolderInFiles(fullComp)) {
        const excluded = [...fullComp.licenseFiles, ...fullComp.noticeFiles].map((f) => f.filename);
        const pkgNotices = findPackageCopyrightNotices(fullComp.packageDir, excluded, vendoredFolders(fullComp));
        if (pkgNotices.length > 0) {
          for (const n of pkgNotices) {
            lines.push(`  - ${n.notice} (${n.file})`);
          }
        } else {
          lines.push(`  - ${noOwnNoticeLine(fullComp)}`);
        }
      }
    }
    lines.push('');
    lines.push(makeVerbatimBlock(group.text));
    lines.push('');
    groupIndex++;
  }

  // Every term of the chosen license needs its text: a package whose own files
  // carry only some of them (MIT AND Apache-2.0 with LICENSE-MIT alone) gets the
  // standard text of the others.
  const mapTerm = (term: string) => term.replace(/^LGPL-2\.1\+?(?= WITH |$)/, 'LGPL-2.1-or-later');
  const compatibleChoices = (choices: string[][]) => {
    const compat = choices.filter((terms) => isGplCompatible(terms.join(' AND ')));
    return compat.length > 0 ? compat : choices;
  };
  const standardTextTerms = new Map<ComponentInfo, string[][]>();
  for (const comp of allComponents) {
    const ownTexts = comp.licenseFiles.filter((f) => !f.vendored && f.content.trim().length > 0).map((f) => f.content);
    if (ownTexts.length === 0) continue;
    const alternatives = spdxAlternatives(comp.license);
    if (!alternatives) continue;
    const left = uncoveredLicenseTerms(
      compatibleChoices(alternatives).map((terms) => terms.map(mapTerm)),
      ownTexts,
      readStandardLicenses()
    );
    if (left.length > 0) standardTextTerms.set(comp, left);
  }
  const componentsWithStandardTexts = [...componentsWithoutFiles, ...standardTextTerms.keys()];
  // A package with own license files that name holders ships its copyright
  // notices there: they are reproduced above with those files.
  // The authors listed for a standard text: the manifest's, a reviewed
  // attribution, the SPDX document's supplier, or the holders the package's
  // own license files name.
  const authorsOf = (comp: ComponentInfo) =>
    comp.authors && comp.authors.length > 0
      ? comp.authors.join(', ')
      : (PINNED_FALLBACK_ATTRIBUTIONS[`${comp.name}@${comp.version}`]?.author ??
        spdxDocumentAttribution(comp)?.author ??
        (ownFilesNotice(comp) ? 'named in its license files' : undefined));
  const ownFilesNotice = (comp: ComponentInfo) => {
    if (!standardTextTerms.has(comp)) return undefined;
    const files = [...comp.licenseFiles, ...comp.noticeFiles]
      .filter((f) => !f.vendored && hasCopyrightHolderInFiles({ ...comp, licenseFiles: [f], noticeFiles: [] }))
      .map((f) => `\`${f.filename}\``);
    return files.length > 0
      ? `The copyright notices it ships are reproduced above, with its license files (${files.join(', ')})`
      : undefined;
  };

  if (componentsWithStandardTexts.length > 0) {
    const standardLicenses = readStandardLicenses();

    lines.push('---');
    lines.push('');
    lines.push('## Standard License Texts for Dependencies that Need Them');
    lines.push('');
    lines.push('The following dependencies need one or more standard license texts: their manifests declare the');
    lines.push('licenses, and their package files do not include the text of each of them.');
    lines.push('In accordance with licensing requirements, the standard verbatim text for each applicable license is');
    lines.push(
      'provided below. Authors are listed as declared in package manifests or reviewed attributions; copyright notices, where available, reproduce upstream repository declarations.'
    );
    lines.push('');

    // Map each component to its required standard license texts using exact parsed SPDX identifiers
    const licenseToComponents = new Map<string, ComponentInfo[]>();
    for (const comp of componentsWithStandardTexts) {
      const key = `${comp.name}@${comp.version}`;
      if (!authorsOf(comp)) {
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
      // The choices of the expression, from its parse: a conjunct binds every
      // choice, and a term keeps its exception. Every choice whose texts all
      // exist is reproduced (MIT OR Apache-2.0 gives both); with none, the
      // first missing text of the first choice fails.
      const alternatives = spdxAlternatives(comp.license);
      if (!alternatives) {
        throw new Error(`Component ${key} has a license expression that does not parse: '${comp.license}'`);
      }
      const missingTexts = (terms: string[]) =>
        terms.flatMap((term) => term.split(' WITH ')).filter((id) => !standardLicenses[id]);
      // A package with own files needs only the terms they leave out.
      const mapped = standardTextTerms.get(comp) ?? compatibleChoices(alternatives).map((terms) => terms.map(mapTerm));
      const reproducible = mapped.filter((terms) => missingTexts(terms).length === 0);
      if (reproducible.length === 0) {
        throw new Error(
          `Component ${comp.name}@${comp.version} requires standard license text for '${missingTexts(mapped[0])[0]}', but no entry exists in ${spdxLicensesFile}`
        );
      }
      const idsToInclude = [...new Set(reproducible.flat())];

      for (const id of idsToInclude) {
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
        const spdxDoc = spdxDocumentAttribution(comp);
        lines.push(`- \`${comp.name}@${comp.version}\` - SPDX: \`${comp.license}\` (authors: ${authorsOf(comp)})`);

        if (spdxDoc?.notice) {
          lines.push(`  - ${spdxDoc.notice} (${spdxDoc.file})`);
        }
        const ownNotice = ownFilesNotice(comp);
        const excluded = [...comp.licenseFiles, ...comp.noticeFiles].map((f) => f.filename);
        const pkgNotices = ownNotice ? [] : findPackageCopyrightNotices(comp.packageDir, excluded, vendoredFolders(comp));
        if (ownNotice) {
          lines.push(`  - ${ownNotice}`);
        } else if (pkgNotices.length > 0) {
          for (const n of pkgNotices) {
            lines.push(`  - ${n.notice} (${n.file})`);
          }
        } else if (spdxDoc?.notice) {
          // The copyright line of the SPDX document is listed above.
        } else if (comp.noticeFiles.length > 0) {
          lines.push(`  - Notice file shipped: ${comp.noticeFiles.map((f) => f.filename).join(', ')}`);
        } else {
          lines.push(`  - ${noOwnNoticeLine(comp)}`);
        }
      }
      lines.push('');

      // "Apache-2.0 WITH LLVM-exception": the license text, then the exception's.
      const [licenseId, exceptionId] = licKey.split(' WITH ');
      let licText = standardLicenses[licenseId];
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
          const pkgNotices = findPackageCopyrightNotices(comp.packageDir, excluded, vendoredFolders(comp));

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
            const noNotice = vendoredFolders(comp).length > 0 ? noOwnNoticeLine(comp) : 'No copyright notice is shipped upstream';
            attributionLines.push(`${noNotice}; authors${srcStr}: ${author} (${comp.name})`);
          }
        }
        licText = licText.replace(
          /Copyright \([cC]\) <(?:year|YEAR)> <(?:copyright holders|COPYRIGHT HOLDERS|author|authors|AUTHOR|AUTHORS)>/gi,
          attributionLines.join('\n')
        );
      }
      // Any placeholder left in the terms, also one the replacement above does
      // not know (a "Copyright (C) <year>" with no author placeholder), fails.
      assertNoTermsPlaceholders(licKey, licText);

      lines.push(makeCodeBlock(exceptionId ? `${licText}\n\n${standardLicenses[exceptionId]}` : licText));
      lines.push('');
    }
  }

  // Code bundled under a license its package does not declare.
  const bundledComponents = allComponents.filter((c) => c.packageDir);
  if (bundledComponents.length > 0) {
    const standardTexts = readStandardLicenses();
    const byLicense = new Map<string, string[]>();
    const holdersByLicense = new Map<string, string[]>();
    const noHolder = 'No copyright line in the header that declares this license';
    const ownTermHeaders: string[] = [];
    for (const comp of bundledComponents) {
      for (const b of bundledOf(comp)) {
        // A header under a declared license is covered by the package's text;
        // its holders are listed with the package, and a header whose terms
        // differ from the standard text and from every license text of the
        // package (its own and the ones shipped beside vendored code) is
        // reproduced below.
        if (b.declared) {
          const standardTerms = licenseTermsOf(standardTexts[b.license.split(' WITH ')[0]] ?? '');
          const ownTerms = new Set(comp.licenseFiles.map((f) => licenseTermsOf(f.content)));
          for (const h of b.headers) {
            const terms = licenseTermsOf(h.text);
            if (terms === standardTerms || ownTerms.has(terms)) continue;
            ownTermHeaders.push(
              `- \`${comp.name}@${comp.version}\` (${comp.source}, SPDX: \`${comp.license}\`) ships a header tagged \`${b.license}\` whose permission text differs from it, in ${h.files.map((f) => `\`${f}\``).join(', ')}:`,
              '',
              makeVerbatimBlock(h.text),
              ''
            );
          }
          continue;
        }
        if (!byLicense.has(b.license)) byLicense.set(b.license, []);
        if (!holdersByLicense.has(b.license)) holdersByLicense.set(b.license, []);
        holdersByLicense.get(b.license)!.push(...(b.notices.length > 0 ? b.notices : [noHolder]).map((n) => `${n} (${comp.name})`));
        byLicense.get(b.license)!.push(
          `- \`${comp.name}@${comp.version}\` (${comp.source}, SPDX: \`${comp.license}\`) bundles code under ${b.license} in ${b.files.map((f) => `\`${f}\``).join(', ')}`,
          ...(b.notices.length > 0 ? b.notices.map((n) => `  - ${n}`) : [`  - ${noHolder}`])
        );
        // A header is reproduced only when its terms differ from the standard text's.
        const standardTerms = licenseTermsOf(standardTexts[b.license.split(' WITH ')[0]] ?? '');
        for (const h of b.headers.filter((x) => licenseTermsOf(x.text) !== standardTerms)) {
          byLicense.get(b.license)!.push('', `  Header with its own terms, as shipped in ${h.files.map((f) => `\`${f}\``).join(', ')}:`, '', makeVerbatimBlock(h.text), '');
        }
      }
    }
    if (byLicense.size > 0) {
      lines.push('## Bundled Code Under Other Licenses');
      lines.push('');
      lines.push('These components bundle code from another project under a license they do not declare themselves. The bundled files name that license in their SPDX-License-Identifier header. The standard text is reproduced with the copyright lines of the header, and the header itself when its permission text differs from the standard text.');
      lines.push('');
      for (const licKey of [...byLicense.keys()].sort(compareStrings)) {
        lines.push(`### Bundled Code: ${licKey}`);
        lines.push('');
        lines.push(...byLicense.get(licKey)!);
        lines.push('');
        // "Apache-2.0 WITH LLVM-exception": the license text, then the exception's.
        // The scan fails on a license or exception without a standard text.
        const [licenseId, exceptionId] = licKey.split(' WITH ');
        // A text with a holder placeholder (BSD) takes the header's copyright lines.
        const text = standardTexts[licenseId].replace(
          /Copyright \([cC]\) <(?:year|YEAR)> <(?:copyright holders|COPYRIGHT HOLDERS|owner|author|authors)>/g,
          () => holdersByLicense.get(licKey)!.join('\n')
        );
        assertNoTermsPlaceholders(licKey, text);
        lines.push(makeCodeBlock(exceptionId ? `${text}\n\n${standardTexts[exceptionId]}` : text));
        lines.push('');
      }
    }
    if (ownTermHeaders.length > 0) {
      lines.push('## Source Headers With Their Own Terms');
      lines.push('');
      lines.push('Each of these headers is tagged with a license whose text these notices already reproduce for its component (the license the component declares, or a license text shipped beside the vendored code), but the permission text of the header differs from that text and from the standard one. The tag is quoted as the header states it, and the header is reproduced as shipped.');
      lines.push('');
      lines.push(...ownTermHeaders);
    }
  }

  const output = lines.join('\n') + '\n';
  assertNoGeneratedPlaceholders(output, verbatimBlocks);

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

  return { output, verbatimBlocks };
}

/**
 * The mode of one run: exactly one of --check and --write. Both together
 * (`npm run check:licenses -- --write`) would write a stale file over and
 * report success without checking it, so they are refused, before any
 * dependency is collected.
 */
export function parseMode(args: string[]): 'check' | 'write' {
  const isCheck = args.includes('--check');
  const isWrite = args.includes('--write');
  if (isCheck === isWrite) {
    throw new Error('Usage: gen-third-party-notices.ts [--check | --write] (exactly one of them)');
  }
  return isCheck ? 'check' : 'write';
}

/**
 * Writes the notices through a temporary file in the same directory, renamed
 * over the target only once the write has succeeded: a failed write (a full
 * disk) leaves the previous notices whole and removes the temporary file.
 */
export function writeNoticesFile(
  target: string,
  content: string,
  write: (file: string, data: string) => void = (file, data) => fs.writeFileSync(file, data, 'utf8')
): void {
  const tmp = path.join(path.dirname(target), `.${path.basename(target)}.${process.pid}.tmp`);
  try {
    write(tmp, content);
    fs.renameSync(tmp, target);
  } catch (err) {
    fs.rmSync(tmp, { force: true });
    throw err;
  }
}

function main() {
  let mode: 'check' | 'write';
  try {
    mode = parseMode(process.argv.slice(2));
  } catch (err: any) {
    console.error(err.message);
    process.exit(1);
  }
  const isCheck = mode === 'check';
  const isWrite = mode === 'write';

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
    writeNoticesFile(NOTICES_FILE, generated);
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
