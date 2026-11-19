// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet (see AI-TRANSPARENCY.md)

import test from 'node:test';
import assert from 'node:assert';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync, spawnSync } from 'node:child_process';
import { createRequire, syncBuiltinESMExports } from 'node:module';
import { createHash } from 'node:crypto';
import YAML from 'yaml';
import {
  citationApplies,
  publicLink,
  CARGO_TREE_ARGS,
  isGplCompatible,
  tokenizeSpdx,
  spdxAlternatives,
  MAX_SPDX_ALTERNATIVES,
  extractSpdxLicenseIds,
  activeCargoPackageIds,
  carriesWholeStandardText,
  collectCargoDependencies,
  collectNpmDependencies,
  readPackageFiles,
  groupLicenseTexts,
  generateNoticesMarkdown,
  SPDX_STANDARD_LICENSES_FILE,
  PINNED_CRATE_LICENSES,
  KNOWN_NON_FREE_EXCEPTIONS,
  PINNED_FALLBACK_ATTRIBUTIONS,
  firstDifferingLine,
  findPackageCopyrightNotices,
  hasCopyrightHolderInFiles,
  namesCopyrightHolder,
  findBundledCodeUnderOtherLicenses,
  licenseTermsOf,
  vendoredFolders,
  isSpdxHeaderLine,
  assertNoGeneratedPlaceholders,
  renderNotices,
  parseMode,
  writeNoticesFile,
  npmInstallsHere,
  currentNodeLibc,
  auditGplCompatibility,
  noticesLinkFrom,
  npmAuthors,
  reviewedLicenseTerms,
  standardTextId,
  reviewedNotices,
  reviewedTagMentions,
  reviewedSourceTexts,
  reviewedSourceText,
} from '../../scripts/gen-third-party-notices.ts';

// The committed notices with the blocks the generator reproduces as shipped,
// from a fresh generation of the same tree (check:licenses runs first in CI,
// so the two are equal). Checks of the committed file read the text the
// notices write: content minus exactly those blocks.
// The real dependency tree, collected once for the whole suite: the Cargo
// metadata and npm walks are the slow part, and every real-tree check reads
// the same inventory.
let realTreeInventory;
function realTree() {
  if (!realTreeInventory) realTreeInventory = { cargo: collectCargoDependencies(), npm: collectNpmDependencies() };
  return realTreeInventory;
}
let committedNotices;
// The committed notices as the generator writes them: a Windows checkout
// with autocrlf has CRLF line ends, the generator LF.
function lf(md) {
  return md.replace(/\r\n/g, '\n');
}

function committedNoticesWithVerbatim() {
  if (!committedNotices) {
    const content = lf(fs.readFileSync(path.join(ROOT, 'THIRD-PARTY-NOTICES.md'), 'utf8'));
    const { output, verbatimBlocks, documents } = renderNotices(realTree().cargo, realTree().npm);
    assert.strictEqual(content, output, 'THIRD-PARTY-NOTICES.md is up to date (npm run gen:licenses)');
    for (const doc of documents) {
      const file = path.join(ROOT, doc.file);
      assert.ok(fs.existsSync(file), `${doc.file} exists (npm run gen:licenses)`);
      assert.strictEqual(fs.readFileSync(file, 'utf8').replace(/\r\n/g, '\n'), doc.content, `${doc.file} is up to date (npm run gen:licenses)`);
    }
    let generated = content;
    for (const block of verbatimBlocks) generated = generated.split(block).join('');
    committedNotices = { content, verbatimBlocks, generated };
  }
  return committedNotices;
}

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const ROOT = path.resolve(__dirname, '..', '..');

// A license file as packages ship it: its first lines, then the standard terms
// without their placeholder line. A file must carry the text to cover a term.
// The lines of a package that mention a copyright. The directory is a literal
// argument, never shell text: a "$" in the checkout path is part of the name.
// Exit status 1 is "no match"; any other failure is an error.
const grepCopyrightLines = (dir) => {
  try {
    return execFileSync('grep', ['-rIhi', '--exclude-dir=node_modules', '-E', '(copyright|©|\\(c\\))', '--', dir], { encoding: 'utf8' });
  } catch (err) {
    if (err.status === 1) return '';
    throw new Error(`Grep scan failed for ${dir} (status ${err.status}): ${err.message}`);
  }
};
const STANDARD_TEXTS = JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'));
const shippedLicense = (lead, id) =>
  [lead, STANDARD_TEXTS[id].replace(/^.*<(?:year|copyright holders?)>.*\n+/im, '')].filter(Boolean).join('\n\n');

test('SPDX parser: tokenizer handles operators, parentheses, slashes, and identifiers', () => {
  assert.deepStrictEqual(tokenizeSpdx('MIT'), ['MIT']);
  assert.deepStrictEqual(tokenizeSpdx('Apache-2.0 / MIT'), ['Apache-2.0', 'OR', 'MIT']);
  assert.deepStrictEqual(tokenizeSpdx('MIT ORacle-x'), ['MIT', 'ORacle-x']);
  assert.deepStrictEqual(
    tokenizeSpdx('(Apache-2.0 OR MIT) AND BSD-3-Clause'),
    ['(', 'Apache-2.0', 'OR', 'MIT', ')', 'AND', 'BSD-3-Clause']
  );
  assert.deepStrictEqual(
    tokenizeSpdx('Apache-2.0 WITH LLVM-exception'),
    ['Apache-2.0', 'WITH', 'LLVM-exception']
  );
});

test('SPDX parser: spdxAlternatives deduplicates repetitive choices and bounds distinct cross-product expansion', () => {
  // Ordinary mixed AND/OR expression still expands every distinct choice.
  assert.deepStrictEqual(
    spdxAlternatives('(MIT OR Apache-2.0) AND (BSD-2-Clause OR ISC)'),
    [
      ['MIT', 'BSD-2-Clause'],
      ['MIT', 'ISC'],
      ['Apache-2.0', 'BSD-2-Clause'],
      ['Apache-2.0', 'ISC'],
    ]
  );
  // 30 repeated two-way choices would yield 2^30 combinations naively, but
  // deduplication keeps only the 3 distinct sets.
  const repetitive = Array.from({ length: 30 }, () => '(MIT OR Apache-2.0)').join(' AND ');
  const deduped = spdxAlternatives(repetitive);
  assert.strictEqual(deduped.length, 3);
  assert.deepStrictEqual(deduped, [['MIT'], ['MIT', 'Apache-2.0'], ['Apache-2.0']]);
  // 11 independent binary choices exceed MAX_SPDX_ALTERNATIVES (2^11 = 2048 > 1024).
  const huge = Array.from({ length: 11 }, (_, i) => `(LicenseA-${i} OR LicenseB-${i})`).join(' AND ');
  assert.throws(
    () => spdxAlternatives(huge),
    new RegExp(`exceeds ${MAX_SPDX_ALTERNATIVES} distinct alternatives`)
  );
});

test('SPDX parser: extractSpdxLicenseIds extracts exact identifiers without operator or exception leak', () => {
  assert.deepStrictEqual(extractSpdxLicenseIds('MIT'), ['MIT']);
  assert.deepStrictEqual(extractSpdxLicenseIds('MIT-0'), ['MIT-0']);
  assert.deepStrictEqual(extractSpdxLicenseIds('MIT OR Apache-2.0'), ['MIT', 'Apache-2.0']);
  assert.deepStrictEqual(
    extractSpdxLicenseIds('(Apache-2.0 OR MIT) AND BSD-3-Clause'),
    ['Apache-2.0', 'MIT', 'BSD-3-Clause']
  );
  assert.deepStrictEqual(
    extractSpdxLicenseIds('Apache-2.0 WITH LLVM-exception'),
    ['Apache-2.0']
  );
});

test('SPDX parser: compatible licenses and expressions pass GPL-3.0 compatibility gate', () => {
  // Permissive standard licenses
  assert.strictEqual(isGplCompatible('MIT'), true);
  assert.strictEqual(isGplCompatible('MIT-0'), true);
  assert.strictEqual(isGplCompatible('Apache-2.0'), true);
  assert.strictEqual(isGplCompatible('BSD-2-Clause'), true);
  assert.strictEqual(isGplCompatible('BSD-3-Clause'), true);
  assert.strictEqual(isGplCompatible('ISC'), true);
  assert.strictEqual(isGplCompatible('0BSD'), true);
  assert.strictEqual(isGplCompatible('CC0-1.0'), true);
  assert.strictEqual(isGplCompatible('Unlicense'), true);
  assert.strictEqual(isGplCompatible('Zlib'), true);
  assert.strictEqual(isGplCompatible('Unicode-3.0'), true);
  assert.strictEqual(isGplCompatible('CDLA-Permissive-2.0'), true);

  // Exceptions
  assert.strictEqual(isGplCompatible('Apache-2.0 WITH LLVM-exception'), true);

  // Weak/strong copyleft compatible with GPLv3
  assert.strictEqual(isGplCompatible('LGPL-2.1-or-later'), true);
  assert.strictEqual(isGplCompatible('LGPL-3.0-or-later'), true);
  assert.strictEqual(isGplCompatible('GPL-2.0-or-later'), true);
  assert.strictEqual(isGplCompatible('GPL-3.0-only'), true);
  assert.strictEqual(isGplCompatible('MPL-2.0'), true);

  // Dual licensing (OR): compatible if any branch is valid
  assert.strictEqual(isGplCompatible('MIT OR SSPL-1.0'), true);
  assert.strictEqual(isGplCompatible('Apache-2.0 OR GPL-2.0-only'), true);
  assert.strictEqual(isGplCompatible('Apache-2.0 / MIT'), true);

  // Conjunction (AND): compatible if all conjuncts are valid
  assert.strictEqual(isGplCompatible('(Apache-2.0 OR MIT) AND BSD-3-Clause'), true);
  assert.strictEqual(isGplCompatible('(MIT OR Apache-2.0) AND Unicode-3.0'), true);
});

test('SPDX parser: incompatible fixtures fail GPL-3.0 compatibility gate (red-proven)', () => {
  // Substring bypasses that would have passed the old gate must now fail
  assert.strictEqual(
    isGplCompatible('MIT AND SSPL-1.0'),
    false,
    'AND with one incompatible conjunct (SSPL-1.0) must fail'
  );
  assert.strictEqual(
    isGplCompatible('(MIT OR Apache-2.0) AND SSPL-1.0'),
    false,
    'Grouped AND with SSPL-1.0 must fail'
  );
  assert.strictEqual(
    isGplCompatible('MIT AND GPL-2.0-only'),
    false,
    'AND with GPL-2.0-only must fail'
  );

  // Incomplete expressions and unconsumed tokens must fail
  assert.strictEqual(isGplCompatible('MIT ORacle-x'), false, 'Operator substring in identifier must not split token');
  assert.strictEqual(isGplCompatible('MIT AND ORacle-x'), false, 'Unrecognized identifier in AND must fail');
  assert.strictEqual(isGplCompatible('MIT SSPL-1.0'), false, 'Trailing unconsumed token must fail');
  assert.strictEqual(isGplCompatible('MIT WITH'), false, 'Missing exception for WITH must fail');
  assert.strictEqual(isGplCompatible('MIT OR'), false, 'Missing RHS for OR must fail');
  assert.strictEqual(isGplCompatible('MIT AND'), false, 'Missing RHS for AND must fail');
  assert.strictEqual(isGplCompatible('OR MIT'), false, 'Leading operator must fail');
  assert.strictEqual(isGplCompatible('(MIT'), false, 'Unclosed parenthesis must fail');
  assert.strictEqual(isGplCompatible('MIT)'), false, 'Unmatched closing parenthesis must fail');
  assert.strictEqual(isGplCompatible('MIT & Apache-2.0'), false, 'Unrecognized characters must fail');

  // Strictly incompatible standalone licenses
  assert.strictEqual(
    isGplCompatible('GPL-2.0-only'),
    false,
    'GPL-2.0-only is strictly incompatible with GPL-3.0'
  );
  assert.strictEqual(
    isGplCompatible('GPL-2.0'),
    false,
    'GPL-2.0 (without or-later) must fail'
  );
  assert.strictEqual(
    isGplCompatible('SSPL-1.0'),
    false,
    'SSPL-1.0 is non-free/incompatible'
  );
  assert.strictEqual(
    isGplCompatible('AGPL-1.0'),
    false,
    'AGPL-1.0 is incompatible'
  );

  // Unknown or invalid identifiers
  assert.strictEqual(
    isGplCompatible('UNKNOWN-PROPRIETARY-LICENSE'),
    false,
    'Unknown license must fail'
  );
  assert.strictEqual(
    isGplCompatible('Apache-2.0 WITH Unknown-Exception'),
    false,
    'Unknown exception must fail'
  );
  assert.strictEqual(isGplCompatible(''), false, 'Empty string must fail');
  assert.strictEqual(isGplCompatible('Unknown'), false, 'Unknown string must fail');
  assert.strictEqual(isGplCompatible('NO_LICENSE'), false, 'NO_LICENSE must fail');
});

test('Pinned crates table contains exact known license-file crates', () => {
  assert.strictEqual(PINNED_CRATE_LICENSES['gopenpgp-sys@0.3.9'], 'MIT');
  assert.strictEqual(PINNED_CRATE_LICENSES['proton-crypto@0.14.1'], 'MIT');
  assert.strictEqual(PINNED_CRATE_LICENSES['proton-crypto-account@0.20.1'], 'MIT');
  assert.strictEqual(PINNED_CRATE_LICENSES['proton-rpgp@0.9.0'], 'MIT');
  assert.strictEqual(PINNED_CRATE_LICENSES['proton-srp@0.8.4'], 'MIT');
  assert.strictEqual(PINNED_CRATE_LICENSES['sigstore-protobuf-specs-derive@0.0.1'], 'Apache-2.0');
});

test('npm collection: walks lock entries by path and preserves multiple versions of the same package', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'npm-lock-fixture-'));
  try {
    const fixtureLock = {
      name: 'fixture-app',
      version: '1.0.0',
      lockfileVersion: 3,
      packages: {
        '': {
          dependencies: {
            foo: '^1.0.0',
            bar: '^1.0.0',
          },
        },
        'node_modules/foo': {
          version: '1.0.0',
          license: 'MIT',
        },
        'node_modules/bar': {
          version: '1.0.0',
          license: 'MIT',
          dependencies: {
            foo: '^2.0.0',
          },
        },
        'node_modules/bar/node_modules/foo': {
          version: '2.0.0',
          license: 'Apache-2.0',
        },
        'node_modules/dev-tool': {
          version: '0.1.0',
          license: 'MIT',
          dev: true,
        },
      },
    };

    fs.mkdirSync(path.join(tmpDir, 'node_modules/foo'), { recursive: true });
    fs.writeFileSync(path.join(tmpDir, 'node_modules/foo/LICENSE'), 'MIT License foo v1');
    fs.mkdirSync(path.join(tmpDir, 'node_modules/bar/node_modules/foo'), { recursive: true });
    fs.writeFileSync(path.join(tmpDir, 'node_modules/bar/node_modules/foo/LICENSE'), 'Apache License foo v2');
    fs.mkdirSync(path.join(tmpDir, 'node_modules/bar'), { recursive: true });
    fs.writeFileSync(path.join(tmpDir, 'node_modules/bar/LICENSE'), 'MIT License bar');
    for (const [dir, version] of [['node_modules/foo', '1.0.0'], ['node_modules/bar', '1.0.0'], ['node_modules/bar/node_modules/foo', '2.0.0']]) {
      fs.writeFileSync(path.join(tmpDir, dir, 'package.json'), JSON.stringify({ name: dir.split('/').pop(), version }));
    }

    const lockPath = path.join(tmpDir, 'package-lock.json');
    fs.writeFileSync(lockPath, JSON.stringify(fixtureLock));

    const deps = collectNpmDependencies(lockPath, tmpDir);

    const fooDeps = deps.filter((d) => d.name === 'foo');
    assert.strictEqual(fooDeps.length, 2, 'Both versions of package foo must be collected');
    assert.strictEqual(fooDeps[0].version, '1.0.0');
    assert.strictEqual(fooDeps[1].version, '2.0.0');

    // Dev dependencies must be excluded
    assert.strictEqual(deps.some((d) => d.name === 'dev-tool'), false, 'devDependencies must be excluded');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('npm collection: the installed package must be the locked package and version, or the notices would name the wrong files', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'npm-version-fixture-'));
  try {
    const lockPath = path.join(tmpDir, 'package-lock.json');
    fs.writeFileSync(lockPath, JSON.stringify({ lockfileVersion: 3, packages: { '': {}, 'node_modules/foo': { version: '2.0.0', license: 'MIT' } } }));
    fs.mkdirSync(path.join(tmpDir, 'node_modules/foo'), { recursive: true });
    fs.writeFileSync(path.join(tmpDir, 'node_modules/foo/LICENSE'), 'MIT License foo');
    fs.writeFileSync(path.join(tmpDir, 'node_modules/foo/package.json'), JSON.stringify({ name: 'foo', version: '2.0.0' }));
    assert.deepStrictEqual(collectNpmDependencies(lockPath, tmpDir).map((d) => `${d.name}@${d.version}`), ['foo@2.0.0']);
    fs.writeFileSync(path.join(tmpDir, 'node_modules/foo/package.json'), JSON.stringify({ name: 'foo', version: '1.0.0' }));
    assert.throws(
      () => collectNpmDependencies(lockPath, tmpDir),
      /node_modules\/foo is installed as foo@1\.0\.0, but package-lock\.json locks foo@2\.0\.0: run npm ci/
    );
    // npm's own reader accepts a manifest that starts with a byte order mark.
    fs.writeFileSync(path.join(tmpDir, 'node_modules/foo/package.json'), '\uFEFF' + JSON.stringify({ name: 'foo', version: '2.0.0' }));
    assert.deepStrictEqual(collectNpmDependencies(lockPath, tmpDir).map((d) => `${d.name}@${d.version}`), ['foo@2.0.0']);
    fs.rmSync(path.join(tmpDir, 'node_modules/foo/package.json'));
    assert.throws(
      () => collectNpmDependencies(lockPath, tmpDir),
      /node_modules\/foo has no package\.json, but package-lock\.json locks foo@2\.0\.0: run npm ci/
    );
    // An alias is installed under its own directory and checked against the package it resolves to.
    fs.writeFileSync(lockPath, JSON.stringify({ lockfileVersion: 3, packages: { '': {}, 'node_modules/foo': { name: 'baz', version: '2.0.0', license: 'MIT' } } }));
    fs.writeFileSync(path.join(tmpDir, 'node_modules/foo/package.json'), JSON.stringify({ name: 'baz', version: '2.0.0' }));
    assert.deepStrictEqual(collectNpmDependencies(lockPath, tmpDir).map((d) => `${d.name}@${d.version}`), ['baz@2.0.0']);
    fs.writeFileSync(path.join(tmpDir, 'node_modules/foo/package.json'), JSON.stringify({ name: 'bar', version: '2.0.0' }));
    assert.throws(
      () => collectNpmDependencies(lockPath, tmpDir),
      /node_modules\/foo is installed as bar@2\.0\.0, but package-lock\.json locks baz@2\.0\.0: run npm ci/
    );
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('npm collection: a locked package missing from the install stops the generation and is named, optional ones included', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'npm-missing-fixture-'));
  try {
    const lockPath = path.join(tmpDir, 'package-lock.json');
    const lockWith = (entry) => fs.writeFileSync(lockPath, JSON.stringify({ lockfileVersion: 3, packages: { '': {}, 'node_modules/opt': { version: '1.0.0', license: 'MIT', ...entry } } }));
    // Required and missing: the install is incomplete.
    lockWith({});
    assert.throws(() => collectNpmDependencies(lockPath, tmpDir), /node_modules\/opt is not installed, but package-lock\.json locks opt@1\.0\.0: run npm ci/);
    // Optional and missing where npm would install it: --omit=optional, named as such.
    lockWith({ optional: true });
    assert.throws(() => collectNpmDependencies(lockPath, tmpDir), /locks the optional opt@1\.0\.0, which is not installed: .*--omit=optional/);
    lockWith({ devOptional: true, os: [process.platform] });
    assert.throws(() => collectNpmDependencies(lockPath, tmpDir), /locks the optional opt@1\.0\.0, which is not installed/);
    // Optional for another platform: this platform cannot read its files, and the generation says so.
    const elsewhere = process.platform === 'aix' ? 'sunos' : 'aix';
    lockWith({ optional: true, os: [elsewhere] });
    assert.throws(() => collectNpmDependencies(lockPath, tmpDir), new RegExp(`locks the optional opt@1\\.0\\.0, which npm installs only for os ${elsewhere}, so this platform cannot read its license files`));
    lockWith({ optional: true, cpu: [`!${process.arch}`] });
    assert.throws(() => collectNpmDependencies(lockPath, tmpDir), /which npm installs only for cpu !.*, so this platform cannot read/);
    // npm also takes a single value as a string: the diagnostic names it the same way.
    lockWith({ optional: true, os: elsewhere });
    assert.throws(() => collectNpmDependencies(lockPath, tmpDir), new RegExp(`locks the optional opt@1\\.0\\.0, which npm installs only for os ${elsewhere}, so this platform cannot read its license files`));
    lockWith({ optional: true, os: elsewhere, cpu: `!${process.arch}` });
    assert.throws(() => collectNpmDependencies(lockPath, tmpDir), new RegExp(`which npm installs only for os ${elsewhere} and cpu !${process.arch}, so this platform cannot read`));
    lockWith({ optional: true, os: process.platform });
    assert.throws(() => collectNpmDependencies(lockPath, tmpDir), /locks the optional opt@1\.0\.0, which is not installed/);
    // Installed, an optional package is collected like any other.
    lockWith({ optional: true });
    fs.mkdirSync(path.join(tmpDir, 'node_modules/opt'), { recursive: true });
    fs.writeFileSync(path.join(tmpDir, 'node_modules/opt/LICENSE'), 'MIT License opt');
    fs.writeFileSync(path.join(tmpDir, 'node_modules/opt/package.json'), JSON.stringify({ name: 'opt', version: '1.0.0' }));
    assert.deepStrictEqual(collectNpmDependencies(lockPath, tmpDir).map((d) => `${d.name}@${d.version}`), ['opt@1.0.0']);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('npm collection: os and cpu lists are read the way npm reads them', () => {
  assert.strictEqual(npmInstallsHere(undefined, 'linux'), true);
  assert.strictEqual(npmInstallsHere([], 'linux'), true);
  assert.strictEqual(npmInstallsHere(['linux'], 'linux'), true);
  assert.strictEqual(npmInstallsHere(['darwin', 'win32'], 'linux'), false);
  assert.strictEqual(npmInstallsHere(['!win32'], 'linux'), true);
  assert.strictEqual(npmInstallsHere(['!win32'], 'win32'), false);
  assert.strictEqual(npmInstallsHere('linux', 'linux'), true);
  assert.strictEqual(npmInstallsHere('darwin', 'linux'), false);
  assert.strictEqual(npmInstallsHere('!win32', 'win32'), false);
});

test('npm collection: no locked production package is limited to some platforms, so any platform reads every license file', () => {
  const lock = JSON.parse(fs.readFileSync(path.join(ROOT, 'package-lock.json'), 'utf8'));
  const limited = Object.entries(lock.packages).filter(([p, info]) => p !== '' && !info.dev && (info.os || info.cpu)).map(([p]) => p);
  assert.deepStrictEqual(limited, [], 'a production package installed only on some platforms needs a reviewed source for its license files on the others');
});

test('npm authors: the manifest author is read in each form npm writes it, a url and a nameless author name no one', () => {
  assert.deepStrictEqual(npmAuthors({ author: 'Jane Doe <jane@example.com> (https://example.com)' }), ['Jane Doe <jane@example.com>']);
  assert.deepStrictEqual(npmAuthors({ author: 'Jane Doe <jane@example.com>' }), ['Jane Doe <jane@example.com>']);
  assert.deepStrictEqual(npmAuthors({ author: 'Jane Doe (https://example.com)' }), ['Jane Doe']);
  assert.deepStrictEqual(npmAuthors({ author: '  Jane Doe  ' }), ['Jane Doe']);
  assert.deepStrictEqual(
    npmAuthors({ author: 'Jane Doe (https://user:s3cret@example.com) <jane@example.com>' }),
    ['Jane Doe <jane@example.com>'],
    'reordered Name (URL) <email> strips the URL and keeps name and email'
  );
  assert.deepStrictEqual(npmAuthors({ author: { name: 'Jane Doe', email: 'jane@example.com', url: 'https://example.com' } }), ['Jane Doe <jane@example.com>']);
  assert.deepStrictEqual(npmAuthors({ author: { name: 'Jane Doe', url: 'https://example.com' } }), ['Jane Doe']);
  for (const author of [
    undefined,
    '',
    '   ',
    '<jane@example.com>',
    '(https://example.com)',
    'Jane Doe https://user:s3cret@example.com',
    'Jane Doe (https://a.example) (https://b.example)',
    { email: 'jane@example.com' },
    { name: '' },
    { name: 'Jane Doe https://user:s3cret@example.com' },
    { name: 42 },
    ['Jane Doe'],
    42,
    null,
  ]) {
    assert.strictEqual(npmAuthors({ author }), undefined, JSON.stringify(author));
  }
});

test('npm authors: the installed manifest gives a package its authors, and with none the reviewed attribution is still required', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'npm-authors-fixture-'));
  try {
    const manifests = {
      'test-npm-string': { author: 'Jane Doe <jane@example.com> (https://example.com)' },
      'test-npm-reordered': { author: 'Alice Roe (https://user:s3cret-token@example.com) <alice@example.com>' },
      'test-npm-object': { author: { name: 'John Roe', email: 'john@example.com', url: 'https://example.com' } },
      'test-npm-unparseable': { author: 'Bob Raw https://user:s3cret-token@example.com' },
      'test-npm-none': {},
    };
    const packages = { '': {} };
    for (const [name, extra] of Object.entries(manifests)) {
      packages[`node_modules/${name}`] = { version: '1.0.0', license: 'Apache-2.0' };
      fs.mkdirSync(path.join(tmpDir, 'node_modules', name), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, 'node_modules', name, 'package.json'), JSON.stringify({ name, version: '1.0.0', ...extra }));
    }
    const lockPath = path.join(tmpDir, 'package-lock.json');
    fs.writeFileSync(lockPath, JSON.stringify({ lockfileVersion: 3, packages }));
    const deps = collectNpmDependencies(lockPath, tmpDir);
    assert.deepStrictEqual(deps.map((d) => [d.name, d.authors]), [
      ['test-npm-none', undefined],
      ['test-npm-object', ['John Roe <john@example.com>']],
      ['test-npm-reordered', ['Alice Roe <alice@example.com>']],
      ['test-npm-string', ['Jane Doe <jane@example.com>']],
      ['test-npm-unparseable', undefined],
    ]);
    const validDeps = deps.filter((d) => d.name !== 'test-npm-none' && d.name !== 'test-npm-unparseable');
    const md = generateNoticesMarkdown(validDeps, []);
    assert.match(md, /- `test-npm-object@1\.0\.0` - SPDX: `Apache-2\.0` \(authors: John Roe <john@example\.com>\)/);
    assert.match(md, /- `test-npm-reordered@1\.0\.0` - SPDX: `Apache-2\.0` \(authors: Alice Roe <alice@example\.com>\)/);
    assert.match(md, /- `test-npm-string@1\.0\.0` - SPDX: `Apache-2\.0` \(authors: Jane Doe <jane@example\.com>\)/);
    assert.doesNotMatch(md, /s3cret-token|https:\/\/example\.com/);
    assert.throws(
      () => generateNoticesMarkdown(deps.filter((d) => d.name === 'test-npm-none'), []),
      /Component test-npm-none@1\.0\.0 has no manifest authors and is missing reviewed attribution/
    );
    assert.throws(
      () => generateNoticesMarkdown(deps.filter((d) => d.name === 'test-npm-unparseable'), []),
      /Component test-npm-unparseable@1\.0\.0 has no manifest authors and is missing reviewed attribution/
    );
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('directory order: the same package contents give the same notices in any enumeration order', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'dir-order-fixture-'));
  const original = fs.readdirSync;
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', shippedLicense('MIT License\n\nCopyright (c) 2020 Order Author', 'MIT'));
    write('src/a.c', '/* Copyright (c) 2001 Alpha Vendor\n * SPDX-License-Identifier: BSD-3-Clause */\n');
    write('src/b.c', '/* Copyright (c) 2002 Beta Vendor\n * SPDX-License-Identifier: BSD-3-Clause */\n');
    write('src/c.rs', '// Copyright 2003 Gamma Holder\n');
    const render = () => {
      const comp = { name: 'order-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Order Author'], ...readPackageFiles(tmpDir, 'order-pkg@1.0.0') };
      return generateNoticesMarkdown([comp], []);
    };
    const sorted = render();
    // Windows keeps the file system's order: the reverse of the sorted one here.
    fs.readdirSync = (...args) => original(...args).reverse();
    syncBuiltinESMExports();
    const reversed = render();
    assert.match(sorted, /bundles code under BSD-3-Clause in `src\/a\.c`, `src\/b\.c`/);
    assert.strictEqual(reversed, sorted);
  } finally {
    fs.readdirSync = original;
    syncBuiltinESMExports();
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('readPackageFiles: collects COPYRIGHT files into noticeFiles without suppressing license fallback', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'pkg-copyright-test-'));
  try {
    fs.writeFileSync(path.join(tmpDir, 'COPYRIGHT'), 'Copyright (c) 2024 Test Project Contributors');
    fs.writeFileSync(path.join(tmpDir, 'LICENSE'), 'MIT License text');
    const { licenseFiles, noticeFiles } = readPackageFiles(tmpDir, 'test-copyright-crate@1.0.0');
    assert.strictEqual(licenseFiles.length, 1);
    assert.strictEqual(noticeFiles.length, 1);
    assert.strictEqual(licenseFiles[0].filename, 'LICENSE');
    assert.strictEqual(noticeFiles[0].filename, 'COPYRIGHT');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('readPackageFiles: the third-party notices a package collects are notice files, at the root, in a license folder and in vendored code', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'third-party-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', 'MIT License');
    // The names real packages use: monaco-editor, uds_windows, security-framework, rolldown.
    const names = ['ThirdPartyNotices.txt', 'THIRDPARTYNOTICES', 'THIRD_PARTY', 'THIRD-PARTY-LICENSE', 'third-party-licenses.md', '3rd-party-notices.txt', 'ThirdPartyNoticeText.txt'];
    for (const name of names) write(name, `Notices in ${name}`);
    write('LICENSES/ThirdPartyNotices.txt', 'Notices in the license folder');
    write('vendor/lib/ThirdPartyNotices.txt', 'Notices of vendored code');
    // Code and data named like one stay code and data.
    write('third_party.rs', '// Copyright (c) 2020 Code');
    write('third-party.json', '{}');
    write('vendor/lib/third_party.js', 'module.exports = {};');
    const { licenseFiles, noticeFiles } = readPackageFiles(tmpDir, 'third-party-pkg@1.0.0');
    assert.deepStrictEqual(licenseFiles.map((f) => f.filename), ['LICENSE']);
    assert.deepStrictEqual(
      noticeFiles.map((f) => [f.filename, f.vendored === true]).sort(),
      [...names.map((n) => [n, false]), ['LICENSES/ThirdPartyNotices.txt', false], ['vendor/lib/ThirdPartyNotices.txt', true]].sort()
    );
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('readPackageFiles: fails fast on missing package directory (no silent absence)', () => {
  assert.throws(
    () => readPackageFiles('/non/existent/package/dir/12345', 'dummy-pkg@1.0.0'),
    /Package directory for dummy-pkg@1\.0\.0 does not exist/
  );
});

test('readPackageFiles: resolves declared nested license_file and fails fast if missing', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'crate-license-file-fixture-'));
  try {
    const legalDir = path.join(tmpDir, 'legal');
    fs.mkdirSync(legalDir, { recursive: true });
    fs.writeFileSync(path.join(legalDir, 'terms.txt'), 'Custom terms and conditions');

    const result = readPackageFiles(tmpDir, 'custom-crate@1.0.0', 'legal/terms.txt');
    assert.strictEqual(result.licenseFiles.length, 1);
    assert.strictEqual(result.licenseFiles[0].filename, 'legal/terms.txt');
    assert.strictEqual(result.licenseFiles[0].content, 'Custom terms and conditions');
    // The declared nested license file is excluded from source copyright scanning.
    fs.writeFileSync(path.join(legalDir, 'terms.txt'), 'Copyright (c) 2024 Nested Holder\nCustom terms');
    const nestedRead = readPackageFiles(tmpDir, 'custom-crate@1.0.0', 'legal/terms.txt');
    assert.deepStrictEqual(
      findPackageCopyrightNotices(tmpDir, nestedRead.licenseFiles.map((f) => f.filename)),
      [],
      'declared nested license_file must not be re-scanned as source'
    );
    // Root-level declared license_file keeps its root filename unchanged.
    fs.writeFileSync(path.join(tmpDir, 'LICENSE-ROOT'), 'Root terms');
    const rootRead = readPackageFiles(tmpDir, 'custom-crate@1.0.0', 'LICENSE-ROOT');
    assert.strictEqual(rootRead.licenseFiles[0].filename, 'LICENSE-ROOT');

    // Fails fast if declared license_file does not exist
    assert.throws(
      () => readPackageFiles(tmpDir, 'custom-crate@1.0.0', 'legal/missing-file.txt'),
      /Crate custom-crate@1\.0\.0 declared license_file "legal\/missing-file\.txt" but the file does not exist/
    );
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('readPackageFiles: a declared license_file outside the crate directory is refused before it is read', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'crate-license-outside-fixture-'));
  try {
    const crate = path.join(tmpDir, 'crate');
    fs.mkdirSync(path.join(crate, 'legal'), { recursive: true });
    fs.writeFileSync(path.join(tmpDir, 'LICENSE'), 'Workspace terms the crate does not ship');
    fs.writeFileSync(path.join(crate, 'LICENSE-MIT'), 'MIT License');
    const outside = /declared license_file ".*", which is outside the crate directory/;
    for (const declared of ['../LICENSE', path.join(tmpDir, 'LICENSE'), 'legal/../../LICENSE']) {
      assert.throws(() => readPackageFiles(crate, 'shared-crate@1.0.0', declared), outside, declared);
    }
    // A directory at the declared path is no license text.
    assert.throws(() => readPackageFiles(crate, 'shared-crate@1.0.0', 'legal'), /declared license_file "legal", which is not a file/);
    // A path inside the crate, also one that passes through `..`, is still read.
    const inside = readPackageFiles(crate, 'shared-crate@1.0.0', 'legal/../LICENSE-MIT');
    assert.deepStrictEqual(inside.licenseFiles.map((f) => [f.filename, f.content]), [['LICENSE-MIT', 'MIT License']]);
    assert.deepStrictEqual(readPackageFiles(crate, 'shared-crate@1.0.0', path.join(crate, 'LICENSE-MIT')).licenseFiles.map((f) => f.filename), ['LICENSE-MIT']);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

// A link is a case of its own: creating one needs a permission Windows grants
// apart from writing files, so the test reports a skip only when the link
// cannot be made, and the path cases above run everywhere.
test('readPackageFiles: a declared license_file that links outside the crate directory is refused', (t) => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'crate-license-link-fixture-'));
  try {
    const crate = path.join(tmpDir, 'crate');
    fs.mkdirSync(crate, { recursive: true });
    fs.writeFileSync(path.join(tmpDir, 'LICENSE'), 'Workspace terms the crate does not ship');
    try {
      fs.symlinkSync(path.join(tmpDir, 'LICENSE'), path.join(crate, 'LINKED-LICENSE'));
    } catch (e) {
      t.skip(`a file link cannot be created here: ${e.code}`);
      return;
    }
    assert.throws(() => readPackageFiles(crate, 'shared-crate@1.0.0', 'LINKED-LICENSE'), /declared license_file "LINKED-LICENSE", which is outside the crate directory/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('shared SPDX templates: generic attribution without component-specific leakage', () => {
  const jsonPath = path.join(ROOT, 'scripts', 'spdx-standard-licenses.json');
  const json = JSON.parse(fs.readFileSync(jsonPath, 'utf8'));

  // Ensure templates use generic year and copyright holders instead of specific authors
  assert.match(json['BSD-2-Clause'], /<copyright holders>/);
  assert.doesNotMatch(json['BSD-2-Clause'], /David Roundy/);

  assert.match(json['BSD-3-Clause'], /<copyright holders>/);
  assert.doesNotMatch(json['BSD-3-Clause'], /Dropbox/);

  assert.match(json['ISC'], /<copyright holders>/);
  assert.doesNotMatch(json['ISC'], /Hanno Braun/);

  assert.doesNotMatch(json['0BSD'], /Jonas Schievink/);
  assert.doesNotMatch(json['MIT-0'], /ppmd-rust/);
});

test('groupLicenseTexts: NOTICE-only component receives both standard license and notice attribution', () => {
  const noticeOnlyComponent = {
    name: 'apache-notice-only-crate',
    version: '1.0.0',
    license: 'Apache-2.0',
    source: 'cargo',
    licenseFiles: [],
    noticeFiles: [{ filename: 'NOTICE', content: 'Apache Notice Attribution' }],
  };

  const { groupedTexts, componentsWithoutFiles } = groupLicenseTexts([noticeOnlyComponent]);

  // Notice attribution must be retained in groupedTexts
  assert.strictEqual(groupedTexts.length, 1);
  assert.strictEqual(groupedTexts[0].text, 'Apache Notice Attribution');

  // Component must also be in componentsWithoutFiles to receive standard Apache-2.0 text
  assert.strictEqual(componentsWithoutFiles.length, 1);
  assert.strictEqual(componentsWithoutFiles[0].name, 'apache-notice-only-crate');
});

test('standard SPDX texts: MIT-0 receives distinct MIT-0 section and is not conflated with MIT', () => {
  const dummyComponent = {
    name: 'test-mit0-crate',
    version: '1.0.0',
    license: 'MIT-0',
    source: 'cargo',
    licenseFiles: [],
    noticeFiles: [],
    authors: ['Test Author <author@example.com>'],
  };

  const reviewed = { ...PINNED_FALLBACK_ATTRIBUTIONS, 'test-mit0-crate@1.0.0': { author: 'Test Author <author@example.com>', source: 'test fixture' } };
  const md = generateNoticesMarkdown([dummyComponent], [], SPDX_STANDARD_LICENSES_FILE, KNOWN_NON_FREE_EXCEPTIONS, undefined, reviewed);
  assert.match(md, /### Standard License: MIT-0/);
  // Verify test-mit0-crate is listed under MIT-0 and not under MIT
  const mit0Section = md.slice(md.indexOf('### Standard License: MIT-0'));
  assert.match(mit0Section, /test-mit0-crate@1\.0\.0/);
  assert.doesNotMatch(md, /### Standard License: MIT(\r?\n|$)/);
});

test('standard SPDX texts: fails fast with component name if a required standard text is missing from json', () => {
  const dummyComponent = {
    name: 'test-unmapped-crate',
    version: '1.2.3',
    license: 'CUSTOM-UNKNOWN-LICENSE-ID',
    source: 'cargo',
    licenseFiles: [],
    noticeFiles: [],
    authors: ['Test Author'],
  };

  assert.throws(
    () => generateNoticesMarkdown([dummyComponent], []),
    /Component test-unmapped-crate@1\.2\.3 requires standard license text for 'CUSTOM-UNKNOWN-LICENSE-ID'/
  );
});

test('standard SPDX texts: accepts OR branch present in standard licenses without requiring unaccepted branch (MIT OR SSPL-1.0)', () => {
  const dummyComponent = {
    name: 'dual-licensed-crate',
    version: '1.0.0',
    license: 'MIT OR SSPL-1.0',
    source: 'cargo',
    licenseFiles: [],
    noticeFiles: [],
    authors: ['Dual Author'],
  };

  // Must succeed rendering the accepted MIT branch without throwing for SSPL-1.0
  const md = generateNoticesMarkdown([dummyComponent], []);
  assert.match(md, /### Standard License: MIT/);
  assert.match(md, /dual-licensed-crate@1\.0\.0/);
});

test('standard SPDX texts: a shipped text covers a term only when it carries every clause, similarity alone is not coverage', () => {
  const comp = (name, license, files) => ({
    name,
    version: '1.0.0',
    license,
    source: 'cargo',
    authors: ['Clause Author'],
    licenseFiles: files.map(([filename, content]) => ({ filename, content })),
    noticeFiles: [],
  });
  // A phrase of the standard text, whatever line breaks the text has.
  const words = (phrase, flags = '') => new RegExp(phrase.split(' ').map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('\\s+'), flags);
  const clausesReviewed = { ...PINNED_FALLBACK_ATTRIBUTIONS, 'test-clauses@1.0.0': { author: 'Clause Author', source: 'test fixture' } };
  const standardFor = (license, text) => [...generateNoticesMarkdown([comp('test-clauses', license, [['LICENSE', text]])], [], SPDX_STANDARD_LICENSES_FILE, KNOWN_NON_FREE_EXCEPTIONS, undefined, clausesReviewed).matchAll(/^### Standard License: (.+)$/gm)].map((m) => m[1]);
  const apache = shippedLicense('', 'Apache-2.0');
  assert.deepStrictEqual(standardFor('Apache-2.0', apache), [], 'the whole text covers its term');
  const no4a = apache.replace(words('(a) You must give any other recipients of the Work or Derivative Works a copy of this License; and'), '');
  assert.notStrictEqual(no4a, apache);
  assert.deepStrictEqual(standardFor('Apache-2.0', no4a), ['Apache-2.0'], 'section 4(a) removed keeps the standard text');
  const no3 = apache.slice(0, apache.indexOf('3. Grant of Patent License')) + apache.slice(apache.indexOf('4. Redistribution'));
  assert.deepStrictEqual(standardFor('Apache-2.0', no3), ['Apache-2.0'], 'the patent grant removed, though its words repeat the copyright grant');
  // Shipped texts insert words, add clauses and number lists their own way: still the whole text.
  const mit = shippedLicense('MIT License\n\nCopyright (c) 2020 Clause Holder', 'MIT');
  const inserted = mit.replace(words('this permission notice shall'), 'this permission notice (including the next paragraph) shall');
  assert.notStrictEqual(inserted, mit);
  assert.deepStrictEqual(standardFor('MIT', inserted), []);
  // Only a reviewed insertion: one that reverses or conditions the grant, or
  // any other words inside a sentence, is another text.
  for (const [from, to] of [
    ['Permission is hereby granted', 'Permission is hereby not granted'],
    ['Permission is hereby granted', 'Permission is never hereby granted'],
    ['Permission is hereby granted', 'Permission is hereby, subject to written approval, granted'],
    ['this permission notice shall', 'this permission notice (excluding the next paragraph) shall'],
    ['WITHOUT WARRANTY OF ANY KIND', 'WITHOUT WARRANTY OF ANY KIND EXCEPT'],
  ]) {
    const changed = mit.replace(words(from), to);
    assert.notStrictEqual(changed, mit, to);
    assert.deepStrictEqual(standardFor('MIT', changed), ['MIT'], to);
  }
  const extraClause = mit.replace(words('substantial portions of the Software.'), 'substantial portions of the Software.\n\nDistributions of modified copies shall replace the Original Author\'s bug reporting addresses with those of the parties responsible for the changes, or remove them entirely.');
  assert.notStrictEqual(extraClause, mit);
  // Fail-closed: a clause added between two standard sentences makes another
  // text, so the standard text is reproduced beside it.
  assert.deepStrictEqual(standardFor('MIT', extraClause), ['MIT']);
  // Nothing before the first sentence may turn it: the terms start at a
  // sentence or paragraph boundary.
  for (const lead of ['No permission is hereby granted', 'No such permission is hereby granted', 'Neither permission is hereby granted']) {
    const changed = mit.replace(words('Permission is hereby granted'), lead);
    assert.notStrictEqual(changed, mit, lead);
    assert.deepStrictEqual(standardFor('MIT', changed), ['MIT'], lead);
  }
  // A number in a term is a term; a list marker is not.
  const lgpl = shippedLicense('', 'LGPL-2.1-or-later');
  const gpl3 = lgpl.replace(words('instead of to this License. (If a newer version than version 2 of the ordinary GNU General Public License has appeared'), 'instead of to this License. (If a newer version than version 3 of the ordinary GNU General Public License has appeared');
  assert.notStrictEqual(gpl3, lgpl);
  assert.deepStrictEqual(standardFor('LGPL-2.1-or-later', lgpl), []);
  assert.deepStrictEqual(standardFor('LGPL-2.1-or-later', gpl3), ['LGPL-2.1-or-later']);
  const renumbered = lgpl.replace(/^(\s*)(\d+)\. /gm, (m, sp, n) => `${sp}${n})  `);
  assert.notStrictEqual(renumbered, lgpl);
  assert.deepStrictEqual(standardFor('LGPL-2.1-or-later', renumbered), []);
  const noCondition = mit.replace(words('The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.'), '');
  assert.notStrictEqual(noCondition, mit);
  assert.deepStrictEqual(standardFor('MIT', noCondition), ['MIT'], 'the notice condition removed keeps the standard text');
  // A slot of the SPDX template holds the holder's own name; nothing else may change.
  const bsd3 = shippedLicense('Copyright (c) 2019 Example Corp', 'BSD-3-Clause');
  const named = bsd3.replace(words('the copyright holder nor'), 'Example Corp nor').replace(words('THE COPYRIGHT HOLDERS AND CONTRIBUTORS'), 'EXAMPLE CORP AND ITS CONTRIBUTORS').replace(words('THE COPYRIGHT HOLDER OR CONTRIBUTORS'), 'EXAMPLE CORP');
  assert.strictEqual(named.match(/copyright holder/gi), null);
  assert.deepStrictEqual(standardFor('BSD-3-Clause', named), []);
  // A slot holds a name, never a word that turns what its sentence says.
  for (const negated of [named.replace(words('EXAMPLE CORP BE LIABLE'), 'EXAMPLE CORP NOT BE LIABLE'), named.replace(words('Example Corp nor'), 'Example Corp, except its affiliates, nor')]) {
    assert.notStrictEqual(negated, named);
    assert.deepStrictEqual(standardFor('BSD-3-Clause', negated), ['BSD-3-Clause']);
  }
  const bulleted = bsd3.replace(/^\s*([123])\. /gm, '* ');
  assert.notStrictEqual(bulleted, bsd3);
  assert.deepStrictEqual(standardFor('BSD-3-Clause', bulleted), []);
  const endorse = bsd3.replace(words('without specific prior written permission'), 'with specific prior written permission');
  assert.notStrictEqual(endorse, bsd3);
  assert.deepStrictEqual(standardFor('BSD-3-Clause', endorse), ['BSD-3-Clause'], 'one word changed outside a slot keeps the standard text');
  const isc = shippedLicense('Copyright (c) 2015 Clause Holder', 'ISC');
  const plural = isc.replace(words('THE AUTHOR DISCLAIMS'), 'THE AUTHORS DISCLAIM').replace(words('THE AUTHOR BE LIABLE'), 'THE AUTHORS BE LIABLE');
  assert.notStrictEqual(plural, isc);
  assert.deepStrictEqual(standardFor('ISC', plural), [], 'the verb agrees with a plural holder');
  assert.ok(isc.search(words('IN NO EVENT SHALL THE AUTHOR BE LIABLE')) > 0);
  const noLiability = isc.slice(0, isc.search(words('IN NO EVENT SHALL THE AUTHOR BE LIABLE')));
  assert.deepStrictEqual(standardFor('ISC', noLiability), ['ISC']);
  // A version of a standard text with a clause removed is no grant of its own for a term with no standard text.
  assert.throws(
    () => generateNoticesMarkdown([comp('test-clauses', 'Apache-2.0 AND bzip2-1.0.6', [['LICENSE', no4a]])], []),
    /Component test-clauses@1\.0\.0 requires standard license text for 'bzip2-1\.0\.6'/
  );
});

test('reviewed terms: each license with no standard text has its whole terms from the SPDX list, and nothing else', () => {
  const reviewed = reviewedLicenseTerms();
  assert.deepStrictEqual(Object.keys(reviewed).sort(), ['CDLA-Permissive-2.0', 'FSFULLR', 'SAX-PD-2.0', 'bzip2-1.0.6', 'curl']);
  assert.match(reviewed['bzip2-1.0.6'], /^Redistribution and use in source and binary forms/);
  assert.match(reviewed['bzip2-1.0.6'], /EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE\.\n$/);
  assert.match(reviewed['CDLA-Permissive-2.0'], /Community Data License Agreement - Permissive - Version 2\.0/);
  const raw = JSON.parse(fs.readFileSync(path.join(ROOT, 'scripts', 'reviewed-license-terms.json'), 'utf8'));
  for (const id of Object.keys(reviewed)) assert.match(raw[id].source, new RegExp(`^https://spdx\\.org/licenses/${id.replace(/\./g, '\\.')}\\.html$`));
  // Pinned outside the JSON: the SHA-256 of each reviewed text, read from the
  // SPDX license-list-data text (text/<id>.txt, 2026-11-18 and, for FSFULLR,
  // curl and SAX-PD-2.0, 2026-11-19), whose whole terms it is: the title, the
  // copyright lines and the signature are not terms. An edit of the JSON (a
  // condition dropped) changes the digest.
  const REVIEWED_DIGESTS = {
    'bzip2-1.0.6': 'c6ecc45e3a362d5e8e4a6aa9dfeddb1ece6c58032b6c2516595c18bacd06a58e',
    'CDLA-Permissive-2.0': '4531a67d443284d93ffed0803df5b10634aff21c3d77e381f2d48af01d875868',
    FSFULLR: 'dd665685731358287f85adee4a403b3086d9c366658d6385ef89e590da6907cc',
    curl: '5030e2413942548871ce5f9a72912742b1427ae02e188043c260f7cd9a92c3d9',
    'SAX-PD-2.0': '9a4aee2fad43f2943dfa34f832163b2b86a8af70087a1e860f60209403ecbeef',
  };
  for (const [id, digest] of Object.entries(REVIEWED_DIGESTS)) {
    assert.strictEqual(createHash('sha256').update(raw[id].terms).digest('hex'), digest, id);
  }
  const no4 = raw['bzip2-1.0.6'].terms.replace(/4\. The name of the author[^]*?permission\.\n/, '');
  assert.notStrictEqual(no4, raw['bzip2-1.0.6'].terms, 'the fixture drops condition 4');
  assert.notStrictEqual(createHash('sha256').update(no4).digest('hex'), REVIEWED_DIGESTS['bzip2-1.0.6']);
});

test('standard SPDX texts: LGPL-2.1, LGPL-2.1-only and LGPL-2.1+ share the LGPL-2.1 document and keep the grant they declare', () => {
  // SPDX: the deprecated LGPL-2.1 means LGPL-2.1-only, LGPL-2.1+ means
  // LGPL-2.1-or-later; the license document is one text.
  assert.strictEqual(standardTextId('LGPL-2.1'), 'LGPL-2.1-or-later');
  assert.strictEqual(standardTextId('LGPL-2.1-only'), 'LGPL-2.1-or-later');
  assert.strictEqual(standardTextId('LGPL-2.1+'), 'LGPL-2.1-or-later');
  assert.strictEqual(standardTextId('LGPL-2.1-or-later'), 'LGPL-2.1-or-later');
  assert.strictEqual(standardTextId('MIT'), 'MIT');
  assert.strictEqual(isGplCompatible('LGPL-2.1-only'), true);
  assert.strictEqual(isGplCompatible('LGPL-2.1'), true);
  const tmpDirs = [];
  const make = (name, license, files = {}) => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'lgpl-label-fixture-'));
    tmpDirs.push(dir);
    for (const [rel, content] of Object.entries(files)) {
      fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
      fs.writeFileSync(path.join(dir, rel), content);
    }
    return { name, version: '1.0.0', license, source: 'cargo', packageDir: dir, authors: [`${name} Author`], ...readPackageFiles(dir, `${name}@1.0.0`) };
  };
  const document = 'Version 2.1, February 1999';
  const count = (md, s) => md.split(s).length - 1;
  try {
    // No own files: one section names every grant, the document appears once,
    // and each package keeps the identifier it declares.
    let md = generateNoticesMarkdown([make('bare-lgpl', 'LGPL-2.1'), make('only-lgpl', 'LGPL-2.1-only'), make('plus-lgpl', 'LGPL-2.1+'), make('later-lgpl', 'LGPL-2.1-or-later')], []);
    assert.match(md, /\n### Standard License: LGPL-2\.1, LGPL-2\.1\+, LGPL-2\.1-only, LGPL-2\.1-or-later\n/);
    assert.strictEqual(count(md, document), 1, 'the LGPL-2.1 document appears once');
    for (const [name, id] of [['bare-lgpl', 'LGPL-2.1'], ['only-lgpl', 'LGPL-2.1-only'], ['plus-lgpl', 'LGPL-2.1+'], ['later-lgpl', 'LGPL-2.1-or-later']]) {
      assert.ok(md.includes(`- \`${name}@1.0.0\` - SPDX: \`${id}\``), `${name} keeps ${id}`);
    }
    // A bare LGPL-2.1 alone is never labelled as an or-later grant.
    md = generateNoticesMarkdown([make('bare-alone', 'LGPL-2.1')], []);
    assert.match(md, /\n### Standard License: LGPL-2\.1\n/);
    assert.doesNotMatch(md, /### Standard License: [^\n]*or-later/);
    // An own LGPL-2.1 text covers the bare and the -only grant (acl-sys).
    md = generateNoticesMarkdown([make('own-lgpl', 'LGPL-2.1-only', { LICENSE: STANDARD_TEXTS['LGPL-2.1-or-later'] })], []);
    assert.doesNotMatch(md, /### Standard License:/);
    assert.strictEqual(count(md, document), 1);
    // A bundled header under LGPL-2.1-only has its text, under its own label,
    // and two spellings of the same grant share one section.
    md = generateNoticesMarkdown([make('bundling-lgpl', 'MIT', {
      LICENSE: shippedLicense('MIT License\n\nCopyright (c) 2020 bundling-lgpl Author', 'MIT'),
      'src/a.c': '/* Copyright 2001 Only Header Author\n * SPDX-License-Identifier: LGPL-2.1-only */\nint a;\n',
      'src/b.c': '/* Copyright 2002 Bare Header Author\n * SPDX-License-Identifier: LGPL-2.1 */\nint b;\n',
    })], []);
    assert.match(md, /\n### Bundled Code: LGPL-2\.1, LGPL-2\.1-only\n/);
    assert.ok(md.includes('bundles code under LGPL-2.1-only in `src/a.c`'));
    assert.ok(md.includes('bundles code under LGPL-2.1 in `src/b.c`'));
    assert.strictEqual(count(md, document), 1);
  } finally {
    for (const d of tmpDirs) fs.rmSync(d, { recursive: true, force: true });
  }
});

test('standard SPDX texts: every term of the chosen license needs its text, an own file covers only the terms whose text it carries', () => {
  const mit = shippedLicense('MIT License\n\nCopyright (c) 2020 Term Holder', 'MIT');
  const apache = shippedLicense('', 'Apache-2.0');
  const comp = (name, license, files) => ({
    name,
    version: '1.0.0',
    license,
    source: 'cargo',
    authors: ['Term Author'],
    licenseFiles: files.map(([filename, content]) => ({ filename, content })),
    noticeFiles: [],
  });
  const standardFor = (c) => {
    const fb = { ...PINNED_FALLBACK_ATTRIBUTIONS, [`${c.name}@${c.version}`]: { author: c.authors?.[0] || 'Term Author', source: 'test fixture' } };
    return [...generateNoticesMarkdown([c], [], SPDX_STANDARD_LICENSES_FILE, KNOWN_NON_FREE_EXCEPTIONS, undefined, fb).matchAll(/^### Standard License: (.+)$/gm)].map((m) => m[1]);
  };
  // A conjunction with one text shipped gets the standard text of the other.
  assert.deepStrictEqual(standardFor(comp('test-and-one', 'MIT AND Apache-2.0', [['LICENSE-MIT', mit]])), ['Apache-2.0']);
  // Controls: both texts shipped, or the single license shipped, add nothing.
  assert.deepStrictEqual(standardFor(comp('test-and-both', 'MIT AND Apache-2.0', [['LICENSE-MIT', mit], ['LICENSE-APACHE', apache]])), []);
  assert.deepStrictEqual(standardFor(comp('test-mit', 'MIT', [['LICENSE', mit]])), []);
  // A choice the files cover is enough: no Apache-2.0 text for MIT OR Apache-2.0.
  assert.deepStrictEqual(standardFor(comp('test-or', 'MIT OR Apache-2.0', [['LICENSE-MIT', mit]])), []);
  // A file that names the licenses without their texts carries none (siphasher's COPYING).
  const pointer = 'Copyright 2016 Pointer Holder.\n\nLicensed under the Apache License, Version 2.0 or the MIT license, at your option.';
  assert.deepStrictEqual(standardFor(comp('test-pointer', 'MIT OR Apache-2.0', [['COPYING', pointer]])), ['Apache-2.0', 'MIT']);
  // A text inside a longer one is not the longer one, and the longer one is not it.
  assert.deepStrictEqual(standardFor(comp('test-mit-in-mit0', 'MIT', [['LICENSE', shippedLicense('', 'MIT-0')]])), ['MIT']);
  assert.deepStrictEqual(standardFor(comp('test-mit0-in-mit', 'MIT-0', [['LICENSE', mit]])), ['MIT-0']);
  // An exception is a term of its own: the Apache-2.0 text alone does not carry it.
  assert.deepStrictEqual(standardFor(comp('test-with', 'Apache-2.0 WITH LLVM-exception', [['LICENSE-APACHE', apache]])), ['Apache-2.0 WITH LLVM-exception']);
  assert.deepStrictEqual(standardFor(comp('test-with-both', 'Apache-2.0 WITH LLVM-exception', [['LICENSE-APACHE', `${apache}\n\n${shippedLicense('', 'LLVM-exception')}`]])), []);
  // Which sections an exception waives is part of its terms: an exception
  // that names 4(c) where the standard one names 4(d) is not the standard
  // exception, so its text is added; the unchanged one adds nothing.
  const llvm = shippedLicense('', 'LLVM-exception');
  assert.ok(llvm.includes('4(a), 4(b) and 4(d)'));
  const altered = llvm.replace('4(a), 4(b) and 4(d)', '4(a), 4(b) and 4(c)');
  assert.deepStrictEqual(standardFor(comp('test-with-altered', 'Apache-2.0 WITH LLVM-exception', [['LICENSE-APACHE', `${apache}\n\n${altered}`]])), ['Apache-2.0 WITH LLVM-exception']);
  // A term with no standard text is carried by a file that carries its whole
  // reviewed terms, whatever holder and version its attribution names;
  // without one the generation fails with the package and the term.
  const bzip2Terms = reviewedLicenseTerms()['bzip2-1.0.6'];
  const bzip2 = `The original program, "bzip2", the associated library "libbzip2", and all\ndocumentation, are\n\nCopyright (C) 1996-2021 Julian R Seward.\n\n${bzip2Terms}\nJulian Seward, jseward@acm.org\nbzip2/libbzip2 version 1.1.0 of 6 September 2010\n`;
  assert.deepStrictEqual(standardFor(comp('test-custom', 'MIT AND bzip2-1.0.6', [['LICENSE-MIT', mit], ['LICENSE', bzip2]])), []);
  // A grant that only names the license is no text of it: cut before the
  // conditions, before the disclaimer, or a short grant under its tag.
  const truncated = 'This program, "bzip2", the associated library "libbzip2", and all documentation, are copyright (C) 1996-2010 Julian R Seward. All rights reserved. Redistribution and use in source and binary forms, with or without modification, are permitted provided that the following conditions are met.';
  const noDisclaimer = bzip2.slice(0, bzip2.indexOf('THIS SOFTWARE IS PROVIDED'));
  const tagged = 'SPDX-License-Identifier: bzip2-1.0.6\n\nRedistribution and use in source and binary forms are permitted provided this notice is kept.';
  const negatedCondition = bzip2.replace('must retain the above copyright', 'need not retain the above copyright');
  assert.notStrictEqual(negatedCondition, bzip2);
  // A grant for another work is another license, and a file that only names the licenses carries no terms of its own.
  const custom = 'This program, "custom-zip", is free software. Redistribution and use in source and binary forms are permitted provided this notice is kept.';
  for (const files of [
    [['LICENSE-MIT', mit]],
    [['LICENSE-MIT', mit], ['COPYING', pointer]],
    [['LICENSE-MIT', mit], ['LICENSE', custom]],
    [['LICENSE-MIT', mit], ['LICENSE', truncated]],
    [['LICENSE-MIT', mit], ['LICENSE', noDisclaimer]],
    [['LICENSE-MIT', mit], ['LICENSE', tagged]],
    [['LICENSE-MIT', mit], ['LICENSE', negatedCondition]],
  ]) {
    assert.throws(
      () => generateNoticesMarkdown([comp('test-custom', 'MIT AND bzip2-1.0.6', files)], []),
      /Component test-custom@1\.0\.0 requires standard license text for 'bzip2-1\.0\.6'/
    );
  }
  // A choice whose missing texts exist wins over a better covered one that
  // needs a text nobody has.
  assert.deepStrictEqual(standardFor(comp('test-reproducible', '(MIT AND bzip2-1.0.6) OR Apache-2.0', [['LICENSE-MIT', mit]])), ['Apache-2.0']);
  assert.deepStrictEqual(standardFor(comp('test-reproducible', '(MIT AND bzip2-1.0.6) OR Apache-2.0', [['LICENSE-MIT', mit], ['LICENSE', custom]])), ['Apache-2.0']);
  assert.deepStrictEqual(standardFor(comp('test-reproducible', '(MIT AND bzip2-1.0.6) OR Apache-2.0', [['LICENSE-MIT', mit], ['LICENSE', bzip2]])), []);
  // Without manifest authors the holders the package's own files name are its
  // authors; with neither, a reviewed attribution is still required.
  const holderOnly = { ...comp('test-holder-only', 'MIT AND Apache-2.0', [['LICENSE-MIT', mit]]), authors: [] };
  assert.match(generateNoticesMarkdown([holderOnly], []), /- `test-holder-only@1\.0\.0` - SPDX: `MIT AND Apache-2\.0` \(authors: named in its license files\)\n {2}- The copyright notices it ships are reproduced above, with its license files \(`LICENSE-MIT`\)\n/);
  assert.throws(
    () => generateNoticesMarkdown([{ ...comp('test-no-holder', 'MIT AND Apache-2.0', [['LICENSE-MIT', shippedLicense('', 'MIT')]]), authors: [] }], []),
    /Component test-no-holder@1\.0\.0 has no manifest authors and is missing reviewed attribution/
  );
  // A standard text with a holder slot takes a reviewed holder, never one
  // guessed from the files of another license.
  assert.throws(
    () => generateNoticesMarkdown([comp('slot-pkg', 'MIT AND BSD-3-Clause', [['LICENSE-MIT', mit]])], []),
    /Component slot-pkg@1\.0\.0 requires an entry in PINNED_FALLBACK_ATTRIBUTIONS/
  );
  // The section says what is missing: the text of a declared license, not every license file.
  assert.match(generateNoticesMarkdown([comp('test-and-one', 'MIT AND Apache-2.0', [['LICENSE-MIT', mit]])], []), /need one or more standard license texts: their manifests declare the\nlicenses, and their package files do not include the text of each of them\./);
  const googleBsd3 = STANDARD_TEXTS['BSD-3-Clause']
    .replace(/^Copyright.*?\nAll rights reserved\.\n+/s, '')
    .replace(/^[123]\./gm, '   *')
    .replace('the copyright holder', 'Google Inc.')
    .replace('COPYRIGHT HOLDER', 'COPYRIGHT OWNER');
  assert.deepStrictEqual(standardFor(comp('test-google-bsd3', 'BSD-3-Clause', [['LICENSE', googleBsd3]])), []);
  assert.deepStrictEqual(standardFor(comp('test-google-bsd3-for-bsd2', 'BSD-2-Clause', [['LICENSE', googleBsd3]])), ['BSD-2-Clause']);
  assert.deepStrictEqual(standardFor(comp('test-combined-mit-mit0', 'MIT AND MIT-0', [['LICENSE', `${mit}\n\n${shippedLicense('', 'MIT-0')}`]])), []);

  const gpl2 = 'GNU GENERAL PUBLIC LICENSE\nVersion 2, June 1991\nEveryone is permitted to copy and distribute verbatim copies.\nYou may copy and distribute verbatim copies of the Program.';
  assert.deepStrictEqual(standardFor(comp('test-gpl2-or-apache', 'Apache-2.0 OR GPL-2.0-only', [['LICENSE-GPLv2', gpl2]])), ['Apache-2.0']);
  // A file that mentions a second license carries no terms of it: the
  // conjunct is still uncovered, with any number of files.
  const both = `${bzip2}\nThis notice also covers BSD-3-Clause-Clear.`;
  for (const files of [[['LICENSE', bzip2]], [['LICENSE', both]], [['LICENSE', both], ['LICENSE-BZIP2', bzip2]]]) {
    assert.throws(
      () => generateNoticesMarkdown([comp('test-two-custom', 'bzip2-1.0.6 AND BSD-3-Clause-Clear', files)], []),
      /Component test-two-custom@1\.0\.0 requires standard license text for 'BSD-3-Clause-Clear'/
    );
  }

  PINNED_FALLBACK_ATTRIBUTIONS['slot-pkg@1.0.0'] = {
    author: 'Reviewed BSD Author',
    source: 'https://example.com/slot-pkg',
  };
  try {
    const slotOut = generateNoticesMarkdown([comp('slot-pkg', 'MIT AND BSD-3-Clause', [['LICENSE-MIT', mit]])], []);
    assert.match(slotOut, /No copyright notice is shipped upstream; authors per https:\/\/example\.com\/slot-pkg: Reviewed BSD Author \(slot-pkg\)/);
    const bsdSection = slotOut.slice(slotOut.indexOf('### Standard License: BSD-3-Clause'));
    const bsdText = bsdSection.slice(bsdSection.indexOf('```text'));
    assert.doesNotMatch(bsdText.slice(0, bsdText.indexOf('\n```\n')), /`LICENSE-MIT`/);
  } finally {
    delete PINNED_FALLBACK_ATTRIBUTIONS['slot-pkg@1.0.0'];
  }
});

test('markdown formatting: uses extended code fences when notice content contains embedded backticks', () => {
  const dummyComponent = {
    name: 'nested-backticks-crate',
    version: '1.0.0',
    license: 'MIT',
    source: 'cargo',
    licenseFiles: [
      {
        filename: 'LICENSE',
        content: 'Notice with embedded ``` code block inside text ``` here.',
      },
    ],
    noticeFiles: [],
    authors: ['Fence Author'],
  };

  const md = generateNoticesMarkdown([dummyComponent], []);
  assert.match(md, /````text\r?\nNotice with embedded ``` code block/);
  assert.match(md, /### Notice Entry 1/);
});

test('groupLicenseTexts: whitespace-only license files trigger standard license fallback', () => {
  const emptyLicenseComponent = {
    name: 'empty-license-crate',
    version: '1.0.0',
    license: 'MIT',
    source: 'cargo',
    licenseFiles: [{ filename: 'LICENSE', content: '   \n  \t  ' }],
    noticeFiles: [],
  };

  const { componentsWithoutFiles } = groupLicenseTexts([emptyLicenseComponent]);
  assert.strictEqual(
    componentsWithoutFiles.length,
    1,
    'Whitespace-only license file must be classified as missing dedicated license'
  );
  assert.strictEqual(componentsWithoutFiles[0].name, 'empty-license-crate');
});

test('readPackageFiles: propagates error when reading vendored files fails', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'vendored-error-fixture-'));
  try {
    const vendorDir = path.join(tmpDir, 'vendor');
    fs.mkdirSync(vendorDir, { recursive: true });
    const secretDir = path.join(vendorDir, 'secret');
    fs.mkdirSync(secretDir, { recursive: true });
    fs.writeFileSync(path.join(secretDir, 'LICENSE'), 'content');
    fs.chmodSync(secretDir, 0o000);

    // If environment bypasses access restrictions (Windows, DAC-bypass, or root), skip chmodSync check
    try {
      fs.readdirSync(secretDir);
      return;
    } catch {}

    assert.throws(
      () => readPackageFiles(tmpDir, 'failing-vendor-crate@1.0.0'),
      /Failed to read vendored files in failing-vendor-crate@1\.0\.0/
    );
  } finally {
    try {
      const secretDir = path.join(tmpDir, 'vendor', 'secret');
      fs.chmodSync(secretDir, 0o777);
    } catch {}
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('readPackageFiles: a vendored library keeps its license at any depth, tests and source files named like one are skipped', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'vendored-depth-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE-MIT', 'MIT License\n\nCopyright (c) 2024 Wrapper Author');
    write('libfoo/COPYING', 'Copyright (c) 2004 Foo Project\nRedistribution and use ...');
    write('go/vendor/golang.org/x/crypto/LICENSE', 'Copyright 2009 The Go Authors.');
    write('src/unicode_tables/LICENSE-UNICODE', 'UNICODE LICENSE V3');
    write('libfoo/tests/LICENSE', 'license of a test fixture');
    write('.github/workflows/license.yml', 'name: license');
    write('src/copying.rs', '// Copying helpers');
    write('libfoo/empty/LICENSE', '   ');
    const { licenseFiles, noticeFiles } = readPackageFiles(tmpDir, 'vendoring-crate@1.0.0');
    assert.deepStrictEqual(
      [...licenseFiles, ...noticeFiles].map((f) => f.filename).sort(),
      ['LICENSE-MIT', 'go/vendor/golang.org/x/crypto/LICENSE', 'libfoo/COPYING', 'src/unicode_tables/LICENSE-UNICODE']
    );
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('readPackageFiles: files in a nested REUSE license folder are vendored license texts, a source folder named license is not', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'vendored-licenses-dir-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE-APACHE', 'Apache License\nVersion 2.0, January 2004');
    write('vendor/lib/LICENSES/MIT.txt', 'MIT License\n\nCopyright (c) 2019 Vendored Lib Authors\n\nPermission is hereby granted.');
    write('vendor/lib/LICENSES/BSD-3-Clause.txt', 'Copyright (c) 2019 Vendored Lib Authors\nRedistribution and use in source and binary forms');
    write('vendor/lib/LICENSES/README.md', 'The license texts of this folder.');
    write('vendor/lib/src/x.c', '/* Copyright (c) 2019 Vendored Lib Authors */');
    write('src/license/mod.rs', '// license checks');
    write('src/license/README', 'license module');
    const { licenseFiles, noticeFiles } = readPackageFiles(tmpDir, 'reuse-vendoring@1.0.0');
    const vendored = licenseFiles.filter((f) => f.vendored).map((f) => f.filename).sort();
    assert.deepStrictEqual(vendored, ['vendor/lib/LICENSES/BSD-3-Clause.txt', 'vendor/lib/LICENSES/MIT.txt']);
    assert.deepStrictEqual(licenseFiles.filter((f) => !f.vendored).map((f) => f.filename), ['LICENSE-APACHE']);
    assert.deepStrictEqual(noticeFiles, []);
    const comp = { name: 'reuse-vendoring', version: '1.0.0', license: 'Apache-2.0', source: 'cargo', packageDir: tmpDir, authors: [], licenseFiles, noticeFiles };
    // The vendored code is the folder that holds LICENSES/, not LICENSES/ itself.
    assert.deepStrictEqual(vendoredFolders(comp), ['vendor/lib']);
    // The MIT text is vendored: it does not cover the package's own license.
    const onlyVendored = { ...comp, license: 'MIT', licenseFiles: licenseFiles.filter((f) => f.vendored) };
    const { groupedTexts, componentsWithoutFiles } = groupLicenseTexts([onlyVendored]);
    assert.deepStrictEqual(componentsWithoutFiles.map((c) => c.name), ['reuse-vendoring']);
    const mit = groupedTexts.flatMap((g) => g.components).find((c) => c.filename === 'vendor/lib/LICENSES/MIT.txt');
    assert.ok(mit && mit.vendored, 'the nested MIT text is listed as vendored');
    // A header whose text the vendored code's LICENSES/ ships is covered by it;
    // the same identifier outside that code has no text and fails.
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    // A REUSE file that only points upstream carries no FSFULLR terms, so it
    // covers nothing and the header fails; the file with the terms whole is
    // the control that covers it.
    write('vendor/lib/LICENSES/FSFULLR.txt', 'See https://www.gnu.org/prep/maintain/html_node/License-Notices-for-Other-Files.html for the terms of this file.');
    write('vendor/lib/src/y.c', '/* SPDX-License-Identifier: FSFULLR */\n');
    const pointing = { ...comp, ...readPackageFiles(tmpDir, 'reuse-vendoring@1.0.0') };
    assert.throws(() => findBundledCodeUnderOtherLicenses(pointing, known), /bundles vendor\/lib\/src\/y\.c under 'FSFULLR', but no standard text exists for 'FSFULLR'/);
    write('vendor/lib/LICENSES/FSFULLR.txt', '# Copyright Free Software Foundation, Inc.\n#\n# This file is free software; the Free Software Foundation gives\n# unlimited permission to copy and/or distribute it, with or without\n# modifications, as long as this notice is preserved.\n');
    const shipped = { ...comp, ...readPackageFiles(tmpDir, 'reuse-vendoring@1.0.0') };
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(shipped, known), []);
    write('other/z.c', '/* SPDX-License-Identifier: FSFULLR */\n');
    assert.throws(() => findBundledCodeUnderOtherLicenses(shipped, known), /bundles other\/z\.c under 'FSFULLR', but no standard text exists for 'FSFULLR'/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('an anchor leads to license text: an SPDX document or a notice file is no license text', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'spdx-document-fixture-'));
  try {
    const spdxOnly = path.join(tmpDir, 'spdx-only');
    fs.mkdirSync(spdxOnly);
    fs.writeFileSync(
      path.join(spdxOnly, 'LICENSE.spdx'),
      'SPDXVersion: SPDX-2.1\nPackageName: spdx-only\nPackageSupplier: Organization: The Example Programme\nPackageLicenseDeclared: MIT\nPackageCopyrightText: 2019-2022, The Example Programme\n'
    );
    const withNotice = path.join(tmpDir, 'with-notice');
    fs.mkdirSync(withNotice);
    fs.writeFileSync(path.join(withNotice, 'COPYRIGHT'), 'Copyright 2020 Notice Holder. See LICENSE-MIT.');
    fs.writeFileSync(path.join(withNotice, 'LICENSE-MIT'), shippedLicense('MIT License\n\nCopyright (c) 2020 Notice Holder', 'MIT'));
    const component = (name, dir) => ({
      name,
      version: '1.0.0',
      license: 'MIT',
      source: 'npm',
      packageDir: dir,
      authors: [],
      ...readPackageFiles(dir, `${name}@1.0.0`),
    });
    const spdx = component('@test/spdx-only', spdxOnly);
    assert.deepStrictEqual(spdx.licenseFiles, [], 'an SPDX document carries no license text');
    assert.deepStrictEqual(spdx.noticeFiles.map((f) => f.filename), ['LICENSE.spdx']);

    const md = generateNoticesMarkdown([spdx, component('with-notice', withNotice)], []);
    const after = (id) => md.slice(md.indexOf(`<a id="${id}"></a>`)).split('\n').find((l) => l.startsWith('### '));
    assert.strictEqual(after('notice-@test/spdx-only'), '### Standard License: MIT');
    assert.match(
      md,
      /- `@test\/spdx-only@1\.0\.0` - SPDX: `MIT` \(authors: The Example Programme\)\n {2}- Copyright 2019-2022, The Example Programme \(LICENSE\.spdx\)\n/
    );
    const entry = after('notice-with-notice');
    const body = md.slice(md.indexOf(entry));
    assert.match(body.slice(0, body.indexOf('```text\n') + 40), /```text\nMIT License/, `${entry} must hold the license text`);
    assert.strictEqual((md.match(/<a id="notice-with-notice"><\/a>/g) || []).length, 1);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code under a license the package does not declare: its header license and copyright are reproduced', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bundled-code-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', shippedLicense('ISC License\n\nCopyright (c) 2015 Wrapper Author', 'ISC'));
    write('lib/index.js', '/**\n * @license QR Code generator library\n * Copyright (c) Project Nayuki.\n * SPDX-License-Identifier: MIT\n */\nvar a;\n/**\n * Copyright (c) Wrapper Author\n * SPDX-License-Identifier: ISC\n */\n');
    write('lib/either.js', '// SPDX-License-Identifier: ISC OR MIT\n');
    write('libbar/COPYING', 'Copyright (c) 2004 Bar Project\nRedistribution and use in source and binary forms.');
    write('libbar/src/bar.c', '/* Copyright (c) 2004 Bar Project\n * SPDX-License-Identifier: BSD-3-Clause */\n');
    write('tests/fixture.js', '// SPDX-License-Identifier: BSD-3-Clause\n');
    const comp = {
      name: 'bundling-pkg',
      version: '1.0.0',
      license: 'ISC',
      source: 'npm',
      packageDir: tmpDir,
      authors: ['Wrapper Author'],
      ...readPackageFiles(tmpDir, 'bundling-pkg@1.0.0'),
    };
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    // A license file in the same folder does not hide a header: which license
    // libbar/COPYING grants is not known.
    // A header under the declared ISC (alone or as a choice) is covered by the
    // package's text and keeps its holders as declared.
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(comp, known), [
      { license: 'ISC', notices: ['Copyright (c) Wrapper Author'], files: ['lib/either.js', 'lib/index.js'], headers: [], declared: true },
      { license: 'BSD-3-Clause', notices: ['Copyright (c) 2004 Bar Project'], files: ['libbar/src/bar.c'], headers: [] },
      { license: 'MIT', notices: ['Copyright (c) Project Nayuki.'], files: ['lib/index.js'], headers: [] },
    ]);
    const md = generateNoticesMarkdown([comp], []);
    const section = md.slice(md.indexOf('### Bundled Code: MIT'));
    assert.match(section, /^### Bundled Code: MIT\n\n- `bundling-pkg@1\.0\.0` \(npm, SPDX: `ISC`\) bundles code under MIT in `lib\/index\.js`\n {2}- Copyright \(c\) Project Nayuki\.\n\n```text\nPermission is hereby granted/);
    // A standard text with a holder placeholder takes the header's copyright.
    const bsd = md.slice(md.indexOf('### Bundled Code: BSD-3-Clause'), md.indexOf('### Bundled Code: MIT'));
    assert.match(bsd, /```text\nCopyright \(c\) 2004 Bar Project \(bundling-pkg\)\n/);
    assert.doesNotMatch(bsd, /<copyright holders>|<year>/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('vendored license files: they keep their text but neither cover the package license nor take its anchor', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'vendored-coverage-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('only-vendored/src/thirdparty/COPYING', 'Copyright (c) 2004 Bar Project\nRedistribution and use in source and binary forms.');
    write('only-vendored/src/thirdparty/third.c', '/* Copyright (c) 2010 Third Party */\n');
    write('only-vendored/LICENSE.spdx', 'SPDXVersion: SPDX-2.1\nPackageSupplier: Organization: Own Supplier\nPackageCopyrightText: <text>2019 First Holder\n2020 Second Holder</text>\n');
    // "Abc/" sorts before "LICENSE.spdx": a vendored document must not name the package's supplier.
    write('only-vendored/Abc/LICENSE.spdx', 'SPDXVersion: SPDX-2.1\nPackageSupplier: Organization: Vendored Supplier\nPackageCopyrightText: 2001 Vendored Holder\n');
    write('own-and-vendored/LICENSE-MIT', shippedLicense('MIT License\n\nCopyright (c) 2020 Own Holder', 'MIT'));
    // "Abc/" sorts before "LICENSE-MIT", so its group comes first.
    write('own-and-vendored/Abc/COPYING', 'Copyright (c) 2004 Vendored Holder\nRedistribution and use in source and binary forms, vendored.');
    write('holder-free-own/LICENSE', shippedLicense('', 'MIT'));
    write('holder-free-own/Abc/COPYING', 'Copyright (c) 2004 Vendored Holder\nRedistribution and use in source and binary forms, again.');
    write('holder-free-own/src/lib.rs', '// Copyright 2024 Own Source Holder\n');
    const component = (name) => ({
      name,
      version: '1.0.0',
      license: 'MIT',
      source: 'cargo',
      packageDir: path.join(tmpDir, name),
      authors: ['Wrapper Author'],
      ...readPackageFiles(path.join(tmpDir, name), `${name}@1.0.0`),
    });
    const onlyVendored = { ...component('only-vendored'), authors: [] };
    // A nested LICENSE.spdx is found, and is a notice.
    assert.deepStrictEqual(onlyVendored.noticeFiles.map((f) => [f.filename, Boolean(f.vendored)]), [['Abc/LICENSE.spdx', true], ['LICENSE.spdx', false]]);
    assert.deepStrictEqual(onlyVendored.licenseFiles.map((f) => [f.filename, f.vendored]), [['src/thirdparty/COPYING', true]]);
    const own = component('own-and-vendored');
    const holderFree = component('holder-free-own');
    const { componentsWithoutFiles } = groupLicenseTexts([onlyVendored, own, holderFree]);
    assert.deepStrictEqual(componentsWithoutFiles.map((c) => c.name), ['only-vendored']);

    const md = generateNoticesMarkdown([onlyVendored, own, holderFree], []);
    // The holder of vendored code is not the package's holder: the package's
    // own sources are still searched for its notice.
    assert.match(md, /- `holder-free-own@1\.0\.0` \(cargo, file: `LICENSE`\)\n {2}- Copyright 2024 Own Source Holder \(src\/lib\.rs\)/);
    // The package keeps the standard text of its own license.
    assert.match(md, /### Standard License: MIT\n\n[^\n]*\n- `only-vendored@1\.0\.0` - SPDX: `MIT` \(authors: Own Supplier\)\n/);
    // A multi-line SPDX copyright field is read whole, without its delimiters.
    assert.match(md, /- Copyright 2019 First Holder 2020 Second Holder \(LICENSE\.spdx\)/);
    // The notices of vendored code are listed with its license, not with the package's.
    assert.match(md, /- `only-vendored@1\.0\.0` \(cargo, file: `src\/thirdparty\/COPYING`\)\n {2}- Copyright \(c\) 2010 Third Party \(src\/thirdparty\/third\.c\)\n/);
    const standardEntry = md.slice(md.indexOf('- `only-vendored@1.0.0` - SPDX'));
    assert.doesNotMatch(standardEntry.slice(0, standardEntry.indexOf('\n- ', 3)), /Third Party/);
    // The vendored text is kept.
    assert.match(md, /- `only-vendored@1\.0\.0` \(cargo, file: `src\/thirdparty\/COPYING`\)/);
    // The anchor goes to the package's own license text when it has one.
    const after = (id) => md.slice(md.indexOf(`<a id="${id}"></a>`)).split('\n```text\n')[1].split('\n')[0];
    assert.strictEqual(after('notice-own-and-vendored'), 'MIT License');
    // A package whose only license text is vendored is anchored at the standard
    // text of its own license, not at the vendored one (whisper-rs-sys).
    assert.strictEqual(after('notice-only-vendored'), 'Permission is hereby granted, free of charge, to any');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('standard texts: the fallback is described as the license terms, with the attribution listed separately', () => {
  const comp = { name: 'bare-mit', version: '1.0.0', license: 'MIT', source: 'npm', licenseFiles: [], noticeFiles: [], authors: ['Bare Author'] };
  const md = generateNoticesMarkdown([comp], []);
  const section = md.slice(md.indexOf('## Standard License Texts for Dependencies that Need Them'));
  assert.match(section, /the standard terms of each applicable license are provided below:\nthey are the license terms, not a copy of any component's license file, and the attribution of each component is listed separately with it\./);
  assert.doesNotMatch(md, /verbatim text/);
  // The attribution goes with the component, the terms carry none of their own.
  const mit = section.slice(section.indexOf('### Standard License: MIT'));
  assert.ok(mit.indexOf('`bare-mit@1.0.0`') !== -1 && mit.indexOf('Bare Author') < mit.indexOf('Permission is hereby granted'), mit.slice(0, 400));
  assert.doesNotMatch(mit.slice(mit.indexOf('Permission is hereby granted')), /Copyright \(c\) <year>/);
});

test('standard texts: one render reads the SPDX table once, for the header classification, the standard texts and the bundled code', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'spdx-once-fixture-'));
  const original = fs.readFileSync;
  try {
    fs.mkdirSync(path.join(tmpDir, 'src'), { recursive: true });
    fs.writeFileSync(path.join(tmpDir, 'src', 'x.c'), '/* Copyright (c) 2001 Alpha Vendor\n * SPDX-License-Identifier: BSD-3-Clause */\n');
    const comp = { name: 'once-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Once Author'], ...readPackageFiles(tmpDir, 'once-pkg@1.0.0') };
    let reads = 0;
    fs.readFileSync = (file, ...rest) => {
      if (path.resolve(String(file)) === path.resolve(SPDX_STANDARD_LICENSES_FILE)) reads += 1;
      return original(file, ...rest);
    };
    syncBuiltinESMExports();
    const md = generateNoticesMarkdown([comp], []);
    assert.match(md, /## Standard License Texts for Dependencies that Need Them/);
    assert.match(md, /## Bundled Code Under Other Licenses/);
    assert.strictEqual(reads, 1);
  } finally {
    fs.readFileSync = original;
    syncBuiltinESMExports();
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('anchors: a licensing exception links to its vendored text, the package anchor to its own license', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'exception-anchor-fixture-'));
  try {
    fs.mkdirSync(path.join(tmpDir, 'vendor', 'rar'), { recursive: true });
    fs.writeFileSync(path.join(tmpDir, 'vendor', 'rar', 'license.txt'), 'Vendored RAR license terms.\nUse it, never re-create the algorithm.');
    const comp = { name: 'vend_rar', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Rar Wrapper'], ...readPackageFiles(tmpDir, 'vend_rar@1.0.0') };
    assert.deepStrictEqual(comp.licenseFiles.map((f) => [f.filename, f.vendored]), [['vendor/rar/license.txt', true]]);
    const exception = { crateName: 'vend_rar', version: '1.0.0', author: 'Rar Author', licenseName: 'RAR', sourceFile: 'vendor/rar/license.txt', document: 'docs/RAR-LICENSE.md', facts: 'It is not free software.' };
    const md = generateNoticesMarkdown([comp], [], SPDX_STANDARD_LICENSES_FILE, [exception]);
    // Follow each link to the first text block after its anchor.
    const textAfter = (id) => md.slice(md.indexOf(`<a id="${id}"></a>`)).split('\n```text\n')[1].split('\n')[0];
    assert.match(md, /see \[Notice Entry \d+\]\(#exception-vend-rar\)/);
    assert.strictEqual(textAfter('exception-vend-rar'), 'Vendored RAR license terms.');
    assert.strictEqual(textAfter('notice-vend-rar'), 'Permission is hereby granted, free of charge, to any');
    // Another version of the crate with another text: the exception still names the registered version's text, under one anchor.
    const oldDir = path.join(tmpDir, 'old');
    fs.mkdirSync(path.join(oldDir, 'vendor', 'rar'), { recursive: true });
    fs.writeFileSync(path.join(oldDir, 'vendor', 'rar', 'license.txt'), 'Older RAR license terms.\nUse it.');
    const old = { name: 'vend_rar', version: '0.9.0', license: 'MIT', source: 'cargo', packageDir: oldDir, authors: ['Rar Wrapper'], ...readPackageFiles(oldDir, 'vend_rar@0.9.0') };
    const both = generateNoticesMarkdown([old, comp], [], SPDX_STANDARD_LICENSES_FILE, [exception]);
    assert.strictEqual(both.split('<a id="exception-vend-rar"></a>').length - 1, 1);
    assert.strictEqual(both.slice(both.indexOf('<a id="exception-vend-rar"></a>')).split('\n```text\n')[1].split('\n')[0], 'Vendored RAR license terms.');
    const linked = Number(both.match(/see \[Notice Entry (\d+)\]\(#exception-vend-rar\)/)[1]);
    const entry = both.split(`### Notice Entry ${linked}\n`)[1].split('### Notice Entry ')[0];
    assert.match(entry, /Vendored RAR license terms\./);
    assert.doesNotMatch(entry, /Older RAR license terms/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('anchors: a file that only names the licenses takes no anchor, which goes to a full license text', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'pointer-anchor-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    const standard = JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'));
    // siphasher: its only license file, COPYING, names MIT and Apache-2.0 and
    // holds neither text, so the anchor goes to the standard texts.
    write('pointer/COPYING', 'Copyright 2016-2026 Pointer Author.\n\nLicensed under the MIT license or the Apache License, Version 2.0, at your option.');
    // typenum: LICENSE only names the choice, LICENSE-APACHE holds the text.
    write('split/LICENSE', 'MIT OR Apache-2.0');
    write('split/LICENSE-APACHE', standard['Apache-2.0']);
    const pointer = { name: 'pointer_pkg', version: '1.0.0', license: 'MIT/Apache-2.0', source: 'cargo', packageDir: path.join(tmpDir, 'pointer'), authors: ['Pointer Author'], ...readPackageFiles(path.join(tmpDir, 'pointer'), 'pointer_pkg@1.0.0') };
    const split = { name: 'split-pkg', version: '1.0.0', license: 'MIT OR Apache-2.0', source: 'cargo', packageDir: path.join(tmpDir, 'split'), authors: ['Split Author'], ...readPackageFiles(path.join(tmpDir, 'split'), 'split-pkg@1.0.0') };
    // A file with only the opening of the MIT grant is reproduced, and the
    // anchor goes to the whole text beside it.
    write('stub/COPYRIGHT', 'Copyright 2020 Stub Author.\n\nPermission is hereby granted,\nfree of charge, to any person.');
    write('stub/LICENSE-MIT', `Copyright 2020 Stub Author.\n\n${standard.MIT}`);
    const stubFiles = readPackageFiles(path.join(tmpDir, 'stub'), 'stub-pkg@1.0.0');
    const stub = { name: 'stub-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: path.join(tmpDir, 'stub'), authors: ['Stub Author'], licenseFiles: [...stubFiles.licenseFiles, ...stubFiles.noticeFiles], noticeFiles: [] };
    const md = generateNoticesMarkdown([pointer, split, stub], []);
    assert.ok(md.includes('Permission is hereby granted,\nfree of charge, to any person.'), 'the stub is reproduced');
    assert.strictEqual(md.split('<a id="notice-stub-pkg"></a>').length - 1, 1);
    const textAfter = (id) => md.slice(md.indexOf(`<a id="${id}"></a>`)).split('\n```text\n')[1].split('\n')[0];
    assert.strictEqual(md.split('<a id="notice-pointer-pkg"></a>').length - 1, 1);
    assert.match(md.slice(md.indexOf('<a id="notice-pointer-pkg"></a>')), /^<a id="notice-pointer-pkg"><\/a>\n\n### Standard License: /);
    assert.notStrictEqual(textAfter('notice-pointer-pkg'), 'Copyright 2016-2026 Pointer Author.');
    assert.strictEqual(textAfter('notice-split-pkg'), standard['Apache-2.0'].split('\n')[0]);
    assert.strictEqual(textAfter('notice-stub-pkg'), 'Copyright 2020 Stub Author.');
    assert.ok(md.slice(md.indexOf('<a id="notice-stub-pkg"></a>')).split('\n```\n')[0].includes(standard.MIT.split('\n')[0]), 'the anchor leads to the whole MIT text');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code headers: the copyright may follow the identifier or be an SPDX field, and a NOTICE or license file does not hide it', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bundled-header-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', 'ISC License\n\nCopyright (c) 2015 Wrapper Author');
    write('src/NOTICE', 'This product includes software from the Vendor project.');
    write('src/index.js', '/*\n * SPDX-License-Identifier: MIT\n * Copyright (c) 2024 Vendor\n */\n/*\n * Copyright (c) 2023 Next Header\n * SPDX-License-Identifier: ISC\n */\n');
    write('src/two.js', '// Copyright (c) 2022 Slash Vendor\n// SPDX-License-Identifier: MIT\n// Copyright (c) 2023 Slash Vendor Two\nconst x = 1;\n// Copyright (c) 2021 Not In Header\n');
    write('covered/LICENSE', 'Copyright (c) 2004 Covered Project\nPermission granted.');
    write('covered/c.js', '/* Copyright (c) 2004 Covered Project\n * SPDX-License-Identifier: MIT */\n');
    write('spdx/s.js', '// SPDX-FileCopyrightText: 2024 Field Vendor\n// SPDX-License-Identifier: MIT\n');
    const comp = {
      name: 'header-pkg',
      version: '1.0.0',
      license: 'ISC',
      source: 'npm',
      packageDir: tmpDir,
      authors: ['Wrapper Author'],
      ...readPackageFiles(tmpDir, 'header-pkg@1.0.0'),
    };
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(comp, known), [
      // The next header of src/index.js is under the declared ISC: its holder
      // stays with it, never with the MIT block before it.
      { license: 'ISC', notices: ['Copyright (c) 2023 Next Header'], files: ['src/index.js'], headers: [], declared: true },
      {
        license: 'MIT',
        notices: [
          'Copyright (c) 2004 Covered Project',
          'Copyright 2024 Field Vendor',
          'Copyright (c) 2024 Vendor',
          'Copyright (c) 2022 Slash Vendor',
          'Copyright (c) 2023 Slash Vendor Two',
        ],
        files: ['covered/c.js', 'spdx/s.js', 'src/index.js', 'src/two.js'],
        headers: [],
      },
    ]);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code headers: own permission text, an exception, and holder lines only', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bundled-terms-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted.');
    // An ISC-style header tagged BSD-2-Clause, its holder on the first line and
    // the tag more than ten lines below.
    const isc = [
      'Copyright (c) 2013 Header Holder <holder@example.org>',
      '',
      'Adapted by Someone Else for this project',
      '',
      'Permission to use, copy, modify, and distribute this software for any',
      'purpose with or without fee is hereby granted, provided that the above',
      'copyright notice and this permission notice appear in all copies.',
      '',
      'THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES',
      'WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF',
      'MERCHANTABILITY AND FITNESS.',
      '',
      'SPDX-License-Identifier: BSD-2-Clause',
    ];
    write('vendor/chacha.c', `/*\n${isc.map((l) => (l ? ` * ${l}` : ' *')).join('\n')}\n */\nint x;\n`);
    // A header whose terms are the standard BSD-2-Clause text is not repeated.
    const standard = JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'));
    const std = standard['BSD-2-Clause'].replace(/^Copyright \(c\) <year> <copyright holders>/, 'Copyright (c) 2010 Standard Holder');
    write('vendor/std.c', `/*\n${std.split('\n').map((l) => (l ? ` * ${l}` : ' *')).join('\n')}\n * SPDX-License-Identifier: BSD-2-Clause\n */\n`);
    write('ggml/backend.hpp', '//\n// Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.\n// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception\n//\n');
    const comp = {
      name: 'terms-pkg',
      version: '1.0.0',
      license: 'MIT',
      source: 'cargo',
      packageDir: tmpDir,
      authors: ['Wrapper Author'],
      ...readPackageFiles(tmpDir, 'terms-pkg@1.0.0'),
    };
    const found = findBundledCodeUnderOtherLicenses(comp, new Set(Object.keys(standard)));
    assert.deepStrictEqual(
      found.map((b) => [b.license, b.notices, b.files, b.headers.map((h) => h.files)]),
      [
        ['Apache-2.0 WITH LLVM-exception', [], ['ggml/backend.hpp'], []],
        ['BSD-2-Clause', ['Copyright (c) 2013 Header Holder <holder@example.org>', 'Copyright (c) 2010 Standard Holder'], ['vendor/chacha.c', 'vendor/std.c'], [['vendor/chacha.c'], ['vendor/std.c']]],
      ]
    );
    assert.strictEqual(found[1].headers[0].text, isc.join('\n'));
    assert.strictEqual(licenseTermsOf(found[1].headers[1].text), licenseTermsOf(standard['BSD-2-Clause']));

    const md = generateNoticesMarkdown([comp], []);
    const apache = md.slice(md.indexOf('### Bundled Code: Apache-2.0 WITH LLVM-exception'), md.indexOf('### Bundled Code: BSD-2-Clause'));
    assert.match(apache, /\n {2}- No copyright line in the header that declares this license\n/);
    assert.ok(apache.includes(standard['Apache-2.0']) && apache.includes(standard['LLVM-exception']), 'the license text and then the exception text');
    assert.ok(apache.indexOf(standard['Apache-2.0']) < apache.indexOf(standard['LLVM-exception']));
    const bsd = md.slice(md.indexOf('### Bundled Code: BSD-2-Clause'));
    assert.ok(bsd.includes('```text\n' + isc.join('\n') + '\n```'), 'the ISC-style header is reproduced as shipped');
    assert.strictEqual((bsd.match(/Header with its own terms/g) || []).length, 1, 'a header with the standard terms is not repeated');
    assert.doesNotMatch(bsd, /^ {2}- copyright notice and this permission notice/m, 'license prose is no notice');
    assert.match(bsd, /```text\nCopyright \(c\) 2013 Header Holder <holder@example\.org> \(terms-pkg\)\nCopyright \(c\) 2010 Standard Holder \(terms-pkg\)\n/);

    // An exception with no standard text fails instead of dropping it.
    write('ggml/other.hpp', '// SPDX-License-Identifier: Apache-2.0 WITH Unknown-exception\n');
    // An exception the policy does not know fails before its text is looked
    // for; a known license with no text fails on the text (FSFULLR outside the
    // folder that ships it, in the REUSE vendoring test).
    assert.throws(() => generateNoticesMarkdown([comp], []), /bundles ggml\/other\.hpp under 'Apache-2\.0 WITH Unknown-exception', and no choice of it is compatible with GPL-3\.0/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a header license with no standard text fails, a declared or reproducible choice still passes', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bundled-missing-text-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', 'ISC License\n\nCopyright (c) 2015 Wrapper Author\n\nPermission to use, copy, modify.');
    write('lib/covered.js', '// SPDX-License-Identifier: ISC OR Foo-1.0\n');
    write('lib/choice.js', '// Copyright (c) 2016 Choice Vendor\n// SPDX-License-Identifier: Foo-1.0 OR MIT\n');
    const comp = { name: 'missing-text-pkg', version: '1.0.0', license: 'ISC', source: 'npm', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'missing-text-pkg@1.0.0') };
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    // A declared choice covers the header; otherwise the first reproducible choice is taken.
    assert.deepStrictEqual(
      findBundledCodeUnderOtherLicenses(comp, known).map((b) => [b.license, b.files, Boolean(b.declared)]),
      [['ISC', ['lib/covered.js'], true], ['MIT', ['lib/choice.js'], false]]
    );
    // BSD-2-Clause is a license, but without its text the notices cannot reproduce it.
    write('lib/bsd.c', '/* Copyright (c) 2017 Bsd Vendor\n * SPDX-License-Identifier: BSD-2-Clause */\n');
    const withoutBsd = new Set([...known].filter((id) => id !== 'BSD-2-Clause'));
    assert.throws(
      () => findBundledCodeUnderOtherLicenses(comp, withoutBsd),
      /missing-text-pkg@1\.0\.0 bundles lib\/bsd\.c under 'BSD-2-Clause', but no standard text exists for 'BSD-2-Clause'/
    );
    // A choice with text still passes when the other has none.
    fs.writeFileSync(path.join(tmpDir, 'lib/bsd.c'), '/* SPDX-License-Identifier: BSD-2-Clause OR MIT */\n');
    assert.deepStrictEqual(
      findBundledCodeUnderOtherLicenses(comp, withoutBsd).map((b) => [b.license, b.files]),
      [['ISC', ['lib/covered.js']], ['MIT', ['lib/bsd.c', 'lib/choice.js']]]
    );
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('comment closers: a line comment keeps a closer in its text, a block comment still ends at its closer', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'comment-closer-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted.');
    const lineTerms = 'Redistribution and use of the files under src/*/generated are permitted provided that this notice is kept.';
    write('gen/line.rs', `// Copyright (c) 2019 Line Vendor\n// ${lineTerms}\n// SPDX-License-Identifier: BSD-2-Clause\nfn x() {}\n`);
    const blockTerms = 'Redistribution and use in source and binary forms are permitted provided that this notice is kept.';
    write('gen/block.c', `/* Copyright (c) 2018 Block Vendor\n * ${blockTerms}\n * SPDX-License-Identifier: BSD-2-Clause */ int y;\n`);
    // The package scan reads a line comment to its end too.
    write('src/n.rs', '// Copyright (c) 2017 Path Holder, for src/*/gen\n');
    // A preprocessor line is code, not a line comment: its block comment still ends at the closer.
    write('src/d.h', '#define X 1 /* Copyright (c) 2016 Define Holder */\n');
    const comp = { name: 'closer-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'closer-pkg@1.0.0') };
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    const texts = findBundledCodeUnderOtherLicenses(comp, known).flatMap((b) => b.headers.map((h) => h.text));
    assert.ok(texts.includes(`Copyright (c) 2019 Line Vendor\n${lineTerms}\nSPDX-License-Identifier: BSD-2-Clause`), 'the // header keeps src/*/generated');
    assert.ok(texts.includes(`Copyright (c) 2018 Block Vendor\n${blockTerms}\nSPDX-License-Identifier: BSD-2-Clause`), 'the C block ends at its closer, without the code after it');
    const scanned = findPackageCopyrightNotices(tmpDir).map((n) => n.notice);
    assert.ok(scanned.includes('Copyright (c) 2017 Path Holder, for src/*/gen'), 'the // notice keeps src/*/gen');
    assert.ok(scanned.includes('Copyright (c) 2016 Define Holder'), 'the #define block comment ends at its closer');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('comment closers: inside a block comment a closer ends it whatever leads the line, in every block style and both scanners', () => {
  const pairs = [['/*', '*/'], ['(*', '*)'], ['{-', '-}'], ['<!--', '-->']];
  const leaders = ['//', '#', '--', ';', '%'];
  const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
  for (const [open, close] of pairs) {
    for (const leader of leaders) {
      const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'inner-closer-fixture-'));
      try {
        fs.writeFileSync(path.join(tmpDir, 'LICENSE'), 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted.');
        fs.mkdirSync(path.join(tmpDir, 'gen'));
        fs.writeFileSync(
          path.join(tmpDir, 'gen', 'inner.src'),
          `${open}\n${leader} Copyright (c) 2021 Inner Holder ${close} code();\n${open}\n${leader} SPDX-License-Identifier: BSD-2-Clause ${close} int x;\n`
        );
        const comp = { name: 'inner-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'inner-pkg@1.0.0') };
        const bundled = findBundledCodeUnderOtherLicenses(comp, known);
        const label = `${open} ... ${leader} ... ${close}`;
        assert.deepEqual(bundled.map((b) => b.license), ['BSD-2-Clause'], `${label}: the tag ends at the closer, the code after it is no part of it`);
        const scanned = findPackageCopyrightNotices(tmpDir).map((n) => n.notice);
        assert.ok(scanned.includes('Copyright (c) 2021 Inner Holder'), `${label}: the holder ends at the closer: ${JSON.stringify(scanned)}`);
        assert.ok(!scanned.some((n) => n.includes('code()')), `${label}: no code after the closer in a holder`);
      } finally {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      }
    }
  }
  // Inside a block comment only its own closer ends it: `*/` is text inside
  // `(* *)`, `*)` is text inside `/* */`.
  for (const [open, close, other] of [['(*', '*)', '*/'], ['/*', '*/', '*)'], ['{-', '-}', '*/'], ['<!--', '-->', '*)']]) {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'other-closer-fixture-'));
    try {
      fs.writeFileSync(path.join(tmp, 'LICENSE'), 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted.');
      fs.mkdirSync(path.join(tmp, 'gen'));
      fs.writeFileSync(path.join(tmp, 'gen', 'other.src'), `${open}\n Copyright (c) 2021 Inner Holder, for src/${other}generated\n ${close} code();\n${open}\n SPDX-License-Identifier: BSD-2-Clause ${close} int x;\n`);
      const comp = { name: 'other-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmp, authors: ['Wrapper Author'], ...readPackageFiles(tmp, 'other-pkg@1.0.0') };
      const label = `${open} ... ${other} ... ${close}`;
      assert.ok(findPackageCopyrightNotices(tmp).map((n) => n.notice).includes(`Copyright (c) 2021 Inner Holder, for src/${other}generated`), label);
      assert.deepEqual(findBundledCodeUnderOtherLicenses(comp, known).map((b) => b.license), ['BSD-2-Clause'], label);
      // The header reproduced from one block keeps the other style's closer
      // in its clause, whole.
      const clause = `Redistribution and use of the files under src/${other}generated are permitted provided that this notice is kept.`;
      fs.writeFileSync(path.join(tmp, 'gen', 'other.src'), `${open}\n Copyright (c) 2021 Block Holder\n ${clause}\n SPDX-License-Identifier: BSD-2-Clause\n ${close}\nint x;\n`);
      const headers = findBundledCodeUnderOtherLicenses({ ...comp, ...readPackageFiles(tmp, 'other-pkg@1.0.0') }, known).flatMap((b) => b.headers.map((h) => h.text));
      assert.deepEqual(headers, [`Copyright (c) 2021 Block Holder\n${clause}\nSPDX-License-Identifier: BSD-2-Clause`], label);
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  }
  // A real line comment keeps a closer in its text: a closer at its end is
  // dropped from the tag, and one in its words stays.
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'line-closer-fixture-'));
  try {
    fs.writeFileSync(path.join(tmpDir, 'LICENSE'), 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted.');
    fs.writeFileSync(path.join(tmpDir, 'line.rs'), '// Copyright (c) 2019 Line Vendor, for src/*/gen\n// SPDX-License-Identifier: BSD-2-Clause */\nfn x() {}\n');
    const comp = { name: 'line-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'line-pkg@1.0.0') };
    assert.deepEqual(findBundledCodeUnderOtherLicenses(comp, known).map((b) => b.license), ['BSD-2-Clause']);
    assert.ok(findPackageCopyrightNotices(tmpDir).map((n) => n.notice).includes('Copyright (c) 2019 Line Vendor, for src/*/gen'));
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('source notices: a package whose license names a holder still lists the other notices of its sources, each once, another year included', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'extra-holder-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    const mit = 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted,\nfree of charge, to any person.';
    write('LICENSE', mit);
    // A second, untagged holder in the sources, in two files: listed once.
    write('src/lib.rs', '// Copyright (c) 2021 Contributor Two\nfn a() {}\n');
    write('src/two.rs', '// Copyright (c) 2021 Contributor Two\nfn b() {}\n');
    // The holder the license names with other years is another notice: listed.
    write('src/own.rs', '// Copyright (c) 2022 Wrapper Author.\n');
    write('README.md', '(c) 2015 - Present - The Wrapper Author\n');
    // The very notice the license carries, in another spelling: not listed again.
    write('src/same.rs', '// COPYRIGHT © 2020  Wrapper Author.\n');
    // Source headers under the declared license: the license's holder with
    // other years is another notice, the license's own notice is not.
    write('src/hdr.c', '/*\n * Copyright (c) 2024 Wrapper Author\n * SPDX-License-Identifier: MIT\n */\nint h;\n');
    write('src/hdr2.c', '/*\n * Copyright (c) 2020 Wrapper Author\n * SPDX-License-Identifier: MIT\n */\nint g;\n');
    const comp = { name: 'extra-holder', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'extra-holder@1.0.0') };
    const md = generateNoticesMarkdown([comp], []);
    const entry = md.slice(md.indexOf('`extra-holder@1.0.0`'), md.indexOf('```', md.indexOf('`extra-holder@1.0.0`')));
    assert.deepStrictEqual(entry.match(/^ {2}- .*$/gm), [
      '  - Copyright (c) 2024 Wrapper Author (in a source header under MIT)',
      '  - (c) 2015 - Present - The Wrapper Author (README.md)',
      '  - Copyright (c) 2021 Contributor Two (src/lib.rs)',
      '  - Copyright (c) 2022 Wrapper Author. (src/own.rs)',
    ]);
    assert.strictEqual(md.split('Copyright (c) 2020 Wrapper Author').length - 1, 1, 'the license text, with its holder, is reproduced once');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('standard texts: a slot holds the standard words or a holder the file names, never a condition', () => {
  const std = JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))['BSD-3-Clause'];
  const shipped = (slot) => std.replace('<year> <copyright holders>', '2020 Example Corp').replace('the copyright holder nor', `${slot} nor`);
  assert.ok(carriesWholeStandardText('BSD-3-Clause', std, shipped('the copyright holder')));
  assert.ok(carriesWholeStandardText('BSD-3-Clause', std, shipped('Example Corp')), 'the holder its copyright line names');
  assert.ok(!carriesWholeStandardText('BSD-3-Clause', std, shipped('Example Corp when used alone')), 'a condition in the slot changes the terms');
  assert.ok(!carriesWholeStandardText('BSD-3-Clause', std, shipped('Other Corp')), 'a name no copyright line of the file gives');
  // The markers the notice scanner takes: a "©" or "(c)" with a year names the holder too.
  for (const marker of ['© 2020 Example Corp', '(c) 2020 Example Corp']) {
    const symbol = shipped('Example Corp').replace('Copyright (c) 2020 Example Corp', marker);
    assert.ok(carriesWholeStandardText('BSD-3-Clause', std, symbol), marker);
    assert.ok(!carriesWholeStandardText('BSD-3-Clause', std, symbol.replace('Example Corp nor', 'Example Corp when used alone nor')), `${marker}: still no condition`);
  }
});

test('standard texts: a slot name is found in a long attribution, whose names are kept no longer than a slot', { timeout: 30000 }, () => {
  const std = JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))['BSD-3-Clause'];
  // 3000 words of attribution: every candidate of every length would be
  // about 4.5 million names of 1000 words on average.
  const filler = Array.from({ length: 3000 }, (_, i) => `w${i}`).join(' ');
  const shipped = std.replace('<year> <copyright holders>', `2020 ${filler} Example Corp`).replace('the copyright holder nor', 'Example Corp nor');
  assert.ok(carriesWholeStandardText('BSD-3-Clause', std, shipped));
});

test('cargo inventory: a package is active by its id, so a test-only fork with the name and version of a registry crate is not', () => {
  const registry = { id: 'registry+https://github.com/rust-lang/crates.io-index#twin@1.0.0', name: 'twin', version: '1.0.0', source: 'registry+https://github.com/rust-lang/crates.io-index', manifest_path: '/r/twin-1.0.0/Cargo.toml' };
  const fork = { id: 'git+https://github.com/x/twin?rev=r#abcdef0123456789', name: 'twin', version: '1.0.0', source: 'git+https://github.com/x/twin?rev=r#abcdef0123456789', manifest_path: '/g/twin/Cargo.toml' };
  const local = { id: 'path+file:///w/peer#helper@0.1.0', name: 'helper', version: '0.1.0', source: null, manifest_path: '/w/peer/Cargo.toml' };
  const pkgs = [registry, fork, local];
  // Only the registry crate in the production tree: the fork, a dev dependency, stays out.
  assert.deepStrictEqual([...activeCargoPackageIds('aeroftp v4.2.7 (/w)\ntwin v1.0.0\nhelper v0.1.0 (/w/peer)\ntwin v1.0.0 (*)\n', pkgs)].sort(), [local.id, registry.id].sort());
  // Both in the production tree: both are listed.
  assert.deepStrictEqual([...activeCargoPackageIds('twin v1.0.0\ntwin v1.0.0 (https://github.com/x/twin?rev=r#abcdef01)\n', pkgs)].sort(), [fork.id, registry.id].sort());
  // A line no package matches, or a source of another kind, fails the run.
  assert.throws(() => activeCargoPackageIds('twin v2.0.0\n', pkgs), /names 0 packages/);
  assert.throws(() => activeCargoPackageIds('twin v1.0.0 (https://github.com/y/twin#abcdef01)\n', pkgs), /names 0 packages/);
  // A path source with parentheses of its own, which cargo does not escape,
  // also with the marks cargo appends after it.
  const paren = { id: 'path+file:///w/AeroFTP (test)/peer#paren-helper@0.1.0', name: 'paren-helper', version: '0.1.0', source: null, manifest_path: '/w/AeroFTP (test)/peer/Cargo.toml' };
  for (const line of ['paren-helper v0.1.0 (/w/AeroFTP (test)/peer)', 'paren-helper v0.1.0 (/w/AeroFTP (test)/peer) (*)', 'paren-helper v0.1.0 (/w/AeroFTP (test)/peer) (proc-macro)']) {
    assert.deepStrictEqual([...activeCargoPackageIds(`${line}\n`, [...pkgs, paren])], [paren.id], line);
  }
  // A package row in a form not read fails, never drops the package.
  assert.throws(() => activeCargoPackageIds('twin v1.0.0 [something else]\n', pkgs), /twin v1\.0\.0 in a form this generator does not read/);
  // The error names the package and version, never the source: a Git URL
  // can carry credentials.
  const secret = { ...fork, id: 'git+https://user:s3cret-token@github.com/x/twin?rev=r#abcdef0123456789', source: 'git+https://user:s3cret-token@github.com/x/twin?rev=r#abcdef0123456789' };
  assert.throws(
    () => activeCargoPackageIds('twin v1.0.0 (https://user:s3cret-token@github.com/x/twin?rev=r#ffffffff)\n', [registry, secret]),
    (e) => /twin v1\.0\.0, which names 0 packages/.test(e.message) && !e.message.includes('s3cret-token')
  );
});

test('source notices: two installed packages with one name and version keep their own holders', () => {
  const dirs = [fs.mkdtempSync(path.join(os.tmpdir(), 'same-name-a-')), fs.mkdtempSync(path.join(os.tmpdir(), 'same-name-b-'))];
  try {
    const comps = ['Alpha', 'Beta'].map((who, i) => {
      fs.writeFileSync(path.join(dirs[i], 'LICENSE'), `MIT License\n\nCopyright (c) 2020 ${who} Wrapper\n\nPermission is hereby granted,\nfree of charge, to any person.`);
      fs.mkdirSync(path.join(dirs[i], 'src'));
      fs.writeFileSync(path.join(dirs[i], 'src/lib.rs'), `// Copyright (c) 2021 ${who} Contributor\nfn a() {}\n`);
      // The registry crate and its git fork: same name and version, two package ids.
      return { name: 'twin', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: dirs[i], instance: `cargo:twin 1.0.0 (source ${i})`, authors: [`${who} Wrapper`], ...readPackageFiles(dirs[i], 'twin@1.0.0') };
    });
    const md = generateNoticesMarkdown(comps, []);
    for (const who of ['Alpha', 'Beta']) {
      const at = md.indexOf(`Copyright (c) 2020 ${who} Wrapper`);
      const entry = md.slice(md.lastIndexOf('### Notice Entry', at), at);
      assert.deepStrictEqual(entry.match(/^ {2}- .*$/gm), [`  - Copyright (c) 2021 ${who} Contributor (src/lib.rs)`], entry);
    }
  } finally {
    for (const d of dirs) fs.rmSync(d, { recursive: true, force: true });
  }
});

test('source notices: a notice read from a string literal, a source map, a README or a man page is the notice alone', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'notice-shape-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('src/html.rs', 'const S: &str = "// Copyright 2019-2024 Literal Holder\\r// SPDX-License-Identifier: MIT\\r\\nwindow.x = () => {}";\n');
    write('lib/a.js.map', '{"sourcesContent":["/**\\n * Copyright (c) 2017 Map Holder. All rights reserved.\\n * @license MIT\\n */"]}\n');
    write('README.md', 'Copyright (c) 2014-2017, Link Holder ([www.example.com](https://www.example.com/home)) (MIT License)<br>\nCopyright (c) 2014 Entity Holder &lt;holder@example.com&gt;\n');
    write('man/tool.1', 'Copyright (c) 2011\\-2014, Roff Holder (MIT License)\\.\n');
    write('src/mail.rs', '// Copyright (c) 2016 Mail Holder <mail@example.com>\n');
    assert.deepStrictEqual(findPackageCopyrightNotices(tmpDir).map((n) => n.notice), [
      'Copyright (c) 2011-2014, Roff Holder',
      'Copyright (c) 2014 Entity Holder <holder@example.com>',
      'Copyright (c) 2014-2017, Link Holder (www.example.com)',
      'Copyright (c) 2016 Mail Holder <mail@example.com>',
      'Copyright (c) 2017 Map Holder. All rights reserved.',
      'Copyright 2019-2024 Literal Holder',
    ]);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: an unstarred block comment is read whole, its holder and its own terms are reproduced', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bundled-unstarred-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted.');
    const terms = 'Redistribution and use in source and binary forms, with or without modification, are permitted provided that this notice is kept and the Unstarred clause is honoured.';
    write('vendor/unstarred.c', `int before;\n/* Copyright (c) 2001 Adjacent Holder */\n/*\nCopyright (c) 2019 Unstarred Vendor\n\n${terms}\n\nSPDX-License-Identifier: BSD-2-Clause\n*/\nint x;\n/* Copyright (c) 2002 After Holder */\n`);
    const comp = { name: 'unstarred-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'unstarred-pkg@1.0.0') };
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    const found = findBundledCodeUnderOtherLicenses(comp, known);
    assert.deepStrictEqual(found.map((b) => [b.license, b.notices, b.files]), [['BSD-2-Clause', ['Copyright (c) 2019 Unstarred Vendor'], ['vendor/unstarred.c']]]);
    assert.strictEqual(found[0].headers[0].text, `Copyright (c) 2019 Unstarred Vendor\n\n${terms}\n\nSPDX-License-Identifier: BSD-2-Clause`);
    const md = generateNoticesMarkdown([comp], []);
    const bsd = md.slice(md.indexOf('### Bundled Code: BSD-2-Clause'));
    assert.match(bsd, /\n {2}- Copyright \(c\) 2019 Unstarred Vendor\n/);
    assert.ok(bsd.includes(terms), 'the shipped permission text is reproduced');
    assert.doesNotMatch(bsd, /Adjacent Holder|After Holder/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a header under the declared license keeps its holder beside the package, without a second text', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bundled-declared-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    // The package ships the whole MIT text, so it covers the header: a stub
    // would get the standard text added and hide a second copy.
    write('LICENSE', shippedLicense('MIT License\n\nCopyright (c) 2020 Wrapper Author', 'MIT'));
    write('lib/vendor.js', '/*\n * Copyright (c) 2018 Vendor Author\n * SPDX-License-Identifier: MIT\n */\n');
    const comp = { name: 'declared-pkg', version: '1.0.0', license: 'MIT', source: 'npm', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'declared-pkg@1.0.0') };
    const md = generateNoticesMarkdown([comp], []);
    assert.ok(md.includes('Copyright (c) 2020 Wrapper Author'));
    assert.match(md, /\n {2}- Copyright \(c\) 2018 Vendor Author \(in a source header under MIT\)\n/);
    assert.doesNotMatch(md, /### Bundled Code: MIT/);
    assert.doesNotMatch(md, /### Standard License: MIT/);
    assert.strictEqual(md.split('Permission is hereby granted, free of charge, to any\nperson obtaining a copy').length - 1, 1, 'the MIT grant appears once');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a declared term counts as covered only when the notices carry its text, a choice that is carried is taken', () => {
  const tmpDirs = [];
  const fixture = (license, files) => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bundled-carried-fixture-'));
    tmpDirs.push(tmpDir);
    for (const [rel, content] of Object.entries(files)) {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    }
    return { name: `carried-${tmpDirs.length}`, version: '1.0.0', license, source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, `carried-${tmpDirs.length}@1.0.0`) };
  };
  const headers = {
    'src/apache.rs': '// Copyright 2019 Apache Header Author\n// SPDX-License-Identifier: Apache-2.0\nfn a() {}\n',
    'src/mit.rs': '// Copyright 2019 Mit Header Author\n// SPDX-License-Identifier: MIT\nfn b() {}\n',
    'src/either.rs': '// Copyright 2019 Either Header Author\n// SPDX-License-Identifier: Apache-2.0 OR MIT\nfn c() {}\n',
    'src/both.rs': '// Copyright 2019 Both Header Author\n// SPDX-License-Identifier: MIT AND Apache-2.0\nfn d() {}\n',
  };
  const apacheClause = 'Grant of Copyright License';
  const mitClause = 'Permission is hereby granted, free of charge';
  const count = (md, s) => md.split(s).length - 1;
  try {
    // MIT OR Apache-2.0 with LICENSE-MIT alone: the notices carry no Apache
    // text, so an Apache-2.0 header (alone or in a conjunct) is bundled code
    // with the standard text; a header offering MIT takes the carried MIT.
    const orMit = fixture('MIT OR Apache-2.0', { 'LICENSE-MIT': shippedLicense('MIT License\n\nCopyright (c) 2020 Wrapper Author', 'MIT'), ...headers });
    let md = generateNoticesMarkdown([orMit], []);
    // The package declares Apache-2.0 as a choice, so the section must not
    // say the license is one it does not declare: it is one whose text the
    // notices do not already carry for it.
    const intro = md.slice(md.indexOf('## Bundled Code Under Other Licenses')).split('\n')[2];
    assert.match(intro, /whose text the notices above do not already carry for them: a license they do not declare, or one they declare without shipping its text\./);
    assert.doesNotMatch(intro, /under a license they do not declare themselves/);
    const apache = md.slice(md.indexOf('### Bundled Code: Apache-2.0'));
    assert.match(apache, /^### Bundled Code: Apache-2\.0\n\n- `carried-1@1\.0\.0` \(cargo, SPDX: `MIT OR Apache-2\.0`\) bundles code under Apache-2\.0 in `src\/apache\.rs`, `src\/both\.rs`\n {2}- Copyright 2019 Apache Header Author\n {2}- Copyright 2019 Both Header Author\n/);
    assert.strictEqual(count(md, apacheClause), 1, 'the Apache text appears once, with the bundled code');
    assert.strictEqual(count(md, mitClause), 1, 'the MIT text appears once, from LICENSE-MIT');
    assert.match(md, /\n {2}- Copyright 2019 Mit Header Author \(in a source header under MIT\)\n/);
    assert.match(md, /\n {2}- Copyright 2019 Either Header Author \(in a source header under MIT\)\n/);
    assert.doesNotMatch(md, /### Bundled Code: MIT/);
    assert.doesNotMatch(md, /Either Header Author[^\n]*Apache|Mit Header Author[^\n]*Apache/);
    // The same package shipping both texts: every header is declared, no
    // bundled section and no second text.
    const orBoth = fixture('MIT OR Apache-2.0', { 'LICENSE-MIT': shippedLicense('MIT License\n\nCopyright (c) 2020 Wrapper Author', 'MIT'), 'LICENSE-APACHE': STANDARD_TEXTS['Apache-2.0'], ...headers });
    md = generateNoticesMarkdown([orBoth], []);
    assert.doesNotMatch(md, /### Bundled Code:/);
    assert.strictEqual(count(md, apacheClause), 1);
    assert.match(md, /\n {2}- Copyright 2019 Apache Header Author \(in a source header under Apache-2\.0\)\n/);
    // MIT AND Apache-2.0 with LICENSE-MIT alone: the Apache text is added for
    // the package itself, so the Apache headers are covered by it, once.
    const andMit = fixture('MIT AND Apache-2.0', { 'LICENSE-MIT': shippedLicense('MIT License\n\nCopyright (c) 2020 Wrapper Author', 'MIT'), ...headers });
    md = generateNoticesMarkdown([andMit], []);
    assert.doesNotMatch(md, /### Bundled Code:/);
    assert.strictEqual(count(md, apacheClause), 1, 'the package standard text covers the Apache headers');
    assert.match(md, /\n {2}- Copyright 2019 Apache Header Author \(in a source header under Apache-2\.0\)\n/);
    // No own license file: every choice whose text exists is reproduced
    // (MIT OR Apache-2.0 gives both), so both terms are carried.
    const noFiles = fixture('MIT OR Apache-2.0', headers);
    md = generateNoticesMarkdown([noFiles], []);
    assert.doesNotMatch(md, /### Bundled Code:/);
    assert.strictEqual(count(md, apacheClause), 1);
    assert.strictEqual(count(md, mitClause), 1);
  } finally {
    for (const d of tmpDirs) fs.rmSync(d, { recursive: true, force: true });
  }
});

test('standard-license fallback: a holder is inserted literally, a "$&" or "$1" in its name is not expanded', () => {
  // BSD-3-Clause: its standard text has the holder placeholder line.
  const comp = { name: 'test-literal-holder', version: '1.0.0', license: 'BSD-3-Clause', source: 'cargo', licenseFiles: [], noticeFiles: [], authors: ['Research $& Development $1'] };
  const reviewed = { ...PINNED_FALLBACK_ATTRIBUTIONS, 'test-literal-holder@1.0.0': { author: 'Research $& Development $1', source: 'test fixture' } };
  const md = generateNoticesMarkdown([comp], [], SPDX_STANDARD_LICENSES_FILE, KNOWN_NON_FREE_EXCEPTIONS, undefined, reviewed);
  assert.match(md, /\nNo copyright notice is shipped upstream; authors per test fixture: Research \$& Development \$1 \(test-literal-holder\)\n/, 'the holder appears as written in the license block');
  // Control: a plain holder is inserted the same way.
  const reviewedPlain = { ...PINNED_FALLBACK_ATTRIBUTIONS, 'test-literal-holder@1.0.0': { author: 'Plain Holder', source: 'test fixture' } };
  const plain = generateNoticesMarkdown([{ ...comp, authors: ['Plain Holder'] }], [], SPDX_STANDARD_LICENSES_FILE, KNOWN_NON_FREE_EXCEPTIONS, undefined, reviewedPlain);
  assert.match(plain, /\nNo copyright notice is shipped upstream; authors per test fixture: Plain Holder \(test-literal-holder\)\n/);
});

test('standard-license fallback: choices come from the parsed expression, a conjunct is never dropped, an exception keeps its text', () => {
  const fallback = (name, license) => ({ name, version: '1.0.0', license, source: 'cargo', licenseFiles: [], noticeFiles: [], authors: ['Fallback Author'] });
  // A tab-separated AND binds both choices: BSD-3-Clause-Clear has no text, so this fails.
  assert.throws(
    () => generateNoticesMarkdown([fallback('test-clear', '(MIT OR Apache-2.0)\tAND\tBSD-3-Clause-Clear')], []),
    /test-clear@1\.0\.0 requires standard license text for 'BSD-3-Clause-Clear'/
  );
  // An OR with one reproducible choice stays valid through it.
  const sspl = generateNoticesMarkdown([fallback('test-sspl', 'MIT OR SSPL-1.0')], []);
  assert.match(sspl, /### Standard License: MIT\n/);
  assert.doesNotMatch(sspl, /### Standard License: SSPL/);
  // The exception stays with its license: the license text, then the exception's.
  const standard = JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'));
  const llvm = generateNoticesMarkdown([fallback('test-llvm', 'Apache-2.0 WITH LLVM-exception')], []);
  const section = llvm.slice(llvm.indexOf('### Standard License: Apache-2.0 WITH LLVM-exception'));
  assert.ok(section.length > 0 && llvm.includes('### Standard License: Apache-2.0 WITH LLVM-exception'));
  assert.ok(section.includes(standard['Apache-2.0']) && section.indexOf(standard['Apache-2.0']) < section.indexOf(standard['LLVM-exception']));
  assert.throws(
    () => generateNoticesMarkdown([fallback('test-unknown-exc', 'Apache-2.0 WITH Unknown-exception')], []),
    /test-unknown-exc@1\.0\.0 requires standard license text for 'Unknown-exception'/
  );
});

test('scope: the introduction claims only the Cargo and npm dependencies it lists, and names the system libraries it does not cover', () => {
  const md = generateNoticesMarkdown([], []);
  const intro = md.slice(0, md.indexOf('\n---\n'));
  assert.match(intro, /license texts\nfor the Rust \(Cargo\) and Node\.js \(npm\) production dependencies that AeroFTP distribution packages include:/);
  assert.match(intro, /System libraries that are not Cargo or npm dependencies \(for example WebKitGTK, which the AppImage bundles as a\nfallback\) are not covered by this document\. The AppImage ships the copyright file of the Ubuntu package of each library\nit bundles in `usr\/share\/doc\/<package>\/copyright`, and `usr\/share\/doc\/BUNDLED-LIBRARIES\.txt` names that package and its version,\nthe Ubuntu source package and version it was built from, and a link to that source on Launchpad\. For at least three years\nfrom the date of each release, anyone who asks can obtain from us the complete corresponding source code of each of those\nlibraries, at the version listed, for a charge no more than our cost of providing it: ask at dev@aeroftp\.app, the contact\npublished in \[SECURITY\.md\]\(SECURITY\.md\)\. Every version listed is a package of the Ubuntu archive, which keeps its source: we fetch\nthat exact version from the Launchpad page listed, or from the Ubuntu archive snapshot of the release's build date at\nsnapshot\.ubuntu\.com when Launchpad no longer serves it, and send it to you\./);
  // The offer states what the release process provides, nothing it cannot.
  assert.doesNotMatch(intro, /convenience only|whether or not a link still works/);
  // The contact of the offer is the one SECURITY.md publishes, not a new address.
  assert.match(fs.readFileSync(path.join(ROOT, 'SECURITY.md'), 'utf8'), /security contact \*\*dev@aeroftp\.app\*\*/);
  assert.doesNotMatch(md, /all production dependencies|All bundled components|Total Bundled Components/);
  assert.match(md, /- \*\*Total Listed Components:\*\* 0/);
});

test('wrapped notices: a notice that may go on with its next line is joined only from a reviewed entry, never by a guess', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'wrapped-notice-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    const scanOnly = (rel, content) => {
      fs.rmSync(path.join(tmpDir, 'lib'), { recursive: true, force: true });
      write(rel, content);
      return () => findPackageCopyrightNotices(tmpDir).map((n) => `${n.notice} (${n.file})`);
    };
    // The xz getopt header of lzma-sys, reviewed at its source: joined.
    assert.ok(reviewedNotices().some((r) => r.line === 'Copyright (C) 1989-1994,1996-1999,2001,2003,2004' && r.following.length === 1 && r.following[0] === 'Free Software Foundation, Inc.'));
    assert.deepStrictEqual(scanOnly('lib/getopt_int.h', '/* Internal declarations for getopt.\n   Copyright (C) 1989-1994,1996-1999,2001,2003,2004\n   Free Software Foundation, Inc.\n   This file is part of the GNU C Library.\n*/\n')(), [
      'Copyright (C) 1989-1994,1996-1999,2001,2003,2004 Free Software Foundation, Inc. (lib/getopt_int.h)',
    ]);
    // The same shape unreviewed fails and names the file, whatever the next
    // line holds: a holder ("The Example Foundation"), license prose, code
    // after a closed comment, the rest of an open parenthesis or a list.
    for (const [rel, content] of [
      ['lib/other.c', '/* Copyright (C) 1987,2006\n   Free Software Foundation, Inc.\n*/\n'],
      ['lib/the.c', '/* Copyright (C) 2020\n * The Example Foundation\n */\n'],
      ['lib/prose.c', '/* Copyright (C) 1999\n   This program is free software.\n*/\n'],
      ['lib/closed.c', '/* Copyright (c) 2020 */\nWidget widget;\n'],
      ['lib/paren.rs', '// Copyright 2021 Example Consortium, (First University, Second\n// Institute). All Rights Reserved.\n'],
      ['lib/list.rs', '// Copyright 2018 First Author <first@example.org>,\n// Second Author <second@example.org>\n'],
    ]) {
      const scan = scanOnly(rel, content);
      assert.throws(scan, new RegExp(`${rel.replace('.', '\\.')}:1 may continue on its next line, and the generation does not guess`), rel);
    }
    // A reviewed notice that continues on more than one line is used only
    // while every one of those lines is unchanged upstream: the clap_derive
    // header joins its three lines, and the same header with only its third
    // line changed fails instead of giving the stale reviewed holders.
    const clap = ['Copyright 2018 Guillaume Pinot (@TeXitoi) <texitoi@texitoi.eu>,', 'Kevin Knapp (@kbknapp) <kbknapp@gmail.com>, and'];
    const clapReviewed = reviewedNotices().find((r) => r.line === clap[0]);
    assert.deepStrictEqual(clapReviewed?.following, [clap[1], 'Ana Hobden (@hoverbear) <operator@hoverbear.org>']);
    assert.deepStrictEqual(scanOnly('lib/clap.rs', `// ${clap[0]}\n// ${clap[1]}\n// Ana Hobden (@hoverbear) <operator@hoverbear.org>\n//\n// Licensed under MIT\n`)(), [`${clapReviewed.notice} (lib/clap.rs)`]);
    assert.throws(scanOnly('lib/clap.rs', `// ${clap[0]}\n// ${clap[1]}\n// Another Author <another@example.org>\n//\n// Licensed under MIT\n`), /lib\/clap\.rs:1 may continue on its next line, and the generation does not guess/);
    assert.throws(scanOnly('lib/clap.rs', `// ${clap[0]}\n// ${clap[1]}\n`), /lib\/clap\.rs:1 may continue on its next line/);
    // A notice with its holder on the line takes nothing from the next one.
    assert.deepStrictEqual(scanOnly('lib/named.c', '/* Copyright (C) 2001 Named Holder\n   Free Software Foundation, Inc.\n*/\n')(), ['Copyright (C) 2001 Named Holder (lib/named.c)']);
    // A bundled header follows the same rule.
    fs.rmSync(path.join(tmpDir, 'lib'), { recursive: true, force: true });
    write('LICENSE', 'Permission to use, copy, modify, and/or distribute this software for any purpose is hereby granted.');
    write('vendor/getopt.c', '/* Copyright (C) 1989-1994,1996-1999,2001,2003,2004\n   Free Software Foundation, Inc.\n   SPDX-License-Identifier: BSD-2-Clause */\n');
    const comp = () => ({ name: 'wrapped-pkg', version: '1.0.0', license: 'ISC', source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'wrapped-pkg@1.0.0') });
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(comp(), known).find((b) => b.license === 'BSD-2-Clause')?.notices, ['Copyright (C) 1989-1994,1996-1999,2001,2003,2004 Free Software Foundation, Inc.']);
    write('vendor/getopt.c', '/* Copyright (C) 1987,2006\n   Free Software Foundation, Inc.\n   SPDX-License-Identifier: BSD-2-Clause */\n');
    assert.throws(() => findBundledCodeUnderOtherLicenses(comp(), known), /wrapped-pkg@1\.0\.0 vendor\/getopt\.c:1 may continue on its next line/);
    // The header scanner checks every reviewed following line too.
    fs.rmSync(path.join(tmpDir, 'vendor'), { recursive: true, force: true });
    write('vendor/clap.rs', `// ${clap[0]}\n// ${clap[1]}\n// Ana Hobden (@hoverbear) <operator@hoverbear.org>\n// SPDX-License-Identifier: BSD-2-Clause\n`);
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(comp(), known).find((b) => b.license === 'BSD-2-Clause')?.notices, [clapReviewed.notice]);
    write('vendor/clap.rs', `// ${clap[0]}\n// ${clap[1]}\n// Another Author <another@example.org>\n// SPDX-License-Identifier: BSD-2-Clause\n`);
    assert.throws(() => findBundledCodeUnderOtherLicenses(comp(), known), /wrapped-pkg@1\.0\.0 vendor\/clap\.rs:1 may continue on its next line/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('reviewed notices: a reviewed example value gives no attribution, a reviewed wrapped holder still does', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'reviewed-example-fixture-'));
  try {
    const pkgDir = path.join(tmpDir, 'tauri-winres-0.3.6');
    fs.mkdirSync(path.join(pkgDir, 'src'), { recursive: true });
    fs.writeFileSync(path.join(pkgDir, 'README.md'), '```toml\nCopyright © 2016\n...\n```\n');
    fs.writeFileSync(path.join(pkgDir, 'src/lib.rs'), '/// ```\n/// Copyright © 2016\n/// ```\nfn a() {}\n');
    assert.deepStrictEqual(findPackageCopyrightNotices(pkgDir), []);
    // The control: a reviewed notice whose holder is on the next line keeps it.
    const wrapped = reviewedNotices().find((r) => !r.example && !r.citation && r.following.length === 1);
    fs.writeFileSync(path.join(pkgDir, 'src/lib.rs'), `// ${wrapped.line}\n// ${wrapped.following[0]}\nfn a() {}\n`);
    assert.deepStrictEqual(findPackageCopyrightNotices(pkgDir).map((n) => n.notice), [wrapped.notice]);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('reviewed notices: a reviewed citation gives no attribution in the file it was reviewed in, the same lines elsewhere are a notice to review', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'reviewed-citation-fixture-'));
  try {
    const citation = reviewedNotices().find((r) => r.citation);
    const wrapped = reviewedNotices().find((r) => !r.example && !r.citation && r.following.length === 1);
    const { package: pkg, version, file } = citation.citation;
    // The moka layout: the module's docs cite a book, line by line, in the
    // package folder cargo unpacks (<name>-<version>).
    const moka = path.join(tmpDir, `${pkg}-${version}`);
    fs.mkdirSync(path.dirname(path.join(moka, file)), { recursive: true });
    const cited = `// This module was written by the developers, referring to the following book:\n// ${citation.line}\n// ${citation.following[0]}\n`;
    fs.writeFileSync(path.join(moka, file), `${cited}\n// ${wrapped.line}\n// ${wrapped.following[0]}\nfn a() {}\n`);
    assert.deepStrictEqual(findPackageCopyrightNotices(moka).map((n) => n.notice), [wrapped.notice]);
    // The same lines in another package, or another file of moka, may
    // credit code copied from the book: not dropped, they stop the run for
    // review, as any notice that may continue on its next line.
    const other = path.join(tmpDir, 'other-1.0.0');
    fs.mkdirSync(path.join(other, 'src'), { recursive: true });
    fs.writeFileSync(path.join(other, 'src/arc.rs'), `${cited}fn a() {}\n`);
    assert.throws(() => findPackageCopyrightNotices(other), /may continue on its next line/);
    fs.writeFileSync(path.join(moka, 'src/other.rs'), `${cited}fn a() {}\n`);
    assert.throws(() => findPackageCopyrightNotices(moka), /src\/other\.rs:2 may continue on its next line/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('package without a holder in its files: a covered header notice is listed once, a header without one keeps the absence line', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'covered-header-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    // A complete MIT text with no holder, and a source header under MIT whose
    // yearless notice the source scan does not read: render with only src/a.c
    // first to pin declared-header recovery, then add an equivalent notice in
    // src/b.c to pin noticeKey deduplication.
    write('LICENSE', shippedLicense('MIT License', 'MIT'));
    write('src/a.c', '/*\n * \u00a9 Alice Example\n * SPDX-License-Identifier: MIT\n */\nint a;\n');
    const comp = () => ({ name: 'covered-header', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['A'], ...readPackageFiles(tmpDir, 'covered-header@1.0.0') });
    const entry = (out) => out.slice(out.indexOf('`covered-header@1.0.0` (cargo'), out.indexOf('```', out.indexOf('`covered-header@1.0.0` (cargo')));
    const recoveredOnly = entry(generateNoticesMarkdown([comp()], []));
    assert.strictEqual(recoveredOnly.split('Alice Example').length - 1, 1, recoveredOnly);
    assert.doesNotMatch(recoveredOnly, /No copyright notice is shipped in the package/);
    write('src/b.c', '/*\n * Copyright (c) Alice Example.\n * SPDX-License-Identifier: MIT\n */\nint b;\n');
    const withNotice = entry(generateNoticesMarkdown([comp()], []));
    assert.strictEqual(withNotice.split('Alice Example').length - 1, 1, withNotice);
    assert.doesNotMatch(withNotice, /No copyright notice is shipped in the package/);
    // The control: a header without a notice gives the absence line.
    write('src/a.c', '/* SPDX-License-Identifier: MIT */\nint a;\n');
    write('src/b.c', 'int b;\n');
    assert.match(entry(generateNoticesMarkdown([comp()], [])), /No copyright notice is shipped in the package/);
    // Without a LICENSE file at all (standard-text fallback), a yearless
    // covered header attribution is still included and deduplicated by noticeKey.
    fs.rmSync(path.join(tmpDir, 'LICENSE'));
    write('src/a.c', '/*\n * \u00a9 Alice Example\n * SPDX-License-Identifier: MIT\n */\nint a;\n');
    write('src/b.c', '/*\n * Copyright (c) Alice Example.\n * SPDX-License-Identifier: MIT\n */\nint b;\n');
    const noLicComp = {
      name: 'test-covered-no-lic',
      version: '1.0.0',
      license: 'MIT',
      source: 'cargo',
      packageDir: tmpDir,
      authors: ['Manifest Author'],
      ...readPackageFiles(tmpDir, 'test-covered-no-lic@1.0.0'),
    };
    const noLicOut = generateNoticesMarkdown([noLicComp], []);
    const stdIdx = noLicOut.indexOf('- `test-covered-no-lic@1.0.0` - SPDX: `MIT`');
    assert.ok(stdIdx !== -1, noLicOut);
    const stdEntry = noLicOut.slice(stdIdx, noLicOut.indexOf('```', stdIdx));
    assert.strictEqual(stdEntry.split('Alice Example').length - 1, 1, stdEntry);
    assert.doesNotMatch(stdEntry, /No copyright notice is shipped/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('component links: credentials and query values never reach the notices, an ordinary link stays as it is', () => {
  assert.strictEqual(publicLink('https://github.com/rust-lang/libc'), 'https://github.com/rust-lang/libc');
  assert.strictEqual(publicLink('https://user:s3cret-token@git.example.com/org/repo'), 'https://git.example.com/org/repo');
  assert.strictEqual(publicLink('https://:@git.example.com/org/repo'), 'https://git.example.com/org/repo');
  assert.strictEqual(publicLink('https://example.com/pkg?access_token=s3cret-token#x'), 'https://example.com/pkg');
  assert.strictEqual(publicLink('javascript:alert(1)'), undefined);
  assert.strictEqual(publicLink('file:///etc/passwd'), undefined);
  assert.strictEqual(publicLink('not a url'), undefined);
  assert.strictEqual(publicLink(undefined), undefined);
  const dep = (name, repository) => ({ name, version: '1.0.0', license: 'MIT', source: 'cargo', repository, authors: ['A'], licenseFiles: [], noticeFiles: [] });
  const { output } = renderNotices([
    dep('linked', 'https://user:s3cret-token@git.example.com/org/repo?token=s3cret-token'),
    dep('plain', 'https://github.com/org/plain'),
    dep('parenthesis', 'https://git.example.com/docs/Foo).html'),
    dep('balanced', 'https://git.example.com/docs/Foo(bar).html'),
  ], []);
  assert.doesNotMatch(output, /s3cret-token/);
  assert.match(output, /\| linked \| 1\.0\.0 \| MIT \| \[Link\]\(https:\/\/git\.example\.com\/org\/repo\) \|/);
  assert.match(output, /\| plain \| 1\.0\.0 \| MIT \| \[Link\]\(https:\/\/github\.com\/org\/plain\) \|/);
  assert.match(output, /\| parenthesis \| 1\.0\.0 \| MIT \| \[Link\]\(https:\/\/git\.example\.com\/docs\/Foo%29\.html\) \|/);
  assert.match(output, /\| balanced \| 1\.0\.0 \| MIT \| \[Link\]\(https:\/\/git\.example\.com\/docs\/Foo%28bar%29\.html\) \|/);
});

test('reviewed notices: a citation applies in a package folder written with Windows separators', () => {
  const c = { package: 'moka', version: '0.12.16', file: 'src/common/concurrent/arc.rs' };
  assert.ok(citationApplies(c, '/home/r/.cargo/registry/src/idx/moka-0.12.16/src/common/concurrent/arc.rs:5'));
  assert.ok(citationApplies(c, 'C:\\Users\\r\\.cargo\\registry\\src\\idx\\moka-0.12.16/src/common/concurrent/arc.rs:5'));
  assert.ok(!citationApplies(c, 'C:\\Users\\r\\.cargo\\registry\\src\\idx\\other-1.0.0/src/common/concurrent/arc.rs:5'));
  assert.ok(!citationApplies(c, 'C:\\Users\\r\\.cargo\\registry\\src\\idx\\moka-0.12.16/src/other.rs:5'));
});

test('cargo inventory: a line that is neither a package row, a heading nor a blank stops the run', () => {
  const pkg = { name: 'twin', version: '1.0.0', id: 'registry+https://github.com/rust-lang/crates.io-index#twin@1.0.0', source: 'registry+https://github.com/rust-lang/crates.io-index' };
  assert.deepStrictEqual([...activeCargoPackageIds('\n[build-dependencies]\ntwin v1.0.0\n', [pkg])], [pkg.id]);
  for (const line of ['twin 1.0.0', 'twin v1.0.0-git (ssh://user:secret@host/x)x', '<twin v1.0.0>']) {
    assert.throws(() => activeCargoPackageIds(`${line}\n`, [pkg]), (e) => /does not read/.test(e.message) && !e.message.includes('secret'), line);
  }
});

test('cargo inventory: the tree is read without color, so CARGO_TERM_COLOR=always in CI does not wrap its marks', () => {
  const at = CARGO_TREE_ARGS.indexOf('--color');
  assert.ok(at > 0 && CARGO_TREE_ARGS[at + 1] === 'never', CARGO_TREE_ARGS.join(' '));
  // The colored form cargo prints for a repeated package is no row the reader takes.
  const pkg = { name: 'proc-macro2', version: '1.0.107', id: 'registry+https://github.com/rust-lang/crates.io-index#proc-macro2@1.0.107', source: 'registry+https://github.com/rust-lang/crates.io-index' };
  assert.throws(() => activeCargoPackageIds('proc-macro2 v1.0.107 \u001b[33m\u001b[2m(*)\u001b[39m\u001b[22m\n', [pkg]), /in a form this generator does not read/);
  assert.deepStrictEqual([...activeCargoPackageIds('proc-macro2 v1.0.107 (*)\n', [pkg])], [pkg.id]);
});

test('reviewed notices and tag mentions: each entry names its source and the whole notice starts with its line', () => {
  for (const r of reviewedNotices()) {
    assert.ok(r.source && r.line && Array.isArray(r.following) && r.following.length > 0 && r.following.every((f) => typeof f === 'string' && f.trim() === f && f), JSON.stringify(r));
    // An example value or a citation gives no notice; any other entry gives
    // the whole one.
    if (r.citation !== undefined) assert.ok(r.notice === '' && r.example === undefined && ['package', 'version', 'file'].every((k) => typeof r.citation[k] === 'string' && r.citation[k]), r.line);
    else if (r.example !== undefined) assert.ok(r.notice === '' && r.citation === undefined && ['package', 'version', 'file'].every((k) => typeof r.example[k] === 'string' && r.example[k]), r.line);
    else {
      assert.ok(r.example === undefined && r.citation === undefined && r.notice, JSON.stringify(r));
      assert.ok(r.notice.startsWith(r.line.replace(/,$/, '')), r.line);
    }
  }
  // The two tauri-winres examples are reviewed as examples scoped to their files, not notices.
  const reviewedExamples = reviewedNotices().filter((r) => r.example).map((r) => r.source.split(' (')[0]);
  for (const required of ['tauri-winres 0.3.6 README.md:86', 'tauri-winres 0.3.6 src/lib.rs:127']) {
    assert.ok(reviewedExamples.includes(required), `missing reviewed example ${required}`);
  }
  // Example suppression applies only in the two reviewed tauri-winres@0.3.6 files,
  // never in an unrelated package carrying the same pair of lines.
  const exTmp = fs.mkdtempSync(path.join(os.tmpdir(), 'example-scope-fixture-'));
  try {
    const tauriDir = path.join(exTmp, 'tauri-winres-0.3.6');
    fs.mkdirSync(path.join(tauriDir, 'src'), { recursive: true });
    fs.writeFileSync(path.join(tauriDir, 'README.md'), 'Copyright © 2016\n...\n');
    fs.writeFileSync(path.join(tauriDir, 'src', 'lib.rs'), '// Copyright © 2016\n// ```\n');
    assert.deepStrictEqual(findPackageCopyrightNotices(tauriDir), []);
    const otherDir = path.join(exTmp, 'unrelated-pkg-1.0.0');
    fs.mkdirSync(otherDir, { recursive: true });
    fs.writeFileSync(path.join(otherDir, 'README.md'), 'Copyright © 2016\n...\n');
    assert.throws(() => findPackageCopyrightNotices(otherDir), /may continue on its next line/);
  } finally {
    fs.rmSync(exTmp, { recursive: true, force: true });
  }
  // The moka book citation is reviewed as a citation, not a notice of moka.
  const reviewedCitations = reviewedNotices().filter((r) => r.citation).map((r) => r.source.split(' (')[0]);
  assert.ok(reviewedCitations.includes('moka 0.12.16 src/common/concurrent/arc.rs:5-6'));
  for (const [key, reason] of Object.entries(reviewedTagMentions())) assert.ok(/^[^@]+@[^:]+:[^:]+:/.test(key) && reason, key);
});

test('third-party license files: reproduced as notices, a package\'s own only when it declares the file, never by its name', () => {
  const tmpDirs = [];
  const bzip2 = `Copyright (C) 1996-2010 Julian R Seward.\n\n${reviewedLicenseTerms()['bzip2-1.0.6']}`;
  const mitText = shippedLicense('Copyright (c) 2020 Third Party Author', 'MIT');
  const vendorMit = shippedLicense('Copyright (c) 2015 Vendored Library Author', 'MIT');
  const fixture = (files, license, declared) => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'third-party-license-fixture-'));
    tmpDirs.push(tmpDir);
    for (const [rel, content] of Object.entries(files)) {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    }
    const id = `third-${tmpDirs.length}`;
    return { name: id, version: '1.0.0', license, source: 'cargo', packageDir: tmpDir, authors: ['Third Party Author'], ...readPackageFiles(tmpDir, `${id}@1.0.0`, declared) };
  };
  const STANDARD = /## Standard License Texts for Dependencies that Need Them/;
  try {
    for (const rel of ['LICENSES/THIRD-PARTY-LICENSE.txt', 'THIRD_PARTY_LICENSE.txt']) {
      // An anthology of the code the package bundles, at the root or in
      // LICENSES/, with the whole text of the package's license: reproduced,
      // and the package still gets the standard text of its own license.
      const anthology = `This package bundles Vendored Library under the MIT License:\n\n${vendorMit}`;
      const vendorOnly = fixture({ [rel]: anthology }, 'MIT');
      assert.deepStrictEqual(vendorOnly.noticeFiles.map((f) => f.filename), [rel], 'shown as a notice');
      const md = generateNoticesMarkdown([vendorOnly], []);
      assert.ok(md.includes('This package bundles Vendored Library under the MIT License'), `${rel}: the anthology is reproduced`);
      assert.match(md, STANDARD, `${rel}: the anthology does not replace the package's standard entry`);
      // A license with no standard text is not covered by an anthology either.
      const withBzip2 = fixture({ 'LICENSE-MIT': mitText, [rel]: bzip2 }, 'MIT AND bzip2-1.0.6');
      assert.throws(() => generateNoticesMarkdown([withBzip2], []), new RegExp(`Component ${withBzip2.name}@1\\.0\\.0 requires standard license text for 'bzip2-1\\.0\\.6'`), rel);
    }
    // The license_file the package declares is its own, whatever its name.
    const declared = fixture({ 'LICENSE-MIT': mitText, 'THIRD_PARTY_LICENSE.txt': bzip2 }, 'MIT AND bzip2-1.0.6', 'THIRD_PARTY_LICENSE.txt');
    const md = generateNoticesMarkdown([declared], []);
    assert.strictEqual(md.split('The name of the author may not be used').length - 1, 1, 'the bzip2 terms appear once, from the declared file');
    assert.doesNotMatch(md, STANDARD);
    const only = fixture({ 'THIRD_PARTY_LICENSE.txt': `${mitText}\n\n${bzip2}` }, 'MIT AND bzip2-1.0.6', 'THIRD_PARTY_LICENSE.txt');
    const onlyMd = generateNoticesMarkdown([only], []);
    assert.ok(onlyMd.indexOf(`<a id="notice-${only.name}"></a>`) >= 0, 'its declared file takes the anchor');
    assert.doesNotMatch(onlyMd, STANDARD);
    // The same declared file inside LICENSES/ is read once, not rediscovered as a notice.
    const nestedDeclared = fixture(
      { 'LICENSE-MIT': mitText, 'LICENSES/THIRD-PARTY-LICENSE.txt': bzip2 },
      'MIT AND bzip2-1.0.6',
      'LICENSES/THIRD-PARTY-LICENSE.txt'
    );
    assert.deepStrictEqual(nestedDeclared.noticeFiles, []);
    assert.deepStrictEqual(nestedDeclared.licenseFiles.map((f) => f.filename), ['LICENSE-MIT', 'LICENSES/THIRD-PARTY-LICENSE.txt']);
    const nestedMd = generateNoticesMarkdown([nestedDeclared], []);
    assert.strictEqual(nestedMd.split('The name of the author may not be used').length - 1, 1);
    // A vendored library's file describes that library, not the package.
    const vendored = fixture({ 'LICENSE-MIT': mitText, 'vendor/lib/THIRD-PARTY-LICENSE.txt': bzip2 }, 'MIT AND bzip2-1.0.6');
    assert.ok(vendored.noticeFiles.every((f) => f.vendored));
    assert.throws(() => generateNoticesMarkdown([vendored], []), new RegExp(`Component ${vendored.name}@1\\.0\\.0 requires standard license text for 'bzip2-1\\.0\\.6'`));
  } finally {
    for (const d of tmpDirs) fs.rmSync(d, { recursive: true, force: true });
  }
});

// The AppImage is built on Ubuntu: the script reads the versions with
// dpkg-query, so the fixture runs where dpkg is (the Checks job), never as a
// silent pass elsewhere.
const HAS_DPKG_QUERY = process.platform === 'linux' && spawnSync('dpkg-query', ['--version'], { stdio: 'ignore' }).status === 0;

test('AppImage: the fixture is never skipped on the Linux CI runner', () => {
  if (process.env.GITHUB_ACTIONS === 'true' && process.platform === 'linux') {
    assert.ok(HAS_DPKG_QUERY, 'dpkg-query is on the Ubuntu runner, so the AppImage fixture runs there');
  }
});

test('AppImage: every bundled library ships the copyright file of the package that installed its file, and one without it fails the build', { skip: !HAS_DPKG_QUERY && 'needs Linux with dpkg-query, as the AppImage build' }, () => {
  const script = path.join(ROOT, 'src-tauri/scripts/bundle-appimage-lib-notices.sh');
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'appimage-notices-fixture-'));
  const write = (rel, content) => {
    fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
    fs.writeFileSync(path.join(tmpDir, rel), content);
  };
  const A = '/usr/lib/x86_64-linux-gnu';
  // A dpkg database: the files each package installed, and its version.
  const packages = {
    'libwebkit2gtk-4.1-0': [`${A}/libwebkit2gtk-4.1.so.0`],
    'libgtk-3-0': [`${A}/libgtk-3.so.0`, `${A}/gtk-3.0/3.0.0/immodules/im-xim.so`],
    // Two packages install a libfoo.so.1: the loader's one in the library
    // folder, and a plugin of the same name in its own folder.
    'libfoo1': [`${A}/libfoo.so.1`],
    'foo-plugins': [`${A}/foo/plugins/libfoo.so.1`],
    // A copy under /opt is no library folder linuxdeploy copies from.
    'some-app': ['/opt/some-app/libgtk-3.so.0'],
  };
  const dpkg = (pkgs) => {
    fs.rmSync(path.join(tmpDir, 'dpkg'), { recursive: true, force: true });
    for (const [pkg, files] of Object.entries(pkgs)) write(`dpkg/info/${pkg}:amd64.list`, `/.\n/usr\n${files.join('\n')}\n`);
    // libgtk-3-0 is built from the gtk+3.0 source package, libfoo1 from a
    // source version with an epoch; the others from a source of their own
    // name and version, which dpkg then leaves out of the status.
    const source = { 'libgtk-3-0': 'Source: gtk+3.0 (3.24.33-1ubuntu2)\n', libfoo1: 'Source: foo (2:1.7-0ubuntu1)\n' };
    write('dpkg/status', Object.keys(pkgs).map((pkg) => `Package: ${pkg}\nStatus: install ok installed\nArchitecture: amd64\n${source[pkg] ?? ''}Version: 1.${pkg.length}-0ubuntu1\n`).join('\n'));
  };
  const ldcache = (names) => write('ldcache', `${names.length} libs found in cache \`/etc/ld.so.cache'\n${names.map((n) => `\t${n} (libc6,x86-64) => /lib/x86_64-linux-gnu/${n}\n`).join('')}`);
  try {
    write('app/usr/lib/fallback/libwebkit2gtk-4.1.so.0', 'elf');
    write('app/usr/lib/fallback/libgtk-3.so.0', 'elf');
    write('app/usr/lib/gtk-3.0/3.0.0/immodules/im-xim.so', 'elf');
    write('app/usr/lib/fallback/libfoo.so.1', 'elf');
    write('app/usr/lib/foo/plugins/libfoo.so.1', 'elf');
    write('app/usr/lib/aeroftp/libown.so', 'ours');
    for (const pkg of Object.keys(packages)) write(`doc/${pkg}/copyright`, `${pkg} terms`);
    dpkg(packages);
    ldcache(['libwebkit2gtk-4.1.so.0', 'libgtk-3.so.0', 'libfoo.so.1']);
    // Launchpad, without the network: the .dsc of a source version names that
    // source and version, unless the fixture says otherwise (dsc/<file> its
    // body, dsc/missing-<file> a 404, dsc/down the number of 502s first).
    write('fetch-dsc.sh', [
      '#!/bin/bash',
      'url="$1"; out="$2"; base="${url##*/}"; dir="$(dirname "$0")/dsc"',
      'echo "$url" >> "$dir/../fetched"',
      'if [ -f "$dir/down" ] && [ "$(cat "$dir/down")" -gt 0 ]; then echo $(( $(cat "$dir/down") - 1 )) > "$dir/down"; echo 502; exit 0; fi',
      'if [ -f "$dir/missing-$base" ]; then echo 404; exit 0; fi',
      'if [ -f "$dir/$base" ]; then cp "$dir/$base" "$out"; echo 200; exit 0; fi',
      'rest="${url#*+sourcefiles/}"; src="${rest%%/*}"; rest="${rest#*/}"; ver="${rest%%/*}"',
      'printf "Format: 3.0 (quilt)\\nSource: %s\\nVersion: %s\\n" "$src" "$ver" > "$out"; echo 200',
      '',
    ].join('\n'));
    fs.chmodSync(path.join(tmpDir, 'fetch-dsc.sh'), 0o755);
    fs.mkdirSync(path.join(tmpDir, 'dsc'), { recursive: true });
    const run = () =>
      execFileSync('bash', [script, path.join(tmpDir, 'app'), '9.8.7'], {
        env: { ...process.env, AEROFTP_DPKG_ADMINDIR: path.join(tmpDir, 'dpkg'), AEROFTP_LDCONFIG_P: path.join(tmpDir, 'ldcache'), AEROFTP_DOC_ROOT: path.join(tmpDir, 'doc'), AEROFTP_DSC_FETCH: path.join(tmpDir, 'fetch-dsc.sh'), AEROFTP_DSC_BACKOFF: '0' },
        stdio: 'pipe',
        encoding: 'utf8',
      });
    const fails = (pattern) => assert.throws(run, (e) => pattern.test(String(e.stderr)) || assert.fail(String(e.stderr)));
    const startedAt = Math.floor(Date.now() / 1000);
    run();
    const endedAt = Math.ceil(Date.now() / 1000);
    const doc = path.join(tmpDir, 'app/usr/share/doc');
    for (const pkg of ['libwebkit2gtk-4.1-0', 'libgtk-3-0', 'libfoo1', 'foo-plugins']) assert.strictEqual(fs.readFileSync(path.join(doc, pkg, 'copyright'), 'utf8'), `${pkg} terms`);
    assert.ok(!fs.existsSync(path.join(doc, 'some-app')));
    const list = fs.readFileSync(path.join(doc, 'BUNDLED-LIBRARIES.txt'), 'utf8');
    // The written offer of the source, valid three years from the release,
    // at the contact SECURITY.md publishes; no asset is named.
    // The archive snapshot is named by the UTC time of the build, to the
    // second, so the exact version stays retrievable when Launchpad no longer
    // serves it, also for a package published earlier that day (the midnight
    // snapshot of the day does not hold it).
    assert.match(
      list,
      /\nWritten offer: for at least three years from the date of the AeroFTP 9\.8\.7 release, anyone who asks can obtain from us the complete corresponding source code of each library listed here, at the source package version listed, for a charge no more than our cost of providing it\. Ask at dev@aeroftp\.app, the contact published in SECURITY\.md of https:\/\/github\.com\/axpdev-lab\/aeroftp\. Every version listed is a package of the Ubuntu archive, and the archive keeps its source: we fetch that exact version from the Launchpad page listed, or from the archive snapshot of the build date at https:\/\/snapshot\.ubuntu\.com\/ubuntu\/20[0-9]{2}(0[1-9]|1[0-2])(0[1-9]|[12][0-9]|3[01])T([01][0-9]|2[0-3])[0-5][0-9][0-5][0-9]Z\/ when Launchpad no longer serves it, and send it to you\.\n/,
    );
    const [, y, mo, d, h, mi, se] = list.match(/snapshot\.ubuntu\.com\/ubuntu\/(\d{4})(\d{2})(\d{2})T(\d{2})(\d{2})(\d{2})Z\//);
    const stamp = Date.UTC(+y, +mo - 1, +d, +h, +mi, +se) / 1000;
    assert.ok(stamp >= startedAt && stamp <= endedAt, `the snapshot is the time of the run (${stamp} not in ${startedAt}..${endedAt}), not the midnight of its day`);
    assert.doesNotMatch(list, /convenience only|whether or not a link still works/);
    assert.doesNotMatch(list, /lib-sources|\.tar\b/);
    const entries = [
      'usr/lib/fallback/libwebkit2gtk-4.1.so.0 libwebkit2gtk-4.1-0 1.19-0ubuntu1 libwebkit2gtk-4.1-0 1.19-0ubuntu1 https://launchpad.net/ubuntu/+source/libwebkit2gtk-4.1-0/1.19-0ubuntu1',
      'usr/lib/fallback/libgtk-3.so.0 libgtk-3-0 1.10-0ubuntu1 gtk+3.0 3.24.33-1ubuntu2 https://launchpad.net/ubuntu/+source/gtk+3.0/3.24.33-1ubuntu2',
      'usr/lib/gtk-3.0/3.0.0/immodules/im-xim.so libgtk-3-0 1.10-0ubuntu1 gtk+3.0 3.24.33-1ubuntu2 https://launchpad.net/ubuntu/+source/gtk+3.0/3.24.33-1ubuntu2',
      // The same name, each to the package that installed the file it was
      // copied from. Launchpad names a version with its epoch.
      'usr/lib/fallback/libfoo.so.1 libfoo1 1.7-0ubuntu1 foo 2:1.7-0ubuntu1 https://launchpad.net/ubuntu/+source/foo/2:1.7-0ubuntu1',
      'usr/lib/foo/plugins/libfoo.so.1 foo-plugins 1.11-0ubuntu1 foo-plugins 1.11-0ubuntu1 https://launchpad.net/ubuntu/+source/foo-plugins/1.11-0ubuntu1',
    ];
    for (const line of entries) assert.ok(list.includes(`\n${line}\n`), line);
    // Every listed library names its package, its version, its source
    // package and version (what the offer is honoured from), and the
    // Launchpad page of exactly that source version.
    const listed = list.split('\n').filter((l) => l.startsWith('usr/'));
    assert.strictEqual(listed.length, entries.length);
    for (const line of listed) {
      const m = line.match(/^usr\/\S+ \S+ \d\S* (\S+) (\d\S*) (\S+)$/);
      assert.ok(m, line);
      assert.strictEqual(m[3], `https://launchpad.net/ubuntu/+source/${m[1]}/${m[2]}`, line);
    }
    assert.doesNotMatch(list, /libown/, 'AeroFTP\'s own folder is not a bundled system library');
    // A name more than one package installs, which neither its path nor the
    // loader resolves to one of them, fails and names the candidates.
    ldcache(['libwebkit2gtk-4.1.so.0', 'libgtk-3.so.0']);
    fails(/fallback\/libfoo\.so\.1: installed under that name by more than one package \(foo-plugins libfoo1\)/);
    ldcache(['libwebkit2gtk-4.1.so.0', 'libgtk-3.so.0', 'libfoo.so.1']);
    // One package installs the name, but the loader resolves it to a file
    // elsewhere (a copy under /usr/local): that package is not where it came
    // from, so the build fails instead of naming it.
    const ldLine = (n, p) => `\t${n} (libc6,x86-64) => ${p}\n`;
    write('ldcache', `4 libs found in cache \`/etc/ld.so.cache'\n${ldLine('libwebkit2gtk-4.1.so.0', '/usr/local/lib/libwebkit2gtk-4.1.so.0')}${ldLine('libgtk-3.so.0', `${A}/libgtk-3.so.0`)}${ldLine('libfoo.so.1', `${A}/libfoo.so.1`)}`);
    fails(/fallback\/libwebkit2gtk-4\.1\.so\.0: only package libwebkit2gtk-4\.1-0 installs that name, but the loader resolves it \(ldconfig -p\) to \/usr\/local\/lib\/libwebkit2gtk-4\.1\.so\.0, a file that package does not own/);
    ldcache(['libwebkit2gtk-4.1.so.0', 'libgtk-3.so.0', 'libfoo.so.1']);
    run();
    // A plugin in its own folder that one package installs under that name,
    // but only in another folder: the package is not where it came from.
    write('app/usr/lib/gtk-4.0/immodules/im-xim.so', 'elf');
    fails(/gtk-4\.0\/immodules\/im-xim\.so: only package libgtk-3-0 installs that name, but at no path ending in \/gtk-4\.0\/immodules\/im-xim\.so/);
    fs.rmSync(path.join(tmpDir, 'app/usr/lib/gtk-4.0'), { recursive: true });
    // A nested plugin whose soname is installed by two packages (libfoo1 and
    // foo-plugins), at a path neither package owns: must fail rather than
    // falling back to ldconfig's flat entry for libfoo1.
    write('app/usr/lib/other/plugins/libfoo.so.1', 'elf');
    fails(/other\/plugins\/libfoo\.so\.1: installed under that name by more than one package \(foo-plugins libfoo1\)/);
    fs.rmSync(path.join(tmpDir, 'app/usr/lib/other'), { recursive: true });
    run();
    // A library no package installed, a package without its copyright file
    // or its version fails the build and is named: nothing is guessed.
    write('app/usr/lib/fallback/libsoup-3.0.so.0', 'elf');
    fails(/fallback\/libsoup-3\.0\.so\.0: no package installed it/);
    dpkg({ ...packages, 'libsoup-3.0-0': [`${A}/libsoup-3.0.so.0`] });
    fails(/fallback\/libsoup-3\.0\.so\.0: package libsoup-3\.0-0 ships no .*libsoup-3\.0-0\/copyright/);
    write('doc/libsoup-3.0-0/copyright', 'libsoup terms');
    run();
    const status = fs.readFileSync(path.join(tmpDir, 'dpkg/status'), 'utf8');
    write('dpkg/status', status.split('\n\n').filter((s) => !s.includes('Package: libsoup-3.0-0\n')).join('\n\n'));
    fails(/fallback\/libsoup-3\.0\.so\.0: package libsoup-3\.0-0 has no version or source version/);
    dpkg({ ...packages, 'libsoup-3.0-0': [`${A}/libsoup-3.0.so.0`] });
    run();
    // Every source version is checked once against Launchpad's primary
    // archive, at the exact URL of its .dsc (version without its epoch in the
    // file name, with it in the folder).
    fs.rmSync(path.join(tmpDir, 'fetched'), { force: true });
    run();
    const fetched = fs.readFileSync(path.join(tmpDir, 'fetched'), 'utf8').trim().split('\n');
    assert.ok(fetched.includes('https://launchpad.net/ubuntu/+archive/primary/+sourcefiles/foo/2:1.7-0ubuntu1/foo_1.7-0ubuntu1.dsc'), fetched.join('\n'));
    assert.ok(fetched.includes('https://launchpad.net/ubuntu/+archive/primary/+sourcefiles/gtk+3.0/3.24.33-1ubuntu2/gtk+3.0_3.24.33-1ubuntu2.dsc'));
    assert.strictEqual(new Set(fetched).size, fetched.length, 'each source version once');
    // A source Launchpad does not have (a PPA, another repository, a version
    // the archive lacks) fails the build and is named.
    write('dsc/missing-foo_1.7-0ubuntu1.dsc', '');
    fails(/fallback\/libfoo\.so\.1: package libfoo1: source foo 2:1\.7-0ubuntu1 is not in the Ubuntu archive \(Launchpad has no https:\/\/launchpad\.net\/ubuntu\/\+archive\/primary\/\+sourcefiles\/foo\/2:1\.7-0ubuntu1\/foo_1\.7-0ubuntu1\.dsc\)/);
    fs.rmSync(path.join(tmpDir, 'dsc/missing-foo_1.7-0ubuntu1.dsc'));
    // A .dsc of another version (a PPA build of the same name) fails too.
    write('dsc/foo_1.7-0ubuntu1.dsc', 'Format: 3.0 (quilt)\nSource: foo\nVersion: 2:1.7-0ubuntu1~ppa1\n');
    fails(/package libfoo1: Launchpad's \S+foo_1\.7-0ubuntu1\.dsc is source foo 2:1\.7-0ubuntu1~ppa1, not foo 2:1\.7-0ubuntu1/);
    fs.rmSync(path.join(tmpDir, 'dsc/foo_1.7-0ubuntu1.dsc'));
    // Launchpad down: two 502s are retried, three fail the build, never a
    // silent pass.
    write('dsc/down', '2');
    run();
    write('dsc/down', '1000');
    fails(/Launchpad did not answer for \S+ after three tries/);
    fs.rmSync(path.join(tmpDir, 'dsc/down'));
    run();
    // A folder find cannot read fails the build instead of shipping the
    // libraries it did list (as root every folder is readable).
    if (process.getuid?.() !== 0) {
      write('app/usr/lib/locked/libx.so', 'elf');
      fs.chmodSync(path.join(tmpDir, 'app/usr/lib/locked'), 0o000);
      try {
        fails(/could not list every library under/);
      } finally {
        fs.chmodSync(path.join(tmpDir, 'app/usr/lib/locked'), 0o755);
      }
    }
    // The release workflow runs it on the AppDir it packs.
    const workflow = fs.readFileSync(path.join(ROOT, '.github/workflows/build.yml'), 'utf8');
    assert.match(workflow, /\n {10}AEROFTP_VERSION=\$\(jq -r \.version src-tauri\/tauri\.conf\.json\)\n[\s\S]*?\n {10}"\$GITHUB_WORKSPACE\/src-tauri\/scripts\/patch-appimage-lib-fallback\.sh" "\$\(pwd\)"\n[\s\S]*?\n {10}"\$GITHUB_WORKSPACE\/src-tauri\/scripts\/bundle-appimage-lib-notices\.sh" "\$\(pwd\)" "\$AEROFTP_VERSION"\n[\s\S]*?appimagetool/);
    // The source is offered in writing, not downloaded or published by the build.
    assert.doesNotMatch(workflow, /download-appimage-lib-sources|apt-get source|lib-sources\.tar|deb-src/);
    assert.ok(!fs.existsSync(path.join(ROOT, 'src-tauri/scripts/download-appimage-lib-sources.sh')));
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('reviewed source texts: a license text inside a source comment is bound to version and digest, and interior edits or version bumps fail', () => {
  const reviewed = reviewedSourceTexts();
  assert.ok(reviewed.length > 0);
  for (const r of reviewed) {
    assert.ok(r.package && r.version && r.file && r.from && r.to && /^[0-9a-f]{64}$/.test(r.sha256) && r.license && r.source, JSON.stringify(r));
  }
  const r = reviewed.find((x) => x.package === 'tao' && x.file === 'src/keyboard.rs');
  assert.ok(r, 'the tao W3C text is reviewed');
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'source-text-fixture-'));
  try {
    const file = path.join(tmpDir, 'src', 'keyboard.rs');
    fs.mkdirSync(path.dirname(file), { recursive: true });
    const extracted = `${r.from}\nCopyright (c) 2016 Fixture Consortium. All Rights Reserved.\n\nPermission to copy, modify, and distribute this work is granted.\n${r.to}`;
    const sha256 = createHash('sha256').update(extracted, 'utf8').digest('hex');
    const fixtureReview = { ...r, version: '0.35.3', sha256 };
    const body = `// Copyright 2021 Fixture Author\n// SPDX-License-Identifier: Apache-2.0\n\n// ${r.from}\n// Copyright (c) 2016 Fixture Consortium. All Rights Reserved.\n//\n// Permission to copy, modify, and distribute this work is granted.\n// ${r.to}\n\nfn k() {}\n`;
    fs.writeFileSync(file, body);
    const makeComp = (version) => ({
      name: 'tao',
      version,
      license: 'Apache-2.0',
      source: 'cargo',
      packageDir: tmpDir,
      authors: ['Fixture Author'],
      ...readPackageFiles(tmpDir, `tao@${version}`),
    });
    // Control: matching version and unchanged block digest succeeds.
    const md = generateNoticesMarkdown([makeComp('0.35.3')], [], SPDX_STANDARD_LICENSES_FILE, [], [fixtureReview]);
    const section = md.slice(md.indexOf('## License Texts Inside Source Files'));
    assert.ok(md.includes('## License Texts Inside Source Files'));
    assert.ok(section.includes(`- \`tao@0.35.3\`, \`src/keyboard.rs\`: ${r.license}`));
    assert.ok(section.includes(`${extracted}\n`), 'the comment is reproduced from its first to its last line, without the leaders');
    assert.ok(!section.includes('fn k()'));
    // Version mismatch requires a new review instead of silently accepting or omitting.
    assert.throws(
      () => generateNoticesMarkdown([makeComp('9.9.9')], [], SPDX_STANDARD_LICENSES_FILE, [], [fixtureReview]),
      /does not match reviewed sourceTexts version 0\.35\.3/
    );
    // Interior edit with unchanged boundary lines fails on digest mismatch.
    fs.writeFileSync(file, body.replace('Permission to copy, modify, and distribute this work is granted.', 'All commercial use is forbidden.'));
    assert.throws(
      () => generateNoticesMarkdown([makeComp('0.35.3')], [], SPDX_STANDARD_LICENSES_FILE, [], [fixtureReview]),
      /reviewed source text digest changed/
    );
    // Missing boundary line still fails.
    fs.writeFileSync(file, body.replace(`// ${r.to}\n`, ''));
    assert.throws(
      () => generateNoticesMarkdown([makeComp('0.35.3')], [], SPDX_STANDARD_LICENSES_FILE, [], [fixtureReview]),
      /no longer has the reviewed source text/
    );
    // Direct helper check on matching block.
    fs.writeFileSync(file, body);
    assert.strictEqual(reviewedSourceText(makeComp('0.35.3'), fixtureReview), extracted);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('findPackageCopyrightNotices: a template copyright line is no notice, a license text keeps its placeholders', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'template-notice-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('apps/progs.pl', 'print "/*\\n * Copyright 1995-$YEAR The Example Project Authors. All Rights Reserved.\\n */\\n";\n');
    write('src/loclang.hpp', '#define MCopyright "\\nCopyright (c) 1993-%d Example Author   %d %s %d"\n');
    write('src/tmpl.c', '/* Copyright (c) <year> <owner>. */\n/* Copyright (c) [year] [fullname] */\n/* Copyright ${YEAR} ${OWNER} */\n');
    write('src/real.c', '/* Copyright (c) 2001 Real Holder <real@example.org> */\n');
    write('debian/copyright', 'Copyright: <year> <copyright holders>\nCopyright: 2010 Field Holder\n');
    const notices = findPackageCopyrightNotices(tmpDir).map((n) => n.notice);
    assert.deepStrictEqual(notices, ['Copyright (c) 2001 Real Holder <real@example.org>', 'Copyright 2010 Field Holder']);

    // A license file is reproduced as shipped, its placeholder included.
    fs.rmSync(path.join(tmpDir, 'debian'), { recursive: true });
    write('LICENSE', 'MIT License\n\nCopyright (c) <year> <owner>\n\nPermission is hereby granted, free of charge.');
    const comp = { name: 'template-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Real Holder'], ...readPackageFiles(tmpDir, 'template-pkg@1.0.0') };
    const md = generateNoticesMarkdown([comp], []);
    assert.ok(md.includes('```text\nMIT License\n\nCopyright (c) <year> <owner>\n'), 'the license text keeps its placeholder');
    assert.doesNotMatch(md, /^ {2}- .*(?:\$YEAR|%d|<year>|\[year\]|\$\{YEAR\})/m, 'no attribution line carries a template slot');
    // The template line names no holder, so the source scan runs and finds the real one.
    assert.strictEqual(hasCopyrightHolderInFiles(comp), false);
    assert.match(md, /^ {2}- Copyright \(c\) 2001 Real Holder <real@example\.org> \(src\/real\.c\)$/m);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a header under the declared license with its own terms is reproduced, one with the standard terms is not', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'declared-own-terms-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    const standard = JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'));
    write('LICENSE', standard['MIT'].replace(/Copyright \(c\) <year> <copyright holders>/, 'Copyright (c) 2020 Wrapper Author'));
    const ownTerms = 'Permission is hereby granted to use this file only together with the Foo engine, provided that this notice is kept.';
    write('lib/own.js', `/*\n * Copyright (c) 2018 Own Terms Vendor\n * ${ownTerms}\n * SPDX-License-Identifier: MIT\n */\n`);
    const mitTerms = standard['MIT'].split('\n').filter((l) => !/^(?:MIT License|Copyright)/.test(l));
    const mayTerms = 'You may use, copy, modify and distribute this software provided you retain this notice and credit Example Project.';
    write('lib/may.js', `/*\n * Copyright (c) 2017 May Vendor\n * ${mayTerms}\n * SPDX-License-Identifier: MIT\n */\n`);
    const batTerms = 'You may use, copy, modify and distribute this script provided you keep this notice.';
    write('lib/build.bat', `@echo off\nREM -- Copyright (c) 2016 Batch Vendor\nREM -- ${batTerms}\nREM -- SPDX-License-Identifier: MIT\n`);
    write('lib/tpl.js', '/*\n * Copyright (c) <year> <copyright holders>\n * SPDX-License-Identifier: MIT\n */\n');
    const tplTerms = 'Permission is granted to use this file only within the Foo engine.';
    write('lib/tplterms.js', `/*\n * Copyright (c) <year> <copyright holders>\n * ${tplTerms}\n * SPDX-License-Identifier: MIT\n */\n`);
    write('lib/std.js', `/*\n * Copyright (c) 2019 Standard Vendor\n${mitTerms.map((l) => (l ? ` * ${l}` : ' *')).join('\n')}\n * SPDX-License-Identifier: MIT\n */\n`);
    const comp = { name: 'own-terms-pkg', version: '1.0.0', license: 'MIT', source: 'npm', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'own-terms-pkg@1.0.0') };
    const md = generateNoticesMarkdown([comp], []);
    const section = md.slice(md.indexOf('## Source Headers With Their Own Terms'));
    assert.ok(md.includes('## Source Headers With Their Own Terms'), 'the section exists');
    assert.ok(section.includes(ownTerms), 'the differing terms are reproduced');
    assert.match(section, /ships a header tagged `MIT` whose permission text differs from it, in `lib\/own\.js`/);
    assert.ok(section.includes(mayTerms), 'permission text in other words is reproduced too');
    // Every accepted comment style keeps its block: holder and terms of a `REM --` header.
    assert.ok(section.includes(batTerms), 'the terms of a REM header are reproduced');
    // A header reproduced as shipped keeps its template line, and generation does not fail on it.
    assert.ok(section.includes(`Copyright (c) <year> <copyright holders>\n${tplTerms}`), 'the template header is reproduced verbatim');
    assert.match(md, /^ {2}- Copyright \(c\) 2016 Batch Vendor \(/m);
    assert.doesNotMatch(section, /^echo off$/m, 'a batch command is no comment line');
    assert.doesNotMatch(section, /lib\/std\.js/, 'a header with the standard terms is not repeated');
    assert.doesNotMatch(md, /### Bundled Code: MIT/);
    assert.doesNotMatch(md, /<year> <copyright holders> \(in a source header/, 'a template line in a header names no holder');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a header covered by a license text shipped beside the code is reproduced only with terms of its own', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'shipped-covered-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    const standard = JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'));
    assert.ok(!('curl' in standard), 'the fixture needs an identifier with no standard text');
    write('LICENSE', 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted, free of charge.');
    // The reviewed terms of curl: a text beside the code covers its headers
    // only when it carries them whole.
    const shippedTerms = reviewedLicenseTerms().curl.trimEnd();
    write('vend/LICENSES/curl.txt', `COPYRIGHT AND PERMISSION NOTICE\n\nCopyright (c) 1996 - 2025, Daniel Stenberg, <daniel@haxx.se>.\n\n${shippedTerms}`);
    write('vend/lib/plain.c', '/*\n * Copyright (C) Viktor Szakats\n * SPDX-License-Identifier: curl\n */\n');
    write('vend/lib/same.c', `/*\n * Copyright (C) Daniel Stenberg, <daniel@haxx.se>.\n *\n${shippedTerms.split('\n').map((l) => ` * ${l}`).join('\n')}\n *\n * SPDX-License-Identifier: curl\n */\n`);
    const ownTerms = 'Permission to use this file is hereby granted only inside the Foo engine.';
    write('vend/lib/own.c', `/*\n * Copyright (C) Own Terms Vendor\n * ${ownTerms}\n * SPDX-License-Identifier: curl\n */\n`);
    const comp = { name: 'shipped-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'shipped-pkg@1.0.0') };
    const md = generateNoticesMarkdown([comp], []);
    // The holders of the vendored code are listed with its folder, once.
    assert.match(md, /^ {2}- Copyright \(C\) Viktor Szakats \(vend\/lib\/plain\.c\)$/m);
    assert.doesNotMatch(md, /\(in a source header under curl\)/);
    const section = md.slice(md.indexOf('## Source Headers With Their Own Terms'));
    assert.ok(md.includes('## Source Headers With Their Own Terms'), 'the header with terms of its own is reproduced');
    assert.ok(section.includes(ownTerms));
    assert.match(section, /ships a header tagged `curl` whose permission text differs from it, in `vend\/lib\/own\.c`/);
    // The component declares MIT, not curl: the text names no declaration it does not make.
    assert.doesNotMatch(section, /declare the license of these headers themselves/);
    assert.match(section, /or a license text shipped beside the vendored code/);
    assert.doesNotMatch(section, /same\.c|plain\.c/, 'a header with the shipped terms, or with none, is not repeated');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('assertNoGeneratedPlaceholders: a placeholder fails in generated text, not in a block reproduced as shipped', () => {
  const block = '```text\nCopyright (c) <year> <copyright holders>\nPermission is granted.\n```';
  assert.doesNotThrow(() => assertNoGeneratedPlaceholders(`# Notices\n\n${block}\n`, [block]));
  assert.throws(() => assertNoGeneratedPlaceholders(`# Notices\n\n${block}\n`, []), /unpopulated placeholders/);
  assert.throws(() => assertNoGeneratedPlaceholders(`- Copyright (c) <copyright holders>\n\n${block}\n`, [block]), /unpopulated placeholders/);
  assert.throws(() => assertNoGeneratedPlaceholders('- authors: see repository maintainers\n', []), /unpopulated placeholders/);
});

test('readPackageFiles: a root license folder follows the rule of a nested one', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'root-license-dir-fixture-'));
  try {
    // Only a README in LICENSES/: no license text, so the standard text is used.
    fs.mkdirSync(path.join(tmpDir, 'LICENSES'));
    fs.writeFileSync(path.join(tmpDir, 'LICENSES', 'README.md'), 'The license texts of this package.\n');
    const only = readPackageFiles(tmpDir, 'test-readme-only@1.0.0');
    assert.deepStrictEqual([...only.licenseFiles, ...only.noticeFiles], []);
    const comp = { name: 'test-readme-only', version: '1.0.0', license: 'MIT', source: 'npm', packageDir: tmpDir, authors: ['Readme Author'], ...only };
    const readmeOnly = generateNoticesMarkdown([comp], []);
    assert.match(readmeOnly, /### Standard License: MIT\n/);
    assert.match(readmeOnly, /Permission is hereby granted, free of charge/, 'the standard MIT permission text is rendered');
    assert.doesNotMatch(readmeOnly, /The license texts of this package\./, 'the README is not rendered as a license text');
    // The British spelling is a license folder too, as in vendored code.
    fs.rmSync(path.join(tmpDir, 'LICENSES'), { recursive: true });
    fs.mkdirSync(path.join(tmpDir, 'licence'));
    fs.writeFileSync(path.join(tmpDir, 'licence', 'MIT.txt'), 'MIT License\n\nCopyright (c) 2020 Licence Author\n\nPermission is hereby granted.');
    assert.deepStrictEqual(readPackageFiles(tmpDir, 'x@1').licenseFiles.map((f) => f.filename), ['licence/MIT.txt']);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a license text in the package LICENSES/ covers its own headers, as a vendored one covers its code', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'root-reuse-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', shippedLicense('MIT License\n\nCopyright (c) 2020 Wrapper Author', 'MIT'));
    write('LICENSES/SAX-PD-2.0.txt', `SAX2 is Free!\n\n${reviewedLicenseTerms()['SAX-PD-2.0']}`);
    write('src/y.c', '/* SPDX-License-Identifier: SAX-PD-2.0 */\n');
    const comp = { name: 'root-reuse', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: [], ...readPackageFiles(tmpDir, 'root-reuse@1.0.0') };
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(comp, known), []);
    // A header covered by that text keeps a holder the package's files do not
    // name: nothing else lists it, because the package's own holders stop the
    // source scan.
    write('src/z.c', '/* Copyright (c) 2018 Other Source Author\n * SPDX-License-Identifier: SAX-PD-2.0 */\n');
    const withHolder = { ...comp, ...readPackageFiles(tmpDir, 'root-reuse@1.0.0') };
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(withHolder, known).map((b) => [b.license, b.notices, b.files, b.headers.length, b.declared]), [['SAX-PD-2.0', ['Copyright (c) 2018 Other Source Author'], ['src/z.c'], 0, true]]);
    assert.match(generateNoticesMarkdown([withHolder], []), /^ {2}- Copyright \(c\) 2018 Other Source Author \(in a source header under SAX-PD-2\.0\)$/m);
    // The same header inside vendored code is listed with that code's folder,
    // not twice.
    write('vend/lib/COPYING', 'Copyright (c) 2018 Other Source Author\nRedistribution and use in source and binary forms.');
    write('vend/lib/w.c', '/* Copyright (c) 2018 Other Source Author\n * SPDX-License-Identifier: SAX-PD-2.0 */\n');
    fs.rmSync(path.join(tmpDir, 'src/z.c'));
    const vendoredCase = { ...comp, ...readPackageFiles(tmpDir, 'root-reuse@1.0.0') };
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(vendoredCase, known), []);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('declared header notices: a notice the package files carry is not repeated, another holder, year or scope is listed', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'exact-holder-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', shippedLicense('MIT License\n\nCopyright (c) 2020 Alice Smith\n\n  - Intel Corporation (Copyright 2012-2021)', 'MIT'));
    write('src/a.c', '/* Copyright (c) 2021 Alice\n * SPDX-License-Identifier: MIT */\n');
    write('src/b.c', '/* Copyright (C) 2019 Alice Smith.\n * SPDX-License-Identifier: MIT */\n');
    write('src/c.c', '/* Copyright (c) 2014, Intel Corporation. All Rights Reserved.\n * SPDX-License-Identifier: MIT */\n');
    write('src/d.c', '/* COPYRIGHT © 2020  Alice Smith.\n * SPDX-License-Identifier: MIT */\n');
    write('build.bat', '@echo off\nREM -- Copyright Batch Vendor\nREM -- SPDX-License-Identifier: MIT\n');
    const comp = { name: 'exact-holder', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: [], ...readPackageFiles(tmpDir, 'exact-holder@1.0.0') };
    const md = generateNoticesMarkdown([comp], []);
    assert.match(md, /^ {2}- Copyright \(c\) 2021 Alice \(in a source header under MIT\)$/m, 'Alice is not Alice Smith');
    assert.match(md, /^ {2}- Copyright \(C\) 2019 Alice Smith\. \(in a source header under MIT\)$/m, 'the license holder with other years is another notice');
    assert.match(md, /^ {2}- Copyright \(c\) 2014, Intel Corporation\. All Rights Reserved\. \(in a source header under MIT\)$/m, 'a notice the credits list does not carry is listed');
    assert.doesNotMatch(md, /2020 {1,2}Alice Smith\.? \(in a source header/, 'the notice the license carries is attributed by the package');
    assert.match(md, /^ {2}- Copyright Batch Vendor \(in a source header under MIT\)$/m, 'a REM header names its holder');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a header whose only license is a known typo keeps a holder the package does not name', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'typo-holder-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', shippedLicense('MIT License\n\nCopyright (c) 2020 Wrapper Author', 'MIT'));
    write('src/menu.rs', '// Copyright 2022-2022 Typo Header Author\n// SPDX-License-Identifier: Apache-2.inner\n');
    write('src/items/mod.rs', '// Copyright 2020 Wrapper Author\n// SPDX-License-Identifier: Apache-2.inner\n');
    write('src/items/bare.rs', '// SPDX-License-Identifier: Apache-2.inner\n');
    const comp = { name: 'muda', version: '0.19.3', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: [], ...readPackageFiles(tmpDir, 'muda@0.19.3') };
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(comp, known).map((b) => [b.license, b.notices, b.files, b.headers.length, b.declared]), [['Apache-2.inner', ['Copyright 2020 Wrapper Author', 'Copyright 2022-2022 Typo Header Author'], ['src/items/mod.rs', 'src/menu.rs'], 0, true]]);
    const md = generateNoticesMarkdown([comp], []);
    assert.match(md, /^ {2}- Copyright 2022-2022 Typo Header Author \(in a source header under Apache-2\.inner\)$/m);
    assert.doesNotMatch(md, /Copyright 2020 Wrapper Author \(in a source header/, 'the package names this holder itself');
    assert.throws(
      () => findBundledCodeUnderOtherLicenses({ ...comp, name: 'unrelated', version: '1.0.0' }, known),
      /unrelated@1\.0\.0.*Apache-2\.inner/,
      'the reviewed typo exception must not apply to another package'
    );
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a Rust header with a nested block comment is read to its outer closer, a C one ends at its first', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'nested-rust-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', shippedLicense('MIT License\n\nCopyright (c) 2020 Wrapper Author', 'MIT'));
    const grant = 'Permission to use this file is granted, provided that the Nested clause is kept with every copy of it.';
    // Rust nests block comments: the holder and the grant after the inner
    // closer are still in the header.
    write('src/nested.rs', `/* SPDX-License-Identifier: BSD-2-Clause\n * /* Aside */\n * Copyright (c) 2021 Nested Holder\n * ${grant}\n */\nfn x() {}\n`);
    const comp = { name: 'nested-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: [], ...readPackageFiles(tmpDir, 'nested-pkg@1.0.0') };
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    const [bundled] = findBundledCodeUnderOtherLicenses(comp, known);
    assert.deepStrictEqual([bundled.license, bundled.files, bundled.notices], ['BSD-2-Clause', ['src/nested.rs'], ['Copyright (c) 2021 Nested Holder']]);
    const md = generateNoticesMarkdown([comp], []);
    assert.match(md, /^ {2}- Copyright \(c\) 2021 Nested Holder$/m);
    assert.ok(md.includes('the Nested clause is kept with every copy of it'), 'the grant after the inner closer is reproduced');
    // C does not nest: the same text in a .c file ends at the first closer,
    // so the lines after it are no header.
    fs.rmSync(path.join(tmpDir, 'src/nested.rs'));
    write('src/flat.c', `/* SPDX-License-Identifier: BSD-2-Clause\n * /* Aside */\n * Copyright (c) 2021 Flat Holder\n */\nint x;\n`);
    const flat = { ...comp, ...readPackageFiles(tmpDir, 'nested-pkg@1.0.0') };
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(flat, known).map((b) => b.notices), [[]]);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: in Rust the outer comment decides, past an inner closer before the tag and a quote in a character literal', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'rust-tag-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', shippedLicense('MIT License\n\nCopyright (c) 2020 Wrapper Author', 'MIT'));
    const grant = (who) => `Permission to use this file is granted, provided that the ${who} clause is kept with every copy of it.`;
    // An inner comment closed before the tag, on the tag's line.
    write('src/inner.rs', `/* Copyright (c) 2021 Inner Tag Holder\n * /* note */ SPDX-License-Identifier: BSD-2-Clause\n * ${grant('Inner')}\n */\nfn x() {}\n`);
    // A character literal holding a double quote before the opener.
    write('src/chr.rs', `fn c() -> char { '"' } /*\nCopyright (c) 2021 Char Holder\nSPDX-License-Identifier: BSD-2-Clause\n${grant('Char')}\n*/\n`);
    const comp = { name: 'rust-tag-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: [], ...readPackageFiles(tmpDir, 'rust-tag-pkg@1.0.0') };
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    const bundled = findBundledCodeUnderOtherLicenses(comp, known);
    assert.deepStrictEqual(bundled.map((b) => [b.license, b.files, b.notices]), [['BSD-2-Clause', ['src/chr.rs', 'src/inner.rs'], ['Copyright (c) 2021 Char Holder', 'Copyright (c) 2021 Inner Tag Holder']]]);
    const md = generateNoticesMarkdown([comp], []);
    for (const who of ['Inner', 'Char']) assert.ok(md.includes(`the ${who} clause is kept with every copy of it`), who);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: an OCaml or Haskell header ends at its closer, as a C one does', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'closer-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', shippedLicense('MIT License\n\nCopyright (c) 2020 Wrapper Author', 'MIT'));
    write('lib/one.ml', '(* Copyright (c) 2019 OCaml Line Author *)\n(* SPDX-License-Identifier: BSD-2-Clause *)\nlet x = 1\n');
    write('lib/block.ml', '(*\n Copyright (c) 2019 OCaml Block Author\n SPDX-License-Identifier: BSD-2-Clause\n*)\nlet y = (*p*) 2\n');
    write('lib/b.hs', '{- Copyright (c) 2018 Haskell Author\n   SPDX-License-Identifier: BSD-2-Clause -}\nmodule B where\n');
    write('lib/c.c', '/* Copyright (c) 2017 C Author\n * SPDX-License-Identifier: BSD-2-Clause */\nint f(int *p) { return (*p); }\n');
    const comp = { name: 'closer-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: [], ...readPackageFiles(tmpDir, 'closer-pkg@1.0.0') };
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    const bundled = findBundledCodeUnderOtherLicenses(comp, known);
    assert.deepStrictEqual(bundled.map((b) => [b.license, [...b.files].sort(), [...b.notices].sort()]), [
      ['BSD-2-Clause', ['lib/b.hs', 'lib/block.ml', 'lib/c.c', 'lib/one.ml'], ['Copyright (c) 2017 C Author', 'Copyright (c) 2018 Haskell Author', 'Copyright (c) 2019 OCaml Block Author', 'Copyright (c) 2019 OCaml Line Author']],
    ]);
    const md = generateNoticesMarkdown([comp], []);
    assert.match(md, /### Bundled Code: BSD-2-Clause\n/);
    assert.match(md, /Redistribution and use in source and binary forms/);
    for (const holder of ['OCaml Line Author', 'OCaml Block Author', 'Haskell Author', 'C Author']) assert.match(md, new RegExp(`^ {2}- Copyright \\(c\\) \\d{4} ${holder}$`, 'm'));
    assert.doesNotMatch(md, /\*\)|-\}/, 'no closer is left in a tag or a holder line');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a header license goes through the GPL-3.0 policy, however it is covered', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'header-policy-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted, free of charge.');
    write('vendor/lib/LICENSES/GPL-2.0-only.txt', 'GNU GENERAL PUBLIC LICENSE\nVersion 2, June 1991');
    write('vendor/lib/x.c', '/* Copyright (c) 2001 Vendored Author\n * SPDX-License-Identifier: GPL-2.0-only */\n');
    const make = () => ({ name: 'policy-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: [], ...readPackageFiles(tmpDir, 'policy-pkg@1.0.0') });
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    assert.throws(() => findBundledCodeUnderOtherLicenses(make(), known), /policy-pkg@1\.0\.0 bundles vendor\/lib\/x\.c under 'GPL-2\.0-only', and no choice of it is compatible with GPL-3\.0/);
    // A choice that is compatible is taken instead.
    write('vendor/lib/x.c', '/* Copyright (c) 2001 Vendored Author\n * SPDX-License-Identifier: GPL-2.0-only OR MIT */\n');
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(make(), known).map((b) => [b.license, b.declared]), [['MIT', true]]);
    // A header under its own license, not declared by the package, fails the same way.
    write('vendor/lib/x.c', '/* SPDX-License-Identifier: SSPL-1.0 */\n');
    assert.throws(() => findBundledCodeUnderOtherLicenses(make(), known), /under 'SSPL-1\.0', and no choice of it is compatible/);
    // The permissive texts libssh2 ships beside its code stay accepted.
    for (const id of ['FSFULLR', 'curl', 'SAX-PD-2.0']) assert.strictEqual(isGplCompatible(id), true, id);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a known typo is dropped term by term, the other terms of its choice still go through the policy', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'typo-conjunction-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted, free of charge.');
    const make = () => ({ name: 'muda', version: '0.19.3', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: [], ...readPackageFiles(tmpDir, 'muda@0.19.3') });
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    // The typo hid the mandatory GPL-2.0-only term: the whole choice was dropped before the policy.
    write('src/menu.rs', '// Copyright 2022 Typo Author\n// SPDX-License-Identifier: Apache-2.inner AND GPL-2.0-only\n');
    assert.throws(() => findBundledCodeUnderOtherLicenses(make(), known), /muda@0\.19\.3 bundles src\/menu\.rs under 'Apache-2\.inner AND GPL-2\.0-only', and no choice of it is compatible with GPL-3\.0/);
    // A compatible term beside the typo is the header's license.
    write('src/menu.rs', '// Copyright 2022 Typo Author\n// SPDX-License-Identifier: Apache-2.inner AND MIT\n');
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(make(), known).map((b) => [b.license, b.notices, b.declared]), [['MIT', ['Copyright 2022 Typo Author'], true]]);
    // A header whose only license is the typo keeps its holder under the raw tag, as before.
    write('src/menu.rs', '// Copyright 2022 Typo Author\n// SPDX-License-Identifier: Apache-2.inner\n');
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(make(), known).map((b) => [b.license, b.notices, b.declared]), [['Apache-2.inner', ['Copyright 2022 Typo Author'], true]]);
    // Only a reviewed typo is one: a tag named like what every object
    // inherits is an unsupported license, never a typo.
    for (const tag of ['constructor', 'toString', '__proto__']) {
      write('src/menu.rs', `// Copyright 2022 Typo Author\n// SPDX-License-Identifier: ${tag}\n`);
      assert.throws(() => findBundledCodeUnderOtherLicenses(make(), known), new RegExp(`'${tag}'`), tag);
    }
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a block comment is found however far its opener is from the tag, and an opener in a string is code', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'long-block-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', shippedLicense('MIT License\n\nCopyright (c) 2020 Wrapper Author', 'MIT'));
    const terms = 'Redistribution and use in source and binary forms, with or without modification, are permitted provided that this notice is kept and the Long clause is honoured.';
    // 120 lines of unstarred text between the opener and the tag.
    const filler = Array.from({ length: 120 }, (_, n) => `Clause ${n + 1} of the long header.`).join('\n');
    write('vendor/long.c', `int before;\n/* Copyright (c) 2001 Adjacent Holder */\n/*\nCopyright (c) 2019 Long Vendor\n\n${terms}\n${filler}\n\nSPDX-License-Identifier: BSD-2-Clause\n*/\nint x;\n/* Copyright (c) 2002 After Holder */\n`);
    // A line-comment header below code whose string holds `/*`, and a later block comment.
    write('vendor/glob.c', 'const char *glob = "src/*";\n// Copyright (c) 2004 Line Vendor\n// SPDX-License-Identifier: BSD-3-Clause\nint y;\n/* Copyright (c) 2003 Later Holder */\n');
    // A quoted glob inside the comment does not hide the comment's own opener.
    write('vendor/glob-comment.c', 'int z;\n/* Copyright 2020 Glob Vendor; matches "src/*".\n * SPDX-License-Identifier: ISC\n */\n');
    const comp = { name: 'long-block-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'long-block-pkg@1.0.0') };
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    const found = findBundledCodeUnderOtherLicenses(comp, known);
    assert.deepStrictEqual(found.map((b) => [b.license, b.notices, b.files]), [
      ['BSD-2-Clause', ['Copyright (c) 2019 Long Vendor'], ['vendor/long.c']],
      ['BSD-3-Clause', ['Copyright (c) 2004 Line Vendor'], ['vendor/glob.c']],
      ['ISC', ['Copyright 2020 Glob Vendor; matches "src/*".'], ['vendor/glob-comment.c']],
    ]);
    assert.strictEqual(found[0].headers[0].text, `Copyright (c) 2019 Long Vendor\n\n${terms}\n${filler}\n\nSPDX-License-Identifier: BSD-2-Clause`);
    const md = generateNoticesMarkdown([comp], []);
    const bsd = md.slice(md.indexOf('### Bundled Code: BSD-2-Clause'));
    assert.ok(bsd.includes(terms), 'the shipped permission text is reproduced');
    // The notices outside the headers are the package's sources, listed with
    // the package; a header holder is listed only with its bundled code.
    const entry = md.slice(md.indexOf('`long-block-pkg@1.0.0`'), md.indexOf('### Bundled Code'));
    assert.deepStrictEqual(entry.match(/^ {2}- .*$/gm), [
      '  - Copyright (c) 2001 Adjacent Holder (vendor/long.c)',
      '  - Copyright (c) 2002 After Holder (vendor/long.c)',
      '  - Copyright (c) 2003 Later Holder (vendor/glob.c)',
    ]);
    assert.doesNotMatch(md.slice(md.indexOf('### Bundled Code')), /Adjacent Holder|After Holder|Later Holder|No copyright line in the header/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a run of line comments ends where the comment style changes, however long it is', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'line-run-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted.');
    // A `//` header above a crate attribute (`#` is also a comment leader) and `//!` docs.
    write('src/lib.rs', '// Copyright 2020 Rust Vendor\n// SPDX-License-Identifier: BSD-3-Clause\n#![allow(dead_code)]\n//! Copyright 2021 Doc Example, quoted in the crate docs.\n');
    // A `//` header with its holder 100 lines above the tag.
    const clauses = Array.from({ length: 100 }, (_, n) => `// Clause ${n + 1} of the long line header.`).join('\n');
    write('src/long.rs', `// Copyright 2018 Long Line Vendor\n${clauses}\n// SPDX-License-Identifier: BSD-2-Clause\n#include <stdio.h>\n// Copyright 2017 After Include\n`);
    const comp = { name: 'line-run-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'line-run-pkg@1.0.0') };
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(comp, known).map((b) => [b.license, b.notices, b.files]), [
      ['BSD-2-Clause', ['Copyright 2018 Long Line Vendor'], ['src/long.rs']],
      ['BSD-3-Clause', ['Copyright 2020 Rust Vendor'], ['src/lib.rs']],
    ]);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a copyright line that states a condition is a term, only attribution lines leave the comparison', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'copyright-condition-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    const standard = JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'));
    write('LICENSE', standard['MIT'].replace(/Copyright \(c\) <year> <copyright holders>/, 'Copyright (c) 2020 Wrapper Author'));
    const mitTerms = standard['MIT'].split('\n').filter((l) => !/^(?:MIT License|Copyright)/.test(l));
    const condition = 'Copyright notices must remain visible in the user interface.';
    const header = (holder, extra) => `/*\n * Copyright (c) ${holder}\n${[...mitTerms, ...extra].map((l) => (l ? ` * ${l}` : ' *')).join('\n')}\n * SPDX-License-Identifier: MIT\n */\n`;
    write('lib/visible.js', header('2018 Visible Vendor', ['', condition]));
    const shouted = 'Copyright MUST remain visible in the user interface.';
    write('lib/shouted.js', header('2018 Shouted Vendor', ['', shouted]));
    write('lib/std.js', header('2019 Standard Vendor', []));
    const reservedCondition = 'All rights reserved. Commercial distribution is prohibited.';
    write('lib/reserved.js', header('2020 Reserved Vendor', ['', reservedCondition]));
    assert.strictEqual(namesCopyrightHolder(condition), false);
    assert.strictEqual(namesCopyrightHolder('Copyright (c) 2018 Visible Vendor'), true);
    assert.strictEqual(namesCopyrightHolder(shouted), false, 'a condition in capitals names nobody');
    assert.strictEqual(namesCopyrightHolder('Copyright Visible Vendor'), true, 'a capitalized holder is still one');
    // A modal in Title Case is a first name, not a condition.
    for (const holder of ['Copyright Will Smith', 'Copyright Will Smith 2020', 'Copyright May Lee']) assert.strictEqual(namesCopyrightHolder(holder), true, holder);
    for (const cond of ['Copyright must remain visible', 'Copyright MUST remain visible', 'Copyright may not be removed']) assert.strictEqual(namesCopyrightHolder(cond), false, cond);
    assert.ok(licenseTermsOf(`Copyright (c) 2018 Visible Vendor\n${condition}\nPermission is hereby granted.`).includes(condition));
    const comp = { name: 'condition-pkg', version: '1.0.0', license: 'MIT', source: 'npm', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'condition-pkg@1.0.0') };
    const md = generateNoticesMarkdown([comp], []);
    const section = md.slice(md.indexOf('## Source Headers With Their Own Terms'));
    assert.ok(md.includes('## Source Headers With Their Own Terms'), 'the section exists');
    assert.match(section, /ships a header tagged `MIT` whose permission text differs from it, in `lib\/visible\.js`/);
    assert.ok(section.includes(condition), 'the condition is reproduced');
    assert.match(section, /in `lib\/shouted\.js`/);
    assert.ok(section.includes(shouted), 'the condition in capitals is reproduced');
    assert.match(section, /in `lib\/reserved\.js`/);
    assert.ok(section.includes(reservedCondition), 'a condition beside All rights reserved is reproduced');
    assert.strictEqual(licenseTermsOf('All rights reserved.\nPermission is hereby granted.'), 'Permission is hereby granted.');
    // The standard terms under another holder are still the package's text.
    assert.doesNotMatch(section, /lib\/std\.js/, 'a header with the standard terms is not repeated');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('findPackageCopyrightNotices: a notice after any accepted comment leader is found', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'leader-notice-fixture-'));
  try {
    fs.writeFileSync(path.join(tmpDir, 'build.bat'), '@echo off\nREM -- Copyright Batch Vendor\n');
    fs.writeFileSync(path.join(tmpDir, 'x.lua'), '-- Copyright Lua Vendor\n');
    assert.deepStrictEqual(findPackageCopyrightNotices(tmpDir, [], []).map((n) => n.notice).sort(), ['Copyright Batch Vendor', 'Copyright Lua Vendor']);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('isSpdxHeaderLine: a comment line is a header whatever precedes the tag, prose and list items are not', () => {
  const at = (l) => l.indexOf('SPDX-License-Identifier:');
  const header = (l, rel = 'src/a.c') => isSpdxHeaderLine(l, rel, at(l));
  for (const l of [
    '// SPDX-License-Identifier: MIT',
    '/* Copyright 2020 Foo Ltd. SPDX-License-Identifier: BSD-2-Clause */',
    '// Licensed to X. SPDX-License-Identifier: Apache-2.0',
    ' * SPDX-License-Identifier: MIT',
    '@ SPDX-License-Identifier: Apache-2.0',
    '$! SPDX-License-Identifier: BSD-3-Clause',
    'REM -- SPDX-License-Identifier: Apache-2.0 OR ISC',
    'SPDX-License-Identifier: Apache-2.0 OR MIT',
    // A C block comment opened after code on the line.
    'int x; /* SPDX-License-Identifier: MIT */',
    'static int y = 0; /* "quoted" words, SPDX-License-Identifier: ISC */',
  ]) {
    assert.ok(header(l), l);
  }
  assert.ok(header('SPDX-License-Identifier: BSD-3-Clause', 'docs/HACKING.md'));
  assert.ok(header('<!-- SPDX-License-Identifier: MIT -->', 'README.md'));
  for (const [l, rel] of [
    ['Documentation: SPDX-License-Identifier: MIT (preferred)', 'docs/notes.md'],
    ['- SPDX-License-Identifier: MIT', 'README.md'],
    ['* SPDX-License-Identifier: MIT', 'README.md'],
    ['1. SPDX-License-Identifier: MIT', 'README.rst'],
    ['const S: &str = "// SPDX-License-Identifier: MIT";', 'src/html.rs'],
    // A comment opener inside a string literal opens nothing, and a closed
    // comment before the tag leaves it in code.
    ['const char *s = "/* SPDX-License-Identifier: MIT */";', 'src/s.c'],
    ['int x; /* note */ const char *s = "SPDX-License-Identifier: MIT";', 'src/t.c'],
  ]) {
    assert.ok(!header(l, rel), l);
  }
});

test('bundled code: a tag in a block comment after code is a header read from its comment, the same text in a string literal fails', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'inline-spdx-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted.');
    write('vendor/inline.c', 'int x; /* Copyright (c) 2019 Inline Holder. SPDX-License-Identifier: BSD-2-Clause */\n');
    const comp = () => ({ name: 'inline-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'inline-pkg@1.0.0') });
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    const bundled = findBundledCodeUnderOtherLicenses(comp(), known);
    assert.deepEqual(bundled.map((b) => [b.license, b.files]), [['BSD-2-Clause', ['vendor/inline.c']]]);
    // Read from its comment: the code before the opener is no part of the header.
    assert.deepEqual(bundled[0].notices, ['Copyright (c) 2019 Inline Holder. SPDX-License-Identifier: BSD-2-Clause']);
    write('vendor/inline.c', 'const char *s = "/* SPDX-License-Identifier: BSD-2-Clause */";\n');
    assert.throws(() => findBundledCodeUnderOtherLicenses(comp(), known), /inline-pkg@1\.0\.0 has an SPDX-License-Identifier in vendor\/inline\.c:1 outside a comment the scanner knows/);
    // An unstarred multiline C block comment with text before the SPDX tag is
    // inside the tracked block and accepted; closing the block before the tag
    // leaves the tag outside the comment and fails.
    write('vendor/inline.c', '/*\nCopyright (c) 2020 Unstarred Holder\nLicense: SPDX-License-Identifier: BSD-2-Clause\n*/\nint u;\n');
    const unstarred = findBundledCodeUnderOtherLicenses(comp(), known);
    assert.deepEqual(unstarred.map((b) => [b.license, b.files]), [['BSD-2-Clause', ['vendor/inline.c']]]);
    assert.deepEqual(unstarred[0].notices, ['Copyright (c) 2020 Unstarred Holder']);
    write('vendor/inline.c', '/*\nCopyright (c) 2020 Closed Holder\n*/ License: SPDX-License-Identifier: BSD-2-Clause\nint u;\n');
    assert.throws(() => findBundledCodeUnderOtherLicenses(comp(), known), /inline-pkg@1\.0\.0 has an SPDX-License-Identifier in vendor\/inline\.c:3 outside a comment the scanner knows/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a header ends its backward search at the opener of its own comment, also when the previous comment closes on that line', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'adjacent-blocks-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted.');
    const comp = () => ({ name: 'adjacent-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'adjacent-pkg@1.0.0') });
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    // The first comment closes and the second opens on the same line: only
    // the second declares BSD-2-Clause, so only its holder is listed.
    write('vendor/adjacent.c', '/*\n * Copyright (c) 2018 First Holder\n */ /*\n * Copyright (c) 2019 Second Holder\n * SPDX-License-Identifier: BSD-2-Clause\n */\nint x;\n');
    let bundled = findBundledCodeUnderOtherLicenses(comp(), known);
    assert.deepEqual(bundled.map((b) => [b.license, b.files]), [['BSD-2-Clause', ['vendor/adjacent.c']]]);
    assert.ok(bundled[0].notices.some((n) => n.includes('Second Holder')), bundled[0].notices.join('|'));
    assert.ok(!bundled[0].notices.some((n) => n.includes('First Holder')), bundled[0].notices.join('|'));
    // The same two comments on separate lines: the same result.
    write('vendor/adjacent.c', '/*\n * Copyright (c) 2018 First Holder\n */\n/*\n * Copyright (c) 2019 Second Holder\n * SPDX-License-Identifier: BSD-2-Clause\n */\nint x;\n');
    bundled = findBundledCodeUnderOtherLicenses(comp(), known);
    assert.ok(bundled[0].notices.some((n) => n.includes('Second Holder')) && !bundled[0].notices.some((n) => n.includes('First Holder')), bundled[0].notices.join('|'));
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a header expression made of SPDX tokens that does not parse fails, a mention in prose or code does not', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'malformed-spdx-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted.');
    // Mentions measured on the real tree: a README, a string literal, a shell check.
    write('README.md', 'Each file starts with `SPDX-License-Identifier: MIT` at the top.\n');
    write('src/html.rs', 'const S: &str = "// SPDX-License-Identifier: Apache-2.0\\n// SPDX-License-Identifier: MIT\\n\\nwindow.x = 1";\n');
    write('ci/check.sh', "grep -q 'SPDX-License-Identifier: BSD-2-Clause OR Apache-2.0 OR MIT' \\\n");
    write('docs/notes.md', 'Documentation: SPDX-License-Identifier: MIT (preferred)\n');
    write('lib/arm.S', '@ SPDX-License-Identifier: BSD-2-Clause AND MIT\n');
    write('lib/one.c', '/* Copyright 2020 Foo Ltd. SPDX-License-Identifier: BSD-2-Clause */\n');
    write('docs/list.md', '- SPDX-License-Identifier: MIT OR\n');
    write('lib/valid.c', '/* SPDX-License-Identifier: BSD-2-Clause AND MIT */\n');
    const comp = { name: 'malformed-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'malformed-pkg@1.0.0') };
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(comp, known).filter((b) => !b.declared).map((b) => [b.license, b.files]), [['BSD-2-Clause', ['lib/arm.S', 'lib/one.c', 'lib/valid.c']]]);
    write('lib/broken.c', '/* SPDX-License-Identifier: BSD-2-Clause AND */\n');
    assert.throws(
      () => findBundledCodeUnderOtherLicenses(comp, known),
      /malformed-pkg@1\.0\.0 has a malformed SPDX-License-Identifier in lib\/broken\.c:1\./
    );
    fs.rmSync(path.join(tmpDir, 'lib/broken.c'));
    // A malformed declaration containing a credential-bearing URL reports only
    // package, file, and line number without leaking the secret into logs.
    write('lib/secret.c', '/* SPDX-License-Identifier: https://user:s3cret-cred@git.example/repo */\n');
    assert.throws(
      () => findBundledCodeUnderOtherLicenses(comp, known),
      (err) => {
        assert.match(err.message, /malformed-pkg@1\.0\.0 has a malformed SPDX-License-Identifier in lib\/secret\.c:1\./);
        assert.doesNotMatch(err.message, /s3cret-cred/);
        return true;
      }
    );
    fs.rmSync(path.join(tmpDir, 'lib/secret.c'));
    // An empty SPDX-License-Identifier in declaration position fails, while a
    // prose mention with no value in docs is ignored.
    write('lib/empty.c', '// SPDX-License-Identifier:\n');
    assert.throws(
      () => findBundledCodeUnderOtherLicenses(comp, known),
      /malformed-pkg@1\.0\.0 has a malformed SPDX-License-Identifier in lib\/empty\.c:1\./
    );
    fs.rmSync(path.join(tmpDir, 'lib/empty.c'));
    write('docs/empty-prose.md', 'Documentation: SPDX-License-Identifier:\n');
    assert.deepStrictEqual(
      findBundledCodeUnderOtherLicenses(comp, known).filter((b) => !b.declared).map((b) => [b.license, b.files]),
      [['BSD-2-Clause', ['lib/arm.S', 'lib/one.c', 'lib/valid.c']]]
    );
    fs.rmSync(path.join(tmpDir, 'docs/empty-prose.md'));
    // A declaration in a header position that the tokenizer cannot read (an
    // SPDX document reference) fails too, never passes unchecked; the string
    // literal and the shell check above stay mentions.
    write('lib/vendor.c', '/* SPDX-License-Identifier: DocumentRef-Vendor:LicenseRef-Custom */\n');
    assert.throws(() => findBundledCodeUnderOtherLicenses(comp, known), /malformed-pkg@1\.0\.0 has a malformed SPDX-License-Identifier in lib\/vendor\.c:1\./);
    fs.rmSync(path.join(tmpDir, 'lib/vendor.c'));
    // A comment that mentions the tag fails like a malformed header: the remedy
    // is a reviewed entry, never a guess from the words before the tag.
    write('lib/example.js', '// Example: SPDX-License-Identifier: MIT (preferred)\n');
    assert.throws(() => findBundledCodeUnderOtherLicenses(comp, known), /in lib\/example\.js:1\./);
    const listed = { 'malformed-pkg@1.0.0:lib/example.js:// Example: SPDX-License-Identifier: MIT (preferred)': 'a usage example in a comment, not a header' };
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(comp, known, listed).filter((b) => !b.declared).map((b) => [b.license, b.files]), [['BSD-2-Clause', ['lib/arm.S', 'lib/one.c', 'lib/valid.c']]]);
    // Only the listed file is skipped: the same line elsewhere still fails.
    write('lib/other.js', '// Example: SPDX-License-Identifier: MIT (preferred)\n');
    assert.throws(() => findBundledCodeUnderOtherLicenses(comp, known, listed), /in lib\/other\.js:1\./);
    fs.rmSync(path.join(tmpDir, 'lib/other.js'));
    // The entry names one line: a malformed header in the same file still fails.
    write('lib/example.js', '// Example: SPDX-License-Identifier: MIT (preferred)\n/* SPDX-License-Identifier: BSD-2-Clause AND */\n');
    assert.throws(() => findBundledCodeUnderOtherLicenses(comp, known, listed), /in lib\/example\.js:2\./);
    write('lib/example.js', '// Example: SPDX-License-Identifier: MIT (preferred)\n');
    // And one version: the same line in another version of the package fails.
    assert.throws(() => findBundledCodeUnderOtherLicenses({ ...comp, version: '2.0.0' }, known, listed), /malformed-pkg@2\.0\.0 has a malformed SPDX-License-Identifier in lib\/example\.js:1\./);
    // A reviewed mention with > MAX_SPDX_ALTERNATIVES distinct choices is
    // skipped before expansion, while an unreviewed header above the cap fails.
    const hugeExpr = Array.from({ length: 11 }, (_, idx) => `(LicenseA-${idx} OR LicenseB-${idx})`).join(' AND ');
    const mentionLine = `// Example: SPDX-License-Identifier: ${hugeExpr}`;
    write('lib/huge_mention.c', `${mentionLine}\n`);
    const hugeMentionMap = { [`malformed-pkg@1.0.0:lib/huge_mention.c:${mentionLine}`]: 'reviewed test mention' };
    assert.deepStrictEqual(
      findBundledCodeUnderOtherLicenses(comp, known, { ...listed, ...hugeMentionMap }).filter((b) => !b.declared).map((b) => [b.license, b.files]),
      [['BSD-2-Clause', ['lib/arm.S', 'lib/one.c', 'lib/valid.c']]]
    );
    assert.throws(
      () => findBundledCodeUnderOtherLicenses(comp, known, listed),
      new RegExp(`exceeds ${MAX_SPDX_ALTERNATIVES} distinct alternatives`),
      'without the review record the same tokenizable mention must hit the expansion cap'
    );
    fs.rmSync(path.join(tmpDir, 'lib/huge_mention.c'));
    write('lib/huge_header.c', `// SPDX-License-Identifier: ${hugeExpr}\n`);
    assert.throws(
      () => findBundledCodeUnderOtherLicenses(comp, known, listed),
      new RegExp(`exceeds ${MAX_SPDX_ALTERNATIVES} distinct alternatives`)
    );
    fs.rmSync(path.join(tmpDir, 'lib/huge_header.c'));
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: nested Rust block comments close at outer boundary and keep same-line permission text', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'rust-nested-comment-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', shippedLicense('Copyright (c) 2020 Wrapper Author', 'MIT'));
    // Outer Rust block closes after an inner closer on the same line, followed by a real // SPDX header.
    write('src/after_outer.rs', '/* outer\n/* inner */ still outer */ // SPDX-License-Identifier: BSD-2-Clause\npub fn a() {}\n');
    // Same-line permission text after an inner /* Aside */ closer inside a Rust block comment.
    write('src/same_line_grant.rs', '/*\n * Copyright (c) 2021 Same Line Holder\n * SPDX-License-Identifier: BSD-2-Clause\n * /* Aside */ Permission is hereby granted to use this file.\n */\npub fn b() {}\n');
    // Next-line permission text after an inner comment (existing behavior).
    write('src/next_line_grant.rs', '/*\n * Copyright (c) 2021 Next Line Holder\n * SPDX-License-Identifier: BSD-2-Clause\n * /* Aside */\n * Permission is hereby granted to use this next file.\n */\npub fn c() {}\n');
    // C control: C block comments do not nest, so text after the first */ is outside the comment.
    write('src/c_control.c', '/* Copyright (c) 2021 C Holder. SPDX-License-Identifier: BSD-2-Clause */ int x;\n');
    // Inner Rust comment delimiters are stripped from attribution lines without
    // truncating text after them: both a reviewed continuation ending at `*/`
    // and a notice after an inner `/* ... */` work.
    const wrapped = reviewedNotices().find((r) => !r.example && !r.citation && r.following.length === 1);
    write(
      'src/inner_notice.rs',
      `/*\n * SPDX-License-Identifier: BSD-2-Clause\n * /* ${wrapped.line}\n * ${wrapped.following[0]} */\n * /* Note */ Copyright (c) 2022 After Inner Holder\n */\npub fn d() {}\n`
    );
    const comp = { name: 'rust-nested-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'rust-nested-pkg@1.0.0') };
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    const bsd = findBundledCodeUnderOtherLicenses(comp, known).find((b) => b.license === 'BSD-2-Clause');
    assert.ok(bsd);
    assert.deepStrictEqual(bsd.files, ['src/after_outer.rs', 'src/c_control.c', 'src/inner_notice.rs', 'src/next_line_grant.rs', 'src/same_line_grant.rs']);
    assert.ok(bsd.notices.includes(wrapped.notice), JSON.stringify(bsd.notices));
    assert.ok(bsd.notices.some((n) => n.includes('Copyright (c) 2022 After Inner Holder') && !n.includes('*/')), JSON.stringify(bsd.notices));
    assert.ok(bsd.headers.some((h) => h.files.includes('src/same_line_grant.rs') && h.text.includes('Permission is hereby granted to use this file.')), JSON.stringify(bsd.headers));
    assert.ok(bsd.headers.some((h) => h.files.includes('src/next_line_grant.rs') && h.text.includes('Permission is hereby granted to use this next file.')), JSON.stringify(bsd.headers));
    assert.ok(!bsd.headers.some((h) => h.files.includes('src/c_control.c') && h.text.includes('int x;')));
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('readPackageFiles: a source file named after a license word at the root is code, the standard text is used', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'root-source-license-fixture-'));
  try {
    fs.writeFileSync(path.join(tmpDir, 'license.js'), 'module.exports = function license() { return "MIT"; };\n');
    fs.writeFileSync(path.join(tmpDir, 'copyright.rs'), '// copyright helpers\n');
    fs.writeFileSync(path.join(tmpDir, 'NOTICE.html'), '<p>This product includes software developed by Example Project.</p>\n');
    fs.writeFileSync(path.join(tmpDir, 'LICENSE.Apache_2.0'), 'Apache License\nVersion 2.0, January 2004');
    fs.mkdirSync(path.join(tmpDir, 'LICENSES'));
    fs.writeFileSync(path.join(tmpDir, 'LICENSES', 'MIT.txt'), 'MIT License\n\nPermission is hereby granted.');
    fs.writeFileSync(path.join(tmpDir, 'LICENSES', 'build.rs'), 'fn main() {}\n');
    fs.writeFileSync(path.join(tmpDir, 'LICENSES', 'README.md'), 'The license texts of this package.\n');
    const files = readPackageFiles(tmpDir, 'root-source@1.0.0');
    assert.deepStrictEqual(files.licenseFiles.map((f) => f.filename), ['LICENSE.Apache_2.0', 'LICENSES/MIT.txt']);
    // An HTML notice document is a notice, reproduced whole.
    assert.deepStrictEqual(files.noticeFiles.map((f) => f.filename), ['NOTICE.html']);
    fs.rmSync(path.join(tmpDir, 'NOTICE.html'));
    fs.rmSync(path.join(tmpDir, 'LICENSE.Apache_2.0'));
    fs.rmSync(path.join(tmpDir, 'LICENSES'), { recursive: true });
    const only = readPackageFiles(tmpDir, 'test-root-source@1.0.0');
    assert.deepStrictEqual([...only.licenseFiles, ...only.noticeFiles], []);
    const comp = { name: 'test-root-source', version: '1.0.0', license: 'MIT', source: 'npm', packageDir: tmpDir, authors: ['Root Author'], ...only };
    const md = generateNoticesMarkdown([comp], []);
    assert.match(md, /### Standard License: MIT\n/);
    assert.doesNotMatch(md, /file: `license\.js`/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('bundled code: a leftover placeholder in the reproduced text fails', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bundled-placeholder-fixture-'));
  try {
    fs.writeFileSync(path.join(tmpDir, 'LICENSE'), 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted.');
    fs.writeFileSync(path.join(tmpDir, 'a.c'), '/* Copyright (c) 2001 A Holder\n * SPDX-License-Identifier: ISC */\n');
    const comp = { name: 'ph-pkg', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'ph-pkg@1.0.0') };
    const standard = JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'));
    const lic = path.join(tmpDir, 'licenses.json');
    fs.writeFileSync(lic, JSON.stringify({ ...standard, ISC: 'ISC License\n\nCopyright <author>\n\nPermission to use.' }));
    assert.throws(() => generateNoticesMarkdown([comp], [], lic), /Standard license text for ISC contains unpopulated placeholders/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('SPDX document: a NOASSERTION supplier names no author, so the reviewed attribution is required', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'spdx-noassertion-fixture-'));
  try {
    fs.writeFileSync(path.join(tmpDir, 'LICENSE.spdx'), 'SPDXVersion: SPDX-2.1\nPackageName: noassert\nPackageSupplier: NOASSERTION\nPackageLicenseDeclared: MIT\nPackageCopyrightText: NOASSERTION\n');
    const comp = { name: 'noassert-pkg', version: '1.0.0', license: 'MIT', source: 'npm', packageDir: tmpDir, authors: [], ...readPackageFiles(tmpDir, 'noassert-pkg@1.0.0') };
    assert.throws(() => generateNoticesMarkdown([comp], []), /Component noassert-pkg@1\.0\.0 has no manifest authors and is missing reviewed attribution in PINNED_FALLBACK_ATTRIBUTIONS/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('readPackageFiles: a declared license_file in a subfolder is read once, as the package license', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'nested-license-file-fixture-'));
  try {
    fs.mkdirSync(path.join(tmpDir, 'legal'));
    fs.writeFileSync(path.join(tmpDir, 'legal', 'LICENSE.txt'), 'Custom terms');
    fs.mkdirSync(path.join(tmpDir, 'third_party'));
    fs.writeFileSync(path.join(tmpDir, 'third_party', 'LICENSE'), 'Vendored terms');
    const result = readPackageFiles(tmpDir, 'nested-crate@1.0.0', 'legal/LICENSE.txt');
    assert.deepStrictEqual(
      result.licenseFiles.map((f) => [f.filename, f.content, Boolean(f.vendored)]),
      [
        ['legal/LICENSE.txt', 'Custom terms', false],
        ['third_party/LICENSE', 'Vendored terms', true],
      ]
    );
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('findPackageCopyrightNotices: a notice inside a C string ends at the string', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'c-string-notice-fixture-'));
  try {
    fs.mkdirSync(path.join(tmpDir, 'src'));
    fs.writeFileSync(path.join(tmpDir, 'src', 'ver.hpp'), '#define MVersion L"\\nUNRAR Copyright (c) 1993-2024 Alexander Roshal\\n"\n');
    assert.deepStrictEqual(
      findPackageCopyrightNotices(tmpDir, []).map((n) => n.notice),
      ['Copyright (c) 1993-2024 Alexander Roshal']
    );
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('standard SPDX texts: preserves mandatory AND conjuncts even when expression contains OR', () => {
  const dummyComponent = {
    name: 'mixed-and-or-crate',
    version: '1.0.0',
    license: '(MIT OR Apache-2.0) AND MISSING-SPDX-CONJUNCT',
    source: 'cargo',
    licenseFiles: [],
    noticeFiles: [],
    authors: ['Test Author'],
  };

  assert.throws(
    () => generateNoticesMarkdown([dummyComponent], []),
    /requires standard license text for 'MISSING-SPDX-CONJUNCT'/
  );
});

test('standard SPDX texts: an empty entry of the table fails by name before a bundled header resolves to it', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'empty-standard-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('pkg/LICENSE', shippedLicense('MIT License\n\nCopyright (c) 2020 Wrapper Author', 'MIT'));
    write('pkg/src/x.c', '/*\n * Copyright (c) 2021 Bundled Holder\n * SPDX-License-Identifier: BSD-2-Clause\n */\nint x;\n');
    const comp = { name: 'empty-std', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: path.join(tmpDir, 'pkg'), authors: [], ...readPackageFiles(path.join(tmpDir, 'pkg'), 'empty-std@1.0.0') };
    // The complete table: the header gets the BSD-2-Clause text.
    const md = generateNoticesMarkdown([comp], []);
    assert.match(md, /Copyright \(c\) 2021 Bundled Holder/);
    assert.match(md, /Redistribution and use in source and binary forms/);
    for (const empty of ['', '   \n', 42]) {
      write('standard.json', JSON.stringify({ ...STANDARD_TEXTS, 'BSD-2-Clause': empty }));
      assert.throws(() => generateNoticesMarkdown([comp], [], path.join(tmpDir, 'standard.json')), /has an empty or invalid standard text for BSD-2-Clause/, JSON.stringify(empty));
    }
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('standard SPDX texts: fails fast if spdx-standard-licenses.json is missing when components need it', () => {
  const dummyComponent = {
    name: 'test-crate',
    version: '1.0.0',
    license: 'MIT',
    source: 'cargo',
    licenseFiles: [],
    noticeFiles: [],
  };

  assert.throws(
    () => generateNoticesMarkdown([dummyComponent], [], '/tmp/nonexistent-spdx-file.json'),
    /Standard SPDX licenses file is required but missing at \/tmp\/nonexistent-spdx-file\.json/
  );
});

test('CLI mode: exactly one of --check and --write, and both together are refused before anything is collected or written', () => {
  assert.strictEqual(parseMode(['--check']), 'check');
  assert.strictEqual(parseMode(['--write']), 'write');
  assert.throws(() => parseMode([]), /exactly one of them/);
  assert.throws(() => parseMode(['--check', '--write']), /exactly one of them/);
  // `npm run check:licenses -- --write`: the run fails and the committed file is untouched.
  const noticesFile = path.join(ROOT, 'THIRD-PARTY-NOTICES.md');
  const before = fs.readFileSync(noticesFile);
  const mtime = fs.statSync(noticesFile).mtimeMs;
  assert.throws(
    // The pinned tsx CLI run by this Node, as `npm run` does: an npx shim is
    // not an executable execFileSync can start on Windows.
    () => execFileSync(process.execPath, [createRequire(path.join(ROOT, 'package.json')).resolve('tsx/cli'), 'scripts/gen-third-party-notices.ts', '--check', '--write'], { cwd: ROOT, encoding: 'utf8', stdio: 'pipe' }),
    (err) => err.status === 1 && /exactly one of them/.test(String(err.stderr))
  );
  assert.ok(fs.readFileSync(noticesFile).equals(before));
  assert.strictEqual(fs.statSync(noticesFile).mtimeMs, mtime);
});

test('writing the notices: a failed write keeps the previous file whole and leaves no temporary file', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'notices-write-fixture-'));
  try {
    const target = path.join(tmpDir, 'THIRD-PARTY-NOTICES.md');
    fs.writeFileSync(target, 'previous notices\n');
    // A full disk: part of the content is written, then the write fails.
    const failing = (file, data) => {
      fs.writeFileSync(file, data.slice(0, 3));
      throw Object.assign(new Error('ENOSPC: no space left on device'), { code: 'ENOSPC' });
    };
    assert.throws(() => writeNoticesFile(target, 'new notices\n', failing), /ENOSPC/);
    assert.strictEqual(fs.readFileSync(target, 'utf8'), 'previous notices\n');
    assert.deepStrictEqual(fs.readdirSync(tmpDir), ['THIRD-PARTY-NOTICES.md']);
    // A write that succeeds replaces the file and leaves only it.
    writeNoticesFile(target, 'new notices\n');
    assert.strictEqual(fs.readFileSync(target, 'utf8'), 'new notices\n');
    assert.deepStrictEqual(fs.readdirSync(tmpDir), ['THIRD-PARTY-NOTICES.md']);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('THIRD-PARTY-NOTICES.md and the exception pages exist and are up to date with the lockfiles', () => {
  // One in-process generation from the suite's inventory; the CLI (--check,
  // its exit code and its diff) runs as npm run check:licenses before this
  // suite in CI.
  assert.strictEqual(fs.existsSync(path.join(ROOT, 'THIRD-PARTY-NOTICES.md')), true, 'THIRD-PARTY-NOTICES.md must exist in repo root');
  const { content } = committedNoticesWithVerbatim();
  assert.match(content, new RegExp(`- \\*\\*Rust \\(Cargo\\) Production Crates:\\*\\* ${realTree().cargo.length}\\n`));
  assert.match(content, new RegExp(`- \\*\\*Node\\.js / Web \\(npm\\) Production Packages:\\*\\* ${realTree().npm.length}\\n`));
});

test('check:licenses drift reporting: nonzero with a line location, never notice contents', () => {
  assert.strictEqual(firstDifferingLine('line 1\nline 2\n', 'line 1\nchanged\n'), 2);
  assert.strictEqual(firstDifferingLine('same\n', 'same\n'), null);
  const file = path.join(ROOT, 'docs/UNRAR-LICENSE.md');
  const before = fs.readFileSync(file);
  const dummyCredential = 'dummy-credential-for-ci-log-test';
  const tsxCli = createRequire(path.join(ROOT, 'package.json')).resolve('tsx/cli');
  try {
    fs.writeFileSync(file, `${before.toString('utf8')}\n${dummyCredential}\n`);
    const run = spawnSync(process.execPath, [tsxCli, 'scripts/gen-third-party-notices.ts', '--check'], {
      cwd: ROOT,
      encoding: 'utf8',
      stdio: 'pipe',
    });
    const output = `${run.stdout}${run.stderr}`;
    assert.notStrictEqual(run.status, 0, output);
    assert.match(output, /First difference at line \d+\./);
    assert.ok(!output.includes(dummyCredential), 'drift diagnostics must not reproduce notice contents');
  } finally {
    fs.writeFileSync(file, before);
  }
});

test('determinism: shuffled dependencies and CRLF/LF line endings produce identical markdown', () => {
  const compA = {
    name: 'alpha-crate',
    version: '1.0.0',
    license: 'MIT',
    source: 'cargo',
    licenseFiles: [{ filename: 'LICENSE', content: 'Copyright (c) 2024 Alpha\r\nPermission is hereby granted...' }],
    noticeFiles: [],
    authors: ['Alpha Author'],
  };
  const compB = {
    name: 'beta-crate',
    version: '2.0.0',
    license: 'MIT',
    source: 'cargo',
    licenseFiles: [{ filename: 'LICENSE', content: 'Copyright (c) 2024 Beta\nPermission is hereby granted...' }],
    noticeFiles: [],
    authors: ['Beta Author'],
  };
  const out1 = generateNoticesMarkdown([compA, compB], [], SPDX_STANDARD_LICENSES_FILE, []);
  const out2 = generateNoticesMarkdown([compB, compA], [], SPDX_STANDARD_LICENSES_FILE, []);
  assert.strictEqual(out1, out2);
  assert.strictEqual(out1.includes('\r'), false);
});

test('licensing exceptions: generates exception section for unrar_sys with Roshal attribution and facts', () => {
  const noticesFile = path.join(ROOT, 'THIRD-PARTY-NOTICES.md');
  const content = fs.readFileSync(noticesFile, 'utf8');

  assert.match(content, /## Licensing Exceptions/);
  assert.match(content, /UnRAR \(`unrar_sys`\)/);
  assert.match(content, /Alexander Roshal/);
  assert.match(content, /re-create the RAR compression algorithm/);
  assert.match(content, /only extracts RAR archives and never creates them/);
  assert.match(content, /not GPL-compatible/);
  assert.match(content, /\[Notice Entry \d+\]\(#exception-unrar-sys\)/);
});

test('licensing exceptions: each exception gets a standalone page with the text of its notice entry', () => {
  const nonFreeText = 'Nonfree terms, line one.\nThis library cannot be used to develop RAR software.\n';
  const comp = {
    name: 'nonfree-crate',
    version: '1.0.0',
    license: 'MIT',
    source: 'cargo',
    licenseFiles: [
      { filename: 'LICENSE', content: shippedLicense('MIT License\n\nCopyright (c) 2020 Nonfree Author', 'MIT') },
      { filename: 'vendor/x/license.txt', content: nonFreeText, vendored: true },
    ],
    noticeFiles: [],
    authors: ['Nonfree Author'],
  };
  const exception = { crateName: 'nonfree-crate', version: '1.0.0', author: 'Nonfree Author', licenseName: 'Nonfree License', sourceFile: 'vendor/x/license.txt', document: 'docs/NONFREE-LICENSE.md', facts: 'The nonfree facts.' };
  const { output, documents } = renderNotices([comp], [], SPDX_STANDARD_LICENSES_FILE, [exception]);
  assert.deepStrictEqual(documents.map((d) => d.file), ['docs/NONFREE-LICENSE.md']);
  const page = documents[0].content;
  assert.ok(page.startsWith('# Nonfree License\n'));
  assert.ok(page.includes('The nonfree facts.'));
  assert.ok(page.includes('`nonfree-crate@1.0.0` ships in `vendor/x/license.txt`'));
  assert.ok(page.includes('(../THIRD-PARTY-NOTICES.md?raw=1)'));
  assert.strictEqual(fencedBlockAfter(page, '# Nonfree License'), fencedBlockAfter(output, '<a id="exception-nonfree-crate"></a>'));
  assert.strictEqual(fencedBlockAfter(page, '# Nonfree License'), nonFreeText.replace(/\n$/, ''));
  // The backlink is computed from the page's own folder: a nested page and
  // one at the root reach the same notices.
  for (const [document, link] of [
    ['docs/licenses/NONFREE-LICENSE.md', '../../THIRD-PARTY-NOTICES.md'],
    ['NONFREE-LICENSE.md', 'THIRD-PARTY-NOTICES.md'],
  ]) {
    const nested = renderNotices([comp], [], SPDX_STANDARD_LICENSES_FILE, [{ ...exception, document }]).documents[0];
    assert.strictEqual(nested.file, document);
    assert.ok(nested.content.includes(`(${link}?raw=1)`), document);
    assert.strictEqual(path.posix.normalize(path.posix.join(path.posix.dirname(document), link)), 'THIRD-PARTY-NOTICES.md');
  }
  assert.strictEqual(noticesLinkFrom('docs/UNRAR-LICENSE.md'), '../THIRD-PARTY-NOTICES.md');
  // Without a registered exception there is no page.
  const plain = { ...comp, licenseFiles: [comp.licenseFiles[0]] };
  assert.deepStrictEqual(renderNotices([plain], [], SPDX_STANDARD_LICENSES_FILE, []).documents, []);
});

test('licensing exceptions: fails fast if declared exception crate is missing from tree', () => {
  const dummyInventory = Array.from({ length: 15 }, (_, i) => ({
    name: `crate-${i}`,
    version: '1.0.0',
    license: 'MIT',
    source: 'cargo',
    licenseFiles: [],
    noticeFiles: [],
    authors: ['Test Author'],
  }));
  assert.throws(
    () => generateNoticesMarkdown(dummyInventory, [], SPDX_STANDARD_LICENSES_FILE, KNOWN_NON_FREE_EXCEPTIONS),
    /Known non-free exception crate unrar_sys@0\.5\.8 is declared but missing from Cargo dependencies/
  );
});

test('licensing exceptions: fails fast if unlisted component has non-free restriction clauses', () => {
  const unlistedComp = {
    name: 'rogue-crate',
    version: '1.0.0',
    license: 'Custom',
    source: 'cargo',
    licenseFiles: [{ filename: 'LICENSE.txt', content: 'This library cannot be used to develop an archive or re-create RAR compression algorithm.' }],
    noticeFiles: [],
    authors: ['Rogue'],
  };
  assert.throws(
    () => generateNoticesMarkdown([unlistedComp], [], SPDX_STANDARD_LICENSES_FILE, []),
    /Component rogue-crate@1\.0\.0 .* contains non-free restriction clauses but is not registered in KNOWN_NON_FREE_EXCEPTIONS/
  );
});

test('licensing exceptions: fails fast on restriction clauses wrapped across line breaks', () => {
  const wrappedComp = {
    name: 'wrapped-clause-crate',
    version: '1.0.0',
    license: 'Custom',
    source: 'cargo',
    licenseFiles: [{ filename: 'LICENSE.txt', content: 'This library cannot be used to develop\nRAR software.' }],
    noticeFiles: [],
    authors: ['Wrapped Author'],
  };
  assert.throws(
    () => generateNoticesMarkdown([wrappedComp], [], SPDX_STANDARD_LICENSES_FILE, []),
    /Component wrapped-clause-crate@1\.0\.0 .* contains non-free restriction clauses but is not registered in KNOWN_NON_FREE_EXCEPTIONS/
  );
});

test('licensing exceptions: non-free exemption covers only the registered Cargo source and file', () => {
  const restricted = 'This library cannot be used to develop an archive or re-create RAR compression algorithm.';
  const exc = KNOWN_NON_FREE_EXCEPTIONS[0];
  const registeredCargo = {
    name: exc.crateName,
    version: exc.version,
    license: 'MIT',
    source: 'cargo',
    licenseFiles: [
      { filename: 'LICENSE', content: shippedLicense('Copyright (c) 2020 Cargo Holder', 'MIT') },
      { filename: exc.sourceFile, content: restricted },
    ],
    noticeFiles: [],
    authors: ['Cargo Author'],
  };
  // Control: the registered Cargo crate and exact sourceFile pass.
  assert.doesNotThrow(() => generateNoticesMarkdown([registeredCargo], [], SPDX_STANDARD_LICENSES_FILE, [exc]));
  // Another file in the same Cargo crate is still scanned and rejected.
  const extraFileCargo = {
    ...registeredCargo,
    licenseFiles: [...registeredCargo.licenseFiles, { filename: 'OTHER-LICENSE.txt', content: restricted }],
  };
  assert.throws(
    () => generateNoticesMarkdown([extraFileCargo], [], SPDX_STANDARD_LICENSES_FILE, [exc]),
    /Component unrar_sys@0\.5\.8 \(file: OTHER-LICENSE\.txt\) contains non-free restriction clauses/
  );
  // A separate npm component with the same name and version is NOT exempted, even with the same filename.
  const sameNameNpm = {
    name: exc.crateName,
    version: exc.version,
    license: 'MIT',
    source: 'npm',
    licenseFiles: [{ filename: exc.sourceFile, content: restricted }],
    noticeFiles: [],
    authors: ['Npm Author'],
  };
  assert.throws(
    () => generateNoticesMarkdown([registeredCargo], [sameNameNpm], SPDX_STANDARD_LICENSES_FILE, [exc]),
    /Component unrar_sys@0\.5\.8 \(file: vendor\/unrar\/license\.txt\) contains non-free restriction clauses/
  );
});

test('committed notices: a placeholder in a block quoted as shipped passes, one in a generated block or line fails', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'quoted-placeholder-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    const standard = JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'));
    write('LICENSE', standard['MIT'].replace(/Copyright \(c\) <year> <copyright holders>/, 'Copyright (c) 2020 Wrapper Author'));
    // An upstream header that keeps the template line and its own terms.
    write('lib/quoted.c', '/*\n * Copyright (c) <year> <copyright holders>\n * Permission to use, copy, modify, and distribute this software is granted.\n * SPDX-License-Identifier: ISC\n */\n');
    const comp = { name: 'quoted-placeholder', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'quoted-placeholder@1.0.0') };
    const { output, verbatimBlocks } = renderNotices([comp], []);
    assert.match(output, /<copyright holders>/, 'the upstream header is quoted as shipped');
    // The committed-file check, with the generator's verbatim set, passes.
    assert.doesNotThrow(() => assertNoGeneratedPlaceholders(output, verbatimBlocks));
    // A CRLF checkout of the same file: its quoted block no longer equals
    // the generator's LF block, so the raw text fails, and the text the
    // tests read (lf, as committedNoticesWithVerbatim) passes.
    const crlf = output.replace(/\n/g, '\r\n');
    assert.throws(() => assertNoGeneratedPlaceholders(crlf, verbatimBlocks), /unpopulated placeholders/);
    assert.doesNotThrow(() => assertNoGeneratedPlaceholders(lf(crlf), verbatimBlocks));
    // Mutations: the same placeholder in a generated fenced block (a filled-in
    // standard text) and in a holder line still fail.
    const inGeneratedBlock = `${output}\n\`\`\`text\nCopyright (c) <year> <copyright holders>\n\`\`\`\n`;
    assert.throws(() => assertNoGeneratedPlaceholders(inGeneratedBlock, verbatimBlocks), /unpopulated placeholders/);
    assert.throws(() => assertNoGeneratedPlaceholders(`${output}\n- Copyright (c) <copyright holders> (x)\n`, verbatimBlocks), /unpopulated placeholders/);
    assert.throws(() => assertNoGeneratedPlaceholders(`${output}\n- authors: see repository maintainers\n`, verbatimBlocks), /unpopulated placeholders/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('fallback placeholders: no <copyright holders> or unpopulated placeholders remain in output', () => {
  // A block quoted as shipped may keep an upstream placeholder; the text the
  // notices write may not, standard texts included. The generator's own set,
  // and the committed file with LF line ends (a CRLF checkout included).
  const { content, verbatimBlocks } = committedNoticesWithVerbatim();
  assertNoGeneratedPlaceholders(content, verbatimBlocks);
  // Verifies reviewed attribution is present for cx448
  assert.match(
    content,
    /`cx448@0\.1\.1` - SPDX: `BSD-3-Clause` \(authors: Friedel Ziegelmayer \(port to RustCrypto\), based on crate-crypto\/x448 by Kevaundray Wedderburn\)/
  );
  assert.match(
    content,
    /Copyright \(c\) 2016-2019 Henry de Valence \(cx448\)/
  );
  assert.doesNotMatch(
    content,
    /No copyright notice is shipped upstream; authors.*\(cx448\)/
  );
});

test('fallback placeholders: generator rejects unreviewed component without authors or pinned attribution', () => {
  const compWithoutAuthor = {
    name: 'unreviewed-authorless-crate',
    version: '0.0.1',
    license: 'BSD-3-Clause',
    source: 'cargo',
    licenseFiles: [],
    noticeFiles: [],
    authors: [],
  };
  assert.throws(
    () => generateNoticesMarkdown([compWithoutAuthor], [], SPDX_STANDARD_LICENSES_FILE, []),
    /Component unreviewed-authorless-crate@0\.0\.1 has no manifest authors and is missing reviewed attribution in PINNED_FALLBACK_ATTRIBUTIONS/
  );
});

test('fallback placeholders: generator rejects authored component without entry in PINNED_FALLBACK_ATTRIBUTIONS', () => {
  const compWithoutEntry = {
    name: 'authored-no-entry-crate',
    version: '1.0.0',
    license: 'BSD-3-Clause',
    source: 'cargo',
    licenseFiles: [],
    noticeFiles: [],
    authors: ['Valid Author <author@example.com>'],
  };
  assert.throws(
    () => generateNoticesMarkdown([compWithoutEntry], [], SPDX_STANDARD_LICENSES_FILE, []),
    /Component authored-no-entry-crate@1\.0\.0 requires an entry in PINNED_FALLBACK_ATTRIBUTIONS/
  );
});

test('build.yml includes THIRD-PARTY-NOTICES.md in Windows portable ZIP stage', () => {
  const buildYml = fs.readFileSync(path.join(ROOT, '.github', 'workflows', 'build.yml'), 'utf8');
  assert.match(buildYml, /Copy-Item "THIRD-PARTY-NOTICES\.md"\s+"\$stage\\THIRD-PARTY-NOTICES\.md"/);
  assert.match(buildYml, /Test-Path "\$stage\\THIRD-PARTY-NOTICES\.md"/);
});

test('THIRD-PARTY-NOTICES.md documents distribution targets and FlatPark distinction', () => {
  const noticesFile = path.join(ROOT, 'THIRD-PARTY-NOTICES.md');
  const content = fs.readFileSync(noticesFile, 'utf8');

  // Distribution targets listed
  assert.match(content, /deb, rpm, AppImage, snap/);
  assert.match(content, /DMG, MSI/);
  assert.match(content, /FlatPark repository/);
});

test('THIRD-PARTY-NOTICES.md contains expected critical crates and packages with license text', () => {
  const noticesFile = path.join(ROOT, 'THIRD-PARTY-NOTICES.md');
  const content = fs.readFileSync(noticesFile, 'utf8');

  // Cargo critical crates
  assert.match(content, /\| tauri \|/);
  assert.match(content, /\| russh \|/);
  assert.match(content, /\| reqwest \|/);
  assert.match(content, /\| webpki-roots \|.*\| CDLA-Permissive-2.0 \|/);

  // npm critical packages
  assert.match(content, /\| react \|/);
  assert.match(content, /\| react-virtuoso \|/);
  assert.match(content, /\| lucide-react \|/);

  // Multiple versions of npm packages preserved
  assert.match(content, /\| @types\/unist \| 3\.0\.3 \|/);
  assert.match(content, /\| @types\/unist \| 2\.0\.11 \|/);

  // Verbatim texts and notices section present
  assert.match(content, /## Third-Party License Texts, Copyright Notices, and Files/);
  assert.match(content, /### Notice Entry/);

  // Standard texts for the dependencies that need them (asserts source statement and NO ownership claim):
  // npm packages as well as crates, and crates that ship some license texts but not the selected one.
  assert.match(content, /## Standard License Texts for Dependencies that Need Them/);
  const standardSection = content.slice(content.indexOf('## Standard License Texts for Dependencies that Need Them'));
  assert.match(standardSection, /- `@tauri-apps\/plugin-dialog@[^`]+` - SPDX:/);
  assert.match(standardSection, /- `iroh-relay@1\.2\.0` - SPDX: `MIT OR Apache-2\.0`/);
  assert.doesNotMatch(content, /Components without Dedicated License Files|The following crates declare/);
  assert.match(content, /Authors for the following components as listed in package manifests or reviewed attributions:/);
  assert.doesNotMatch(committedNoticesWithVerbatim().generated, /The copyright holders for (?:the following|these) components are the authors/);

  // Exclusions (dev deps and unused optional crates like aws-lc-fips-sys)
  assert.doesNotMatch(content, /\| vitest \|/);
  assert.doesNotMatch(content, /\| typescript \|/);
  assert.doesNotMatch(content, /\| aeroftp \|/);
  assert.doesNotMatch(content, /\| aws-lc-fips-sys \|/);
});

test('fallback attributions: every pinned entry declares a non-empty source and fail fast if missing', () => {
  // No fixed count: a new dependency that needs a reviewed attribution adds
  // an entry, and each entry is checked for its source.
  const keys = Object.keys(PINNED_FALLBACK_ATTRIBUTIONS);
  assert.ok(keys.length > 0, 'pinned fallback attributions are loaded');
  for (const key of keys) {
    const entry = PINNED_FALLBACK_ATTRIBUTIONS[key];
    assert.ok(
      entry && typeof entry.source === 'string' && entry.source.trim().length > 0,
      `Pinned entry ${key} must declare a non-empty source`
    );
  }

  // Verify that an empty source throws fail-fast error
  const comp = {
    name: 'test-no-source-crate',
    version: '1.0.0',
    license: 'BSD-3-Clause',
    source: 'cargo',
    licenseFiles: [],
    noticeFiles: [],
    authors: ['Some Author'],
  };
  const original = PINNED_FALLBACK_ATTRIBUTIONS['test-no-source-crate@1.0.0'];
  try {
    PINNED_FALLBACK_ATTRIBUTIONS['test-no-source-crate@1.0.0'] = {
      author: 'Some Author',
      source: '',
    };
    assert.throws(
      () => generateNoticesMarkdown([comp], [], SPDX_STANDARD_LICENSES_FILE, []),
      /Entry test-no-source-crate@1\.0\.0 in PINNED_FALLBACK_ATTRIBUTIONS must have a non-empty 'source' field/
    );
  } finally {
    if (original) {
      PINNED_FALLBACK_ATTRIBUTIONS['test-no-source-crate@1.0.0'] = original;
    } else {
      delete PINNED_FALLBACK_ATTRIBUTIONS['test-no-source-crate@1.0.0'];
    }
  }
});

// The fenced blocks of a Markdown file as GitHub reads them: a fence of three
// or more backticks or tildes, indented up to three spaces, closed by a fence
// of the same character at least as long (including on the very next line for
// an empty fenced block), or by the end of the file.
const FENCED_BLOCK = /(?:^|\n) {0,3}(([`~])\2{2,})[^\n]*\n(?: {0,3}\1\2*[ \t]*(?=\n|$)|[\s\S]*?\n {0,3}\1\2*[ \t]*(?=\n|$)|[\s\S]*$)/g;
// One line ending: a CRLF checkout reads as the LF one.
// The ids an anchor tag gives outside fenced text: a license text that quotes
// `<a id="x"></a>` in a code block is no destination.
const anchorIdsOutsideFences = (md) => new Set(Array.from(lf(md).replace(FENCED_BLOCK, '\n').matchAll(/<a id="([^"]+)"><\/a>/g), (m) => m[1]));
// The links of a Markdown file outside fenced text, by target.
const linksOutsideFences = (md) => Array.from(lf(md).replace(FENCED_BLOCK, '\n').matchAll(/\[[^\]]+\]\(([^)\s]+)\)/g), (m) => m[1]);
// The first fenced block after an anchor, its text without the fence.
const fencedBlockAfter = (input, anchorTag) => {
  const md = lf(input);
  const at = md.indexOf(anchorTag);
  assert.ok(at !== -1, `${anchorTag} must be present`);
  const m = /\n(`{3,})text\n([\s\S]*?)\n\1\n/.exec(md.slice(at));
  assert.ok(m, `a fenced text block must follow ${anchorTag}`);
  return m[2];
};

// The top-level conjuncts of a workflow `if:` expression, each without the
// parentheses around it, or null when an `||` at the top joins it (then no
// single conjunct is required). Quoted strings are no operators.
const topLevelConjuncts = (condition) => {
  let expr = condition.trim();
  const wrapped = /^\$\{\{([\s\S]*)\}\}$/.exec(expr);
  if (wrapped) expr = wrapped[1].trim();
  const unwrap = (e) => {
    for (;;) {
      e = e.trim();
      if (!e.startsWith('(') || !e.endsWith(')')) return e;
      let depth = 0;
      let closesAtEnd = true;
      for (let i = 0, q = null; i < e.length; i++) {
        if (q) {
          if (e[i] === q) q = null;
        } else if (e[i] === "'") q = "'";
        else if (e[i] === '(') depth++;
        else if (e[i] === ')' && --depth === 0 && i < e.length - 1) {
          closesAtEnd = false;
          break;
        }
      }
      if (!closesAtEnd) return e;
      e = e.slice(1, -1);
    }
  };
  expr = unwrap(expr);
  const parts = [];
  let depth = 0;
  let from = 0;
  for (let i = 0, q = null; i < expr.length; i++) {
    if (q) {
      if (expr[i] === q) q = null;
    } else if (expr[i] === "'") q = "'";
    else if (expr[i] === '(') depth++;
    else if (expr[i] === ')') depth--;
    else if (depth === 0 && expr.startsWith('||', i)) return null;
    else if (depth === 0 && expr.startsWith('&&', i)) {
      parts.push(unwrap(expr.slice(from, i)));
      from = i + 2;
      i++;
    }
  }
  parts.push(unwrap(expr.slice(from)));
  return parts;
};
// Whether a job's `if:` requires a needed job to succeed: the comparison is
// one of the top-level conjuncts, so no other term can run the job without it.
// Whether a workflow job runs `command` on every run and fails when it fails:
// a step running it with no `if:` and no `continue-on-error`, in a job with
// neither. A step that runs only on pull requests, or whose failure is
// ignored, lets the job succeed on a tag without the check.
const alwaysGates = (jobInput, command) => {
  const job = typeof jobInput === 'string' ? Object.values(YAML.parse(jobInput) ?? {})[0] : jobInput;
  if (!job || typeof job !== 'object') return false;
  if (Object.prototype.hasOwnProperty.call(job, 'if') || Object.prototype.hasOwnProperty.call(job, 'continue-on-error')) return false;
  if (!Array.isArray(job.steps)) return false;
  return job.steps.some(
    (step) =>
      step &&
      typeof step === 'object' &&
      typeof step.run === 'string' &&
      step.run.trim() === command &&
      !Object.prototype.hasOwnProperty.call(step, 'if') &&
      !Object.prototype.hasOwnProperty.call(step, 'continue-on-error')
  );
};
const requiresSuccessOf = (condition, need) => (topLevelConjuncts(condition) ?? []).some((c) => c.replace(/\s+/g, ' ') === `needs.${need}.result == 'success'`);

test('workflow conditions: a needed job must succeed only when its success is a top-level conjunct', () => {
  for (const ok of [
    "${{ !cancelled() && startsWith(github.ref, 'refs/tags/') && needs.linux-validate.result == 'success' }}",
    "${{ !cancelled() && needs.linux-validate.result == 'success' && (startsWith(github.ref, 'refs/tags/') || github.event_name == 'workflow_dispatch') }}",
    "${{ (needs.linux-validate.result == 'success') && startsWith(github.ref, 'refs/tags/') }}",
    "needs.linux-validate.result == 'success'",
    "${{ contains(github.ref, '||') && needs.linux-validate.result == 'success' }}",
  ]) {
    assert.strictEqual(requiresSuccessOf(ok, 'linux-validate'), true, ok);
  }
  // The same comparison under an `||`, a negation, or as part of another
  // comparison lets the job run without it.
  for (const bad of [
    "${{ needs.linux-validate.result == 'success' || github.ref_type == 'tag' }}",
    "${{ startsWith(github.ref, 'refs/tags/') && needs.linux-validate.result == 'success' || github.event_name == 'workflow_dispatch' }}",
    "${{ startsWith(github.ref, 'refs/tags/') && (needs.linux-validate.result == 'success' || always()) }}",
    "${{ !(needs.linux-validate.result == 'success') && startsWith(github.ref, 'refs/tags/') }}",
    "${{ !cancelled() && needs.linux-validate.result != 'failure' }}",
    "${{ !cancelled() && needs.linux-validate.result == 'success' == false }}",
    "${{ !cancelled() && needs.other-job.result == 'success' }}",
  ]) {
    assert.strictEqual(requiresSuccessOf(bad, 'linux-validate'), false, bad);
  }
});

test('release workflow: every publish job requires a job that checks the notices against the lockfiles', () => {
  const workflow = fs.readFileSync(path.join(ROOT, '.github/workflows/build.yml'), 'utf8');
  const parsed = YAML.parse(workflow);
  assert.ok(parsed && typeof parsed.jobs === 'object' && parsed.jobs !== null);
  const jobs = new Map(Object.entries(parsed.jobs));
  assert.ok(alwaysGates(jobs.get('linux-validate'), 'npm run check:licenses'), 'linux-validate runs npm run check:licenses on every run, and fails when it fails');
  // The same job text with the step skipped on a tag, or its failure
  // ignored, gates nothing.
  const job = (header, step) => `  linux-validate:\n    runs-on: ubuntu-22.04\n${header}    steps:\n      - name: Checkout\n        uses: actions/checkout@v5\n\n${step}\n      - name: Type check\n        run: npx tsc --noEmit\n`;
  const check = '      - name: Third-party license notices match the lockfiles\n        run: npm run check:licenses\n';
  assert.strictEqual(alwaysGates(job('', check), 'npm run check:licenses'), true);
  assert.strictEqual(alwaysGates(job('', '      - run: npm run check:licenses\n'), 'npm run check:licenses'), true);
  for (const [header, step] of [
    ['', check.replace('\n        run:', "\n        if: github.event_name == 'pull_request'\n        run:")],
    ['', check.replace('\n        run:', "\n        if : github.event_name == 'pull_request'\n        run:")],
    ['', `${check}        continue-on-error: true\n`],
    ['', `${check}        continue-on-error : true\n`],
    ['', "      - if: ${{ !startsWith(github.ref, 'refs/tags/') }}\n        run: npm run check:licenses\n"],
    ['', "      - if : ${{ !startsWith(github.ref, 'refs/tags/') }}\n        run: npm run check:licenses\n"],
    ['', '      - name: Third-party license notices match the lockfiles\n        run: npm run check:licenses || true\n'],
    // A script that only writes the line, in a heredoc, runs nothing.
    ['', "      - name: Write a later step\n        run: |\n          cat >> steps.yml <<'EOF'\n          run: npm run check:licenses\n          EOF\n"],
    ['', "      - name: Write a later step\n        run: |\n          echo 'x'\n            run: npm run check:licenses\n"],
    ["    if: github.event_name == 'pull_request'\n", check],
    ["    if : github.event_name == 'pull_request'\n", check],
    ['    continue-on-error: true\n', check],
    ['    continue-on-error : true\n', check],
    ['', ''],
  ]) {
    assert.strictEqual(alwaysGates(job(header, step), 'npm run check:licenses'), false, `${header}${step}`);
  }
  // About's online fallback opens the notices at the tag of the app version
  // (src/utils/thirdPartyLicenses.ts): on a tag run the same job requires the
  // version sites to equal the tag, so that tag is the one whose notices the
  // check above verified. The step must actually run on tag events (no
  // continue-on-error, and either unconditional or gated on a tag condition).
  const versionCheckNeedle = 'node scripts/check-version-sites.mjs --tag "$GITHUB_REF_NAME"';
  const runsOnTag = (cond) => {
    if (cond === undefined) return true;
    if (typeof cond !== 'string') return false;
    const norm = cond.replace(/^\s*\$\{\{\s*|\s*\}\}\s*$/g, '').replace(/\s+/g, ' ').trim();
    return (
      norm === "startsWith(github.ref, 'refs/tags/')" ||
      norm === "startsWith(github.ref, 'refs/tags/v')" ||
      norm === "github.ref_type == 'tag'"
    );
  };
  const gatesTagRun = (jobInput, command) => {
    const j = typeof jobInput === 'string' ? Object.values(YAML.parse(jobInput) ?? {})[0] : jobInput;
    if (!j || typeof j !== 'object') return false;
    if (Object.prototype.hasOwnProperty.call(j, 'continue-on-error') || !runsOnTag(j.if)) return false;
    return Array.isArray(j.steps) && j.steps.some(
      (s) =>
        s &&
        typeof s === 'object' &&
        typeof s.run === 'string' &&
        s.run.trim() === command &&
        !Object.prototype.hasOwnProperty.call(s, 'continue-on-error') &&
        runsOnTag(s.if)
    );
  };
  assert.ok(gatesTagRun(jobs.get('linux-validate'), versionCheckNeedle), 'linux-validate executes check-version-sites on tag runs without continue-on-error');
  const tagStep = `      - name: Version sites match tag\n        if: \${{ startsWith(github.ref, 'refs/tags/v') }}\n        run: ${versionCheckNeedle}\n`;
  assert.strictEqual(gatesTagRun(job('', tagStep), versionCheckNeedle), true);
  assert.strictEqual(
    gatesTagRun(job('', `      - name: Version sites match tag\n        if: github.event_name == 'pull_request'\n        run: ${versionCheckNeedle}\n`), versionCheckNeedle),
    false,
    'a PR-only version check does not gate tag releases'
  );
  assert.strictEqual(
    gatesTagRun(job('', `${tagStep}        continue-on-error: true\n`), versionCheckNeedle),
    false,
    'a continue-on-error version check does not gate tag releases'
  );
  assert.strictEqual(
    gatesTagRun(
      job('', `      - name: Version sites match tag\n        if: \${{ startsWith(github.ref, 'refs/tags/v') }}\n        run: |\n          # ${versionCheckNeedle}\n          :\n`),
      versionCheckNeedle
    ),
    false,
    'a commented-out version check does not gate tag releases'
  );
  assert.strictEqual(
    gatesTagRun(
      job('', `      - name: Version sites match tag\n        if: \${{ startsWith(github.ref, 'refs/tags/v') }}\n        run: ${versionCheckNeedle} || true\n`),
      versionCheckNeedle
    ),
    false,
    'a version check whose failure is ignored does not gate tag releases'
  );
  const tsSource = fs.readFileSync(path.join(ROOT, 'src/utils/thirdPartyLicenses.ts'), 'utf8');
  assert.match(tsSource, /GITHUB_NOTICES_URL = `https:\/\/raw\.githubusercontent\.com\/axpdev-lab\/aeroftp\/v\$\{packageJson\.version\}\/THIRD-PARTY-NOTICES\.md`;/);
  // Publishing jobs: the ones that publish on a tag.
  const publishing = [...jobs.keys()].filter((id) => /^(?:publish-|build-snap$)/.test(id));
  assert.deepStrictEqual(publishing.sort(), ['build-snap', 'publish-flatpark', 'publish-github-release', 'publish-winget']);
  for (const id of publishing) {
    const j = jobs.get(id);
    const needs = Array.isArray(j?.needs) ? j.needs : typeof j?.needs === 'string' ? [j.needs] : [];
    assert.ok(needs.includes('linux-validate'), `${id} needs linux-validate`);
    // `if: !cancelled()` would run a job whose needs failed: the condition
    // must require linux-validate's success explicitly.
    const condition = typeof j?.if === 'string' ? j.if : '';
    assert.ok(condition && requiresSuccessOf(condition, 'linux-validate'), `${id} requires linux-validate to succeed: ${condition}`);
  }
});

test('anchor destinations: an anchor counts only outside fenced text, a quoted one does not', () => {
  const quotedOnly = 'Intro\n\n```text\nA license that quotes <a id="exception-x"></a> in its text.\n```\n';
  assert.strictEqual(anchorIdsOutsideFences(quotedOnly).has('exception-x'), false, 'a quoted anchor is no destination');
  const real = '<a id="exception-x"></a>\n\n### Notice Entry 1\n\n```text\nlicense text\n```\n';
  assert.strictEqual(anchorIdsOutsideFences(real).has('exception-x'), true);
  // A longer fence keeps a shorter one inside it as text.
  const nested = '````text\n```\n<a id="inner"></a>\n```\n````\n<a id="outer"></a>\n';
  assert.deepStrictEqual([...anchorIdsOutsideFences(nested)], ['outer']);
  // A link inside fenced text is no link.
  assert.deepStrictEqual(linksOutsideFences('```\n[x](THIRD-PARTY-NOTICES.md#a)\n```\n[y](docs/b.md)\n'), ['docs/b.md']);
  // Tildes fence as backticks do, a longer closer closes, an indented fence
  // is a fence, and an unclosed one runs to the end of the file.
  assert.deepStrictEqual(linksOutsideFences('~~~\n[x](docs/UNRAR-LICENSE.md)\n~~~\n[y](docs/b.md)\n'), ['docs/b.md']);
  assert.deepStrictEqual(linksOutsideFences('~~~text\n[x](docs/UNRAR-LICENSE.md)\n~~~~~\n[y](docs/b.md)\n'), ['docs/b.md']);
  assert.deepStrictEqual(linksOutsideFences('  ```\n[x](docs/UNRAR-LICENSE.md)\n  ```\n[y](docs/b.md)\n'), ['docs/b.md']);
  assert.deepStrictEqual(linksOutsideFences('~~~\n[x](docs/UNRAR-LICENSE.md)\n```\n[y](docs/b.md)\n'), [], 'a backtick line does not close a tilde fence');
  assert.deepStrictEqual(linksOutsideFences('[y](docs/b.md)\n```\n[x](docs/UNRAR-LICENSE.md)\n'), ['docs/b.md']);
  assert.deepStrictEqual([...anchorIdsOutsideFences('~~~\n<a id="inner"></a>\n~~~\n<a id="outer"></a>\n')], ['outer']);
  // A CRLF copy reads as the LF one: links, anchors and the fenced text.
  const page = '# Nonfree License\n\n```text\nthe terms\nline two\n```\n\n<a id="exception-x"></a>\n\n```text\nthe terms\nline two\n```\n\n[y](docs/b.md)\n';
  const crlf = page.replace(/\n/g, '\r\n');
  assert.deepStrictEqual(linksOutsideFences(crlf), linksOutsideFences(page));
  assert.deepStrictEqual([...anchorIdsOutsideFences(crlf)], [...anchorIdsOutsideFences(page)]);
  assert.strictEqual(fencedBlockAfter(crlf, '# Nonfree License'), 'the terms\nline two');
  assert.strictEqual(fencedBlockAfter(crlf, '<a id="exception-x"></a>'), fencedBlockAfter(page, '<a id="exception-x"></a>'));
});

test('README.md notice links resolve outside fenced text, and the UnRAR terms have a page GitHub displays', () => {
  // A CRLF checkout reads as the LF one (the generator's check does too).
  const readmeContent = lf(fs.readFileSync(path.join(ROOT, 'README.md'), 'utf8'));
  const noticesContent = lf(fs.readFileSync(path.join(ROOT, 'THIRD-PARTY-NOTICES.md'), 'utf8'));
  const destinations = anchorIdsOutsideFences(noticesContent);
  const links = linksOutsideFences(readmeContent);

  assert.ok(
    links.includes('THIRD-PARTY-NOTICES.md?raw=1'),
    'README.md must link to the raw full notices without requiring an anchor on an unrendered file'
  );

  // GitHub does not display the notices (over 2 MB): each licensing exception
  // has its own page, linked from the README, with the same text as the
  // packaged notice, whatever size the notices reach.
  for (const exc of KNOWN_NON_FREE_EXCEPTIONS) {
    assert.ok(links.includes(exc.document), `README.md must link to ${exc.document}`);
    const page = lf(fs.readFileSync(path.join(ROOT, exc.document), 'utf8'));
    assert.ok(page.length < 64 * 1024, `${exc.document} stays small enough for GitHub to display`);
    assert.ok(page.includes(`](${noticesLinkFrom(exc.document)}?raw=1)`), 'the standalone page links to readable raw notices');
    assert.ok(page.startsWith(`# ${exc.licenseName}\n`));
    const id = `exception-${exc.crateName.replace(/_/g, '-')}`;
    const anchorTag = `<a id="${id}"></a>`;
    assert.ok(destinations.has(id), `${anchorTag} must be outside fenced text`);
    assert.strictEqual(fencedBlockAfter(page, `# ${exc.licenseName}`), fencedBlockAfter(noticesContent, anchorTag), `${exc.document} must carry the packaged ${exc.licenseName} text`);
  }
  assert.ok(links.includes('docs/UNRAR-LICENSE.md'));
});

test('THIRD-PARTY-NOTICES.md: every anchor id in the generated file is unique', () => {
  // Read as a CRLF checkout would give it too: every check below runs on the
  // normalized text, the multi-line attribution included.
  const raw = fs.readFileSync(path.join(ROOT, 'THIRD-PARTY-NOTICES.md'), 'utf8');
  for (const noticesContent of [lf(raw), lf(lf(raw).replace(/\n/g, '\r\n'))]) {
    const proseOutsideCodeBlocks = noticesContent.replace(FENCED_BLOCK, '\n');
    const idMatches = Array.from(proseOutsideCodeBlocks.matchAll(/<a id="([^"]+)">/g)).map((m) => m[1]);
    assert.ok(idMatches.length > 0, 'Generated notices must contain anchors');

    const seenIds = new Set();
    const duplicates = [];
    for (const id of idMatches) {
      if (seenIds.has(id)) {
        duplicates.push(id);
      }
      seenIds.add(id);
    }

    assert.deepStrictEqual(
      duplicates,
      [],
      `Found duplicate anchor ids in THIRD-PARTY-NOTICES.md: ${duplicates.join(', ')}`
    );

    // Specifically check previously duplicated IDs are now unique
    assert.strictEqual(
      (proseOutsideCodeBlocks.match(/<a id="notice-unrar">/g) || []).length,
      1,
      'notice-unrar anchor must be emitted exactly once'
    );
    assert.strictEqual(
      (proseOutsideCodeBlocks.match(/<a id="notice-typeid">/g) || []).length,
      1,
      'notice-typeid anchor must be emitted exactly once'
    );
    assert.strictEqual(
      (proseOutsideCodeBlocks.match(/<a id="notice-wasm-streams">/g) || []).length,
      1,
      'notice-wasm-streams anchor must be emitted exactly once'
    );
    assert.strictEqual(
      (proseOutsideCodeBlocks.match(/<a id="notice-unrar-sys">/g) || []).length,
      1,
      'notice-unrar-sys anchor must be emitted exactly once'
    );

    // cesu8@1.1.0 attribution retained in standard license fallback alongside COPYRIGHT notice entry,
    // bounded strictly to each matching standard-license section
    const apacheSection = noticesContent.split('### Standard License: Apache-2.0')[1]?.split('### Standard License:')[0] || '';
    assert.ok(apacheSection.includes('`cesu8@1.1.0`'), 'cesu8@1.1.0 must appear in Apache-2.0 standard license section');

    const mitSection = noticesContent.split('### Standard License: MIT')[1]?.split('### Standard License:')[0] || '';
    assert.ok(mitSection.includes('`cesu8@1.1.0`'), 'cesu8@1.1.0 must appear in MIT standard license section');

    assert.match(noticesContent, /Eric Kidd <git@randomhacks\.net>/);
    // The holder line of its source header, with Eric Kidd and the 2012-2014 range, under both licenses.
    const cesu8Holder = '`cesu8@1.1.0` - SPDX: `Apache-2.0/MIT` (authors: Eric Kidd <git@randomhacks.net>)\n  - Copyright 2012-2014 The Rust Project Developers and Eric Kidd. (src/lib.rs)';
    assert.ok(apacheSection.includes(cesu8Holder), 'the cesu8 holder line must appear in the Apache-2.0 section');
    assert.ok(mitSection.includes(cesu8Holder), 'the cesu8 holder line must appear in the MIT section');
  }
});

test('groupLicenseTexts: COPYRIGHT-only component receives both notice entry and standard license fallback', () => {
  const comp = {
    name: 'copyright-only-pkg',
    version: '1.0.0',
    license: 'Apache-2.0',
    source: 'cargo',
    licenseFiles: [],
    noticeFiles: [{ filename: 'COPYRIGHT-RUST.txt', content: 'Copyright notice text' }],
    authors: ['Author Name <author@example.com>'],
  };
  const { groupedTexts, componentsWithoutFiles } = groupLicenseTexts([comp]);
  assert.strictEqual(componentsWithoutFiles.length, 1);
  assert.strictEqual(componentsWithoutFiles[0].name, 'copyright-only-pkg');
  assert.strictEqual(groupedTexts.length, 1);
  assert.strictEqual(groupedTexts[0].components[0].filename, 'COPYRIGHT-RUST.txt');
  // Rendered: the notice entry keeps the copyright text, and the standard
  // license section gives the license text with the component listed.
  const md = generateNoticesMarkdown([comp], []);
  assert.match(md, /- `copyright-only-pkg@1\.0\.0` \(cargo, file: `COPYRIGHT-RUST\.txt`\)\n\n```text\nCopyright notice text\n```/);
  const standard = md.slice(md.indexOf('### Standard License: Apache-2.0'));
  assert.match(standard, /^### Standard License: Apache-2\.0\n\n[^\n]*\n- `copyright-only-pkg@1\.0\.0` - SPDX: `Apache-2\.0`/);
  assert.match(standard, /```text\n\s*Apache License\s*\n\s*Version 2\.0/);
});

test('standard texts: a leftover placeholder fails, and a holder-placeholder text needs a reviewed attribution', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'placeholder-guard-fixture-'));
  try {
    const comp = (name, license) => ({ name, version: '1.0.0', license, source: 'cargo', licenseFiles: [], noticeFiles: [], authors: ['Some Author'] });
    // A "Copyright (C) <year>" with no author placeholder is not replaced, and must not pass.
    const lic = path.join(tmpDir, 'licenses.json');
    fs.writeFileSync(lic, JSON.stringify({ MIT: 'Copyright (C) <year>\n\nPermission is hereby granted.' }));
    assert.throws(() => generateNoticesMarkdown([comp('year-only-crate', 'MIT')], [], lic), /Standard license text for MIT contains unpopulated placeholders/);
    // The GPL-family appendix after the terms is a template and keeps its placeholders.
    fs.writeFileSync(lic, JSON.stringify({ MIT: 'The terms.\n\nEND OF TERMS AND CONDITIONS\n\nHow to Apply These Terms\n\nCopyright (C) <year> <name of author>' }));
    assert.match(generateNoticesMarkdown([comp('appendix-crate', 'MIT')], [], lic), /Copyright \(C\) <year> <name of author>/);
    // A text with a holder placeholder (0BSD) and no reviewed attribution is refused.
    assert.throws(() => generateNoticesMarkdown([comp('unreviewed-crate', '0BSD')], []), /Component unreviewed-crate@1\.0\.0 requires an entry in PINNED_FALLBACK_ATTRIBUTIONS/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('fallback attributions: a pinned year without a copyrightHolder is refused', () => {
  PINNED_FALLBACK_ATTRIBUTIONS['year-no-holder@1.0.0'] = { author: 'Only Author', year: '2020', source: 'test fixture' };
  try {
    assert.throws(() => generateNoticesMarkdown([], []), /Entry year-no-holder@1\.0\.0 in PINNED_FALLBACK_ATTRIBUTIONS has a year but no copyrightHolder/);
  } finally {
    delete PINNED_FALLBACK_ATTRIBUTIONS['year-no-holder@1.0.0'];
  }
});

test('holder-free license: a bare "Copyright (c)" before license prose names no holder, so the sources are searched', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'bare-marker-fixture-'));
  try {
    fs.writeFileSync(path.join(tmpDir, 'LICENSE'), 'MIT License\n\nCopyright (c)\nPermission is hereby granted, free of charge, to any person.');
    fs.mkdirSync(path.join(tmpDir, 'src'));
    fs.writeFileSync(path.join(tmpDir, 'src', 'lib.rs'), '// Copyright 2024 Real Holder\n');
    const comp = { name: 'bare-marker', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: ['A'], ...readPackageFiles(tmpDir, 'bare-marker@1.0.0') };
    assert.strictEqual(hasCopyrightHolderInFiles(comp), false);
    assert.match(generateNoticesMarkdown([comp], []), /- `bare-marker@1\.0\.0` \(cargo, file: `LICENSE`\)\n {2}- Copyright 2024 Real Holder \(src\/lib\.rs\)\n/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('standard SPDX texts: 0BSD fallback replaces uppercase (C) and <year>/<author> placeholders', () => {
  const comp = {
    name: 'test-0bsd-crate',
    version: '1.0.0',
    license: '0BSD',
    source: 'cargo',
    licenseFiles: [],
    noticeFiles: [],
    authors: ['ZeroBSD Author <zerobsd@example.com>'],
  };
  const reviewed = { ...PINNED_FALLBACK_ATTRIBUTIONS, 'test-0bsd-crate@1.0.0': { author: 'ZeroBSD Author <zerobsd@example.com>', source: 'test fixture' } };
  const out = generateNoticesMarkdown([comp], [], SPDX_STANDARD_LICENSES_FILE, [], undefined, reviewed);
  assert.match(out, /### Standard License: 0BSD/);
  assert.match(out, /ZeroBSD Author <zerobsd@example\.com> \(test-0bsd-crate\)/);
  assert.doesNotMatch(out, /<year>/i);
  assert.doesNotMatch(out, /<author>/i);
  assert.doesNotMatch(out, /<copyright holders>/i);
});

test('anchor validation: excludes fenced code blocks from duplicate anchor detection', () => {
  const compA = {
    name: 'crate-with-id-in-license',
    version: '1.0.0',
    license: 'MIT',
    source: 'cargo',
    licenseFiles: [
      {
        filename: 'LICENSE',
        content:
          'MIT License\n\nCode snippet with triple backticks:\n```\nconst x = 1;\n<a id="notice-crate-with-id-in-license"></a>\n```\nAnother snippet: id="foo"',
      },
    ],
    noticeFiles: [],
    authors: ['Test Author'],
  };
  // Should NOT throw duplicate anchor error even though the license text contains
  // embedded triple-backtick fences (causing makeCodeBlock to use 4-backtick outer fences)
  // and an embedded <a id="..."> matching the component anchor.
  const out = generateNoticesMarkdown([compA], [], SPDX_STANDARD_LICENSES_FILE, []);
  assert.ok(anchorIdsOutsideFences(out).has('notice-crate-with-id-in-license'));
});

test('alloc-stdlib: copyright line matches upstream license and separates author from copyright holder', () => {
  const content = lf(fs.readFileSync(path.join(ROOT, 'THIRD-PARTY-NOTICES.md'), 'utf8'));
  assert.match(content, /Copyright \(c\) 2016 Dropbox, Inc\. \(alloc-stdlib\)/);
  assert.doesNotMatch(committedNoticesWithVerbatim().generated, /Copyright \(c\) 2016 Dropbox, Inc\., Daniel Reiter Horn/);
  assert.match(
    content,
    /- `alloc-stdlib@0\.2\.4` - SPDX: `BSD-3-Clause` \(authors: Daniel Reiter Horn <danielrh@dropbox\.com>\)/
  );
});

test('findPackageCopyrightNotices: extracts header notices, deduplicates, and ignores boilerplate', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'copyright-scan-fixture-'));
  try {
    fs.mkdirSync(path.join(tmpDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(tmpDir, 'src', 'lib.rs'),
      '/*\n * Copyright (c) 2024 Test Author. (MIT License)\n * The above copyright notice and this permission notice shall be included in\n */\nfn foo() {}'
    );
    fs.writeFileSync(
      path.join(tmpDir, 'README.md'),
      '# Test\nCopyright (c) 2024 Test Author.\nSome documentation'
    );
    const notices = findPackageCopyrightNotices(tmpDir);
    assert.strictEqual(notices.length, 1, 'Identical notices across files must be deduplicated');
    assert.strictEqual(notices[0].notice, 'Copyright (c) 2024 Test Author.');
    assert.strictEqual(notices[0].file, 'README.md');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('findPackageCopyrightNotices: untagged source retains a yearless named holder, not a bare marker', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'untagged-yearless-fixture-'));
  try {
    fs.mkdirSync(path.join(tmpDir, 'src'));
    fs.writeFileSync(path.join(tmpDir, 'LICENSE'), 'MIT License\n\nPermission is hereby granted, free of charge...\n');
    fs.writeFileSync(path.join(tmpDir, 'src', 'lib.rs'), '// © Alice Example\n// ©\n');
    assert.deepStrictEqual(findPackageCopyrightNotices(tmpDir).map((n) => n.notice), ['© Alice Example']);
    const comp = {
      name: 'untagged-yearless',
      version: '1.0.0',
      source: 'cargo',
      license: 'MIT',
      authors: ['Manifest Author'],
      packageDir: tmpDir,
      ...readPackageFiles(tmpDir, 'untagged-yearless@1.0.0'),
    };
    const out = generateNoticesMarkdown([comp], []);
    assert.match(out, /- © Alice Example \(src\/lib\.rs\)/);
    assert.doesNotMatch(out, /- © \(src\/lib\.rs\)/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('componentsWithoutFiles copyright notices: extracts upstream source header notices and marks packages without notices', () => {
  const noticesContent = lf(fs.readFileSync(path.join(ROOT, 'THIRD-PARTY-NOTICES.md'), 'utf8'));
  const mitSection = noticesContent.split('### Standard License: MIT')[1]?.split('### Standard License:')[0] || '';

  // qrcodegen@1.8.0 must retain its upstream copyright notice from src/lib.rs
  assert.match(mitSection, /`qrcodegen@1\.8\.0` - SPDX: `MIT`/);
  assert.match(mitSection, /Copyright \(c\) Project Nayuki\. \(src\/lib\.rs\)/);

  // r-efi@5.3.0 must retain its upstream copyright notices from AUTHORS
  assert.match(mitSection, /`r-efi@5\.3\.0`/);
  assert.match(mitSection, /Copyright \(C\) 2017-2023 Red Hat, Inc\. \(AUTHORS\)/);
  assert.match(mitSection, /Copyright \(C\) 2019-2023 Microsoft Corporation \(AUTHORS\)/);
  assert.match(mitSection, /Copyright \(C\) 2022-2023 David Rheinsberg \(AUTHORS\)/);

  // A component without copyright notice shipped in its package must show
  // the explicit line, in its own entry: the lines from its "- `id`" line to
  // the next entry. The control: a neighbour with the absence line does not
  // make an entry that lists a notice pass.
  const entryIn = (section, id) => {
    const lines = section.split('\n');
    const at = lines.findIndex((l) => l.startsWith(`- \`${id}\``));
    if (at < 0) return '';
    const next = lines.findIndex((l, i) => i > at && l.startsWith('- `'));
    return lines.slice(at, next < 0 ? undefined : next).join('\n');
  };
  const absence = /\n {2}- No copyright notice is shipped in the package$/;
  assert.match(entryIn(mitSection, 'qrcodegen-image@1.5.2'), /^- `qrcodegen-image@1\.5\.2` - SPDX: `MIT`/);
  assert.match(entryIn(mitSection, 'qrcodegen-image@1.5.2'), absence);
  const control = '- `qrcodegen-image@1.5.2` - SPDX: `MIT`\n  - Copyright 2020 Someone (src/lib.rs)\n- `neighbour@1.0.0` - SPDX: `MIT`\n  - No copyright notice is shipped in the package';
  assert.match(control, /No copyright notice is shipped in the package/);
  assert.doesNotMatch(entryIn(control, 'qrcodegen-image@1.5.2'), absence);

  // crc-catalog@2.5.0 has dedicated LICENSES/ files, so it is in physical notice entries and NOT in componentsWithoutFiles
  assert.match(noticesContent, /<a id="notice-crc-catalog"><\/a>/);
  assert.match(noticesContent, /`crc-catalog@2\.5\.0` \(cargo, file: `LICENSES\/Apache-2\.0\.txt`\)/);
  assert.match(noticesContent, /`crc-catalog@2\.5\.0` \(cargo, file: `LICENSES\/MIT\.txt`\)/);
});

test('readPackageFiles: REUSE LICENSES directory collects license files so component is not in componentsWithoutFiles', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'reuse-licenses-fixture-'));
  try {
    const licensesDir = path.join(tmpDir, 'LICENSES');
    fs.mkdirSync(licensesDir, { recursive: true });
    fs.writeFileSync(path.join(licensesDir, 'MIT.txt'), 'MIT License text with Copyright (c) 2024 Test Author');

    const res = readPackageFiles(tmpDir, 'test-reuse-pkg@1.0.0');
    assert.strictEqual(res.licenseFiles.length, 1);
    assert.strictEqual(res.licenseFiles[0].filename, 'LICENSES/MIT.txt');

    const dummyComp = {
      name: 'test-reuse-pkg',
      version: '1.0.0',
      license: 'MIT',
      source: 'cargo',
      licenseFiles: res.licenseFiles,
      noticeFiles: res.noticeFiles,
      packageDir: tmpDir,
    };
    const { componentsWithoutFiles, groupedTexts } = groupLicenseTexts([dummyComp]);
    assert.strictEqual(componentsWithoutFiles.length, 0, 'Component with LICENSES/ file must not be in componentsWithoutFiles');
    assert.strictEqual(groupedTexts.length, 1);
    assert.strictEqual(groupedTexts[0].components[0].filename, 'LICENSES/MIT.txt');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('findPackageCopyrightNotices: root AUTHORS copyright lines are extracted as notices', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'authors-notice-fixture-'));
  try {
    fs.writeFileSync(
      path.join(tmpDir, 'AUTHORS'),
      'COPYRIGHT:\n        Copyright (C) 2017-2023 Red Hat, Inc.\n        Copyright (C) 2019-2023 Microsoft Corporation\n'
    );
    const notices = findPackageCopyrightNotices(tmpDir);
    assert.strictEqual(notices.length, 2);
    assert.strictEqual(notices[0].notice, 'Copyright (C) 2017-2023 Red Hat, Inc.');
    assert.strictEqual(notices[0].file, 'AUTHORS');
    assert.strictEqual(notices[1].notice, 'Copyright (C) 2019-2023 Microsoft Corporation');
    assert.strictEqual(notices[1].file, 'AUTHORS');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

/**
 * Whether the notices mark the component as shipping no copyright notice,
 * read the same from an LF and a CRLF checkout.
 */
function marksNoNotice(noticesContent, name, version) {
  const esc = (t) => t.replace(/[-/\\^$*+?.()|[\]{}]/g, '\\$&');
  return new RegExp(`- \`${esc(name)}@${esc(version)}\`[^\n]*\n\\s+- No copyright notice is shipped in the package`).test(lf(noticesContent));
}

test('property selector: a component without a notice is found from an LF and a CRLF checkout alike', () => {
  const md = '- `quiet@1.0.0` - SPDX: `MIT`\n  - No copyright notice is shipped in the package\n- `loud@1.0.0` - SPDX: `MIT`\n  - Copyright (c) 2020 Loud\n';
  for (const text of [md, md.replace(/\n/g, '\r\n')]) {
    assert.ok(marksNoNotice(text, 'quiet', '1.0.0'), JSON.stringify(text));
    assert.ok(!marksNoNotice(text, 'loud', '1.0.0'));
  }
});

test('property: no component marked with "No copyright notice is shipped" has copyright lines in its package sources', () => {
  const noticesContent = fs.readFileSync(path.join(ROOT, 'THIRD-PARTY-NOTICES.md'), 'utf8');
  const { cargo, npm } = realTree();
  const allComponents = [...cargo, ...npm];

  const copyrightPattern = /(?:copyright:?\s*(?:\([cC]\)|©|\d{4})|\([cC]\)\s*\d{4}|©\s*\d{4}|SPDX-FileCopyrightText:\s*\S)\s*[A-Za-z0-9]/i;
  const boilerplateRegex = /above copyright|copyright holders|copyright notice|copyright owner|copyright law|copyright (license|and|or|to|in|is|of|for|statement)|copyrighted|copyright_|copyright:? *$/i;
  const cocoaApiRegex = /NSCopyright|kMDItemCopyright|kFontCopyright|AuthorizationCopyRights/i;

  const falseNoneList = [];

  for (const c of allComponents) {
    if (!c.packageDir || !fs.existsSync(c.packageDir)) continue;

    if (!marksNoNotice(noticesContent, c.name, c.version)) continue;

    const matched = grepCopyrightLines(c.packageDir)
      .split('\n')
      .map((l) => l.trim())
      .filter((l) => l.length > 0 && copyrightPattern.test(l) && !boilerplateRegex.test(l) && !cocoaApiRegex.test(l));
    if (matched.length > 0) {
      falseNoneList.push({ comp: `${c.source} ${c.name}@${c.version}`, matched });
    }
  }

  assert.deepStrictEqual(
    falseNoneList,
    [],
    `Components marked with "No copyright notice is shipped" contain real copyright lines in their packages: ${JSON.stringify(falseNoneList, null, 2)}`
  );
});

test('property: grepCopyrightLines excludes nested node_modules while still scanning the component own source', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'grep-nested-nm-fixture-'));
  try {
    fs.mkdirSync(path.join(tmpDir, 'src'), { recursive: true });
    fs.mkdirSync(path.join(tmpDir, 'node_modules', 'nested-dep'), { recursive: true });
    fs.writeFileSync(path.join(tmpDir, 'src', 'index.js'), 'export const x = 1;\n');
    fs.writeFileSync(path.join(tmpDir, 'node_modules', 'nested-dep', 'index.js'), '// Copyright (c) 2024 Nested Dep Author\n');
    assert.strictEqual(grepCopyrightLines(tmpDir).trim(), '', 'nested node_modules notices must not be attributed to parent');
    fs.writeFileSync(path.join(tmpDir, 'src', 'index.js'), '// Copyright (c) 2024 Parent Own Author\nexport const x = 1;\n');
    assert.match(grepCopyrightLines(tmpDir), /Parent Own Author/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('notice-only component: generates "Notice file shipped: ..." and does not claim "No copyright notice is shipped"', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'notice-only-fixture-'));
  try {
    fs.writeFileSync(path.join(tmpDir, 'COPYRIGHT'), 'Copyright (c) 2024 Notice Only Author\n');
    // Subdir with a dummy file having no copyright notice
    fs.mkdirSync(path.join(tmpDir, 'src'));
    fs.writeFileSync(path.join(tmpDir, 'src', 'lib.rs'), 'pub fn hello() {}\n');

    const readFiles = readPackageFiles(tmpDir, 'MIT');
    assert.strictEqual(readFiles.licenseFiles.length, 0);
    assert.strictEqual(readFiles.noticeFiles.length, 1);

    const comp = {
      name: 'fixture-notice-only',
      version: '1.0.0',
      source: 'cargo',
      license: 'MIT',
      authors: ['Notice Only Author'],
      repository: 'https://example.com/notice-only',
      packageDir: tmpDir,
      licenseFiles: readFiles.licenseFiles,
      noticeFiles: readFiles.noticeFiles,
    };

    const out = generateNoticesMarkdown([comp], []);
    assert.match(out, /Notice file shipped: COPYRIGHT/);
    assert.doesNotMatch(out, /No copyright notice is shipped in the package/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('property: grep scan error propagates and fails validation instead of being swallowed', () => {
  // A child of a private temp directory, never created, so nothing else can put it there.
  const parent = fs.mkdtempSync(path.join(os.tmpdir(), 'grep-missing-'));
  try {
    assert.throws(() => grepCopyrightLines(path.join(parent, 'missing')), /Grep scan failed for .* \(status 2\)/);
  } finally {
    fs.rmSync(parent, { recursive: true, force: true });
  }
});

test('property: the grep scan reads a directory whose name has a literal dollar sign', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'grep-$HOME-$PATH-'));
  try {
    fs.writeFileSync(path.join(tmpDir, 'lib.rs'), '// Copyright 2024 Dollar Holder\n');
    assert.strictEqual(grepCopyrightLines(tmpDir).trim(), '// Copyright 2024 Dollar Holder');
    fs.writeFileSync(path.join(tmpDir, 'lib.rs'), 'fn main() {}\n');
    assert.strictEqual(grepCopyrightLines(tmpDir), '');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('holder-free license: SPDX headers and REUSE Copyright fields are notices, a yearless "Copyright (c)" is no holder', () => {
  assert.strictEqual(
    hasCopyrightHolderInFiles({ licenseFiles: [{ filename: 'LICENSE', content: 'Copyright (c)\n' }], noticeFiles: [] }),
    false
  );
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'holder-free-spdx-reuse-'));
  try {
    fs.writeFileSync(path.join(tmpDir, 'LICENSE'), 'MIT License\n\nPermission is hereby granted, free of charge...\n');
    fs.mkdirSync(path.join(tmpDir, 'src'));
    fs.writeFileSync(path.join(tmpDir, 'src', 'lib.rs'), '// SPDX-FileCopyrightText: 2024 Alice Example\npub fn f() {}\n');
    fs.mkdirSync(path.join(tmpDir, '.reuse'));
    fs.writeFileSync(
      path.join(tmpDir, '.reuse', 'dep5'),
      'Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\r\n\r\nFiles: *\r\nCopyright: 2021 Kathryn Long <k@example.com>\r\n  2022 Second Holder\r\nLicense: MIT\r\n'
    );
    fs.writeFileSync(path.join(tmpDir, 'README.md'), 'Copyright: see the LICENSE file\n');
    const readFiles = readPackageFiles(tmpDir, 'MIT');
    const comp = {
      name: 'fixture-spdx-reuse',
      version: '1.0.0',
      source: 'cargo',
      license: 'MIT',
      authors: ['Fixture Author'],
      repository: 'https://example.com/fixture',
      packageDir: tmpDir,
      licenseFiles: readFiles.licenseFiles,
      noticeFiles: readFiles.noticeFiles,
    };
    const out = generateNoticesMarkdown([comp], []);
    assert.match(out, /- SPDX-FileCopyrightText: 2024 Alice Example \(src\/lib\.rs\)/);
    assert.match(out, /- Copyright 2021 Kathryn Long <k@example\.com> \(\.reuse\/dep5\)/);
    assert.match(out, /- Copyright 2022 Second Holder \(\.reuse\/dep5\)/);
    assert.doesNotMatch(out, /see the LICENSE file/);
    assert.doesNotMatch(out, /fixture-spdx-reuse.*\n\s+- No copyright notice is shipped in the package/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('holder-free license: extracts source notices for components with holder-free license files', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'holder-free-fixture-'));
  try {
    // Holder-free MIT license file (Permission is hereby granted...)
    fs.writeFileSync(
      path.join(tmpDir, 'LICENSE'),
      'MIT License\n\nPermission is hereby granted, free of charge, to any person obtaining a copy...\n'
    );
    fs.mkdirSync(path.join(tmpDir, 'src'));
    fs.writeFileSync(
      path.join(tmpDir, 'src', 'lib.rs'),
      '// Copyright 2024 Fixture Holder Developers\n\npub fn fixture() {}\n'
    );

    const readFiles = readPackageFiles(tmpDir, 'MIT');
    assert.strictEqual(readFiles.licenseFiles.length, 1);
    assert.strictEqual(hasCopyrightHolderInFiles({ licenseFiles: readFiles.licenseFiles, noticeFiles: [] }), false);

    const comp = {
      name: 'fixture-holder-free',
      version: '1.0.0',
      source: 'cargo',
      license: 'MIT',
      authors: ['Fixture Author'],
      repository: 'https://example.com/fixture',
      packageDir: tmpDir,
      licenseFiles: readFiles.licenseFiles,
      noticeFiles: readFiles.noticeFiles,
    };

    const out = generateNoticesMarkdown([comp], []);
    assert.match(
      out,
      /- `fixture-holder-free@1\.0\.0` \(cargo, file: `LICENSE`\)\n\s+- Copyright 2024 Fixture Holder Developers \(src\/lib\.rs\)/
    );
    assert.doesNotMatch(out, /fixture-holder-free.*No copyright notice is shipped in the package/);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }

  // Also assert ident_case@1.0.1 in the generated THIRD-PARTY-NOTICES.md, as
  // the generator writes it (LF, also from a CRLF checkout).
  const noticesContent = committedNoticesWithVerbatim().content;
  assert.match(
    noticesContent,
    /- `ident_case@1\.0\.1` \(cargo, file: `LICENSE`\)\n\s+- Copyright 2017 Serde Developers \(src\/lib\.rs\)/
  );
});

test('hasCopyrightHolderInFiles: recognizes markdown backticks and brackets in copyright notices and does not misclassify them as absent', () => {
  const compBackticks = {
    licenseFiles: [{ filename: 'LICENSE.md', content: 'Copyright © `2015` `Sebastian Thiel`\n\nPermission is hereby granted...\n' }],
    noticeFiles: [],
  };
  const compBrackets = {
    licenseFiles: [{ filename: 'LICENSE.md', content: 'Copyright (c) [2021] [Marvin Countryman]\n\nPermission is hereby granted...\n' }],
    noticeFiles: [],
  };
  assert.strictEqual(hasCopyrightHolderInFiles(compBackticks), true);
  assert.strictEqual(hasCopyrightHolderInFiles(compBrackets), true);

  const noticesFile = path.join(ROOT, 'THIRD-PARTY-NOTICES.md');
  const noticesContent = fs.readFileSync(noticesFile, 'utf8');
  assert.doesNotMatch(noticesContent, /- `open@5\.4\.5`[^\n]*\n\s+- No copyright notice is shipped in the package/);
  assert.doesNotMatch(noticesContent, /- `simd-adler32@0\.3\.10`[^\n]*\n\s+- No copyright notice is shipped in the package/);
});

test('hasCopyrightHolderInFiles: Apache boilerplate "Grant of Copyright License" is not classified as holder and extracts source notices', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'apache-boilerplate-fixture-'));
  try {
    fs.writeFileSync(
      path.join(tmpDir, 'LICENSE'),
      'Apache License\nVersion 2.0, January 2004\n\n2. Grant of Copyright License.\nSubject to the terms...\n'
    );
    fs.mkdirSync(path.join(tmpDir, 'src'));
    fs.writeFileSync(
      path.join(tmpDir, 'src', 'lib.rs'),
      '// Copyright 2024 Apache Fixture Author\npub fn foo() {}\n'
    );

    const readFiles = readPackageFiles(tmpDir, 'Apache-2.0');
    assert.strictEqual(hasCopyrightHolderInFiles({ licenseFiles: readFiles.licenseFiles, noticeFiles: [] }), false);

    const comp = {
      name: 'fixture-apache',
      version: '1.0.0',
      source: 'cargo',
      license: 'Apache-2.0',
      authors: ['Apache Author'],
      repository: 'https://example.com/apache',
      packageDir: tmpDir,
      licenseFiles: readFiles.licenseFiles,
      noticeFiles: readFiles.noticeFiles,
    };

    const out = generateNoticesMarkdown([comp], []);
    assert.match(
      out,
      /- `fixture-apache@1\.0\.0` \(cargo, file: `LICENSE`\)\n\s+- Copyright 2024 Apache Fixture Author \(src\/lib\.rs\)/
    );
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('compMap: distinguishes Cargo and npm components with identical name and version', () => {
  const tmpDirCargo = fs.mkdtempSync(path.join(os.tmpdir(), 'eco-cargo-'));
  const tmpDirNpm = fs.mkdtempSync(path.join(os.tmpdir(), 'eco-npm-'));
  try {
    fs.writeFileSync(path.join(tmpDirCargo, 'LICENSE'), 'MIT License\n\nPermission is hereby granted...\n');
    fs.mkdirSync(path.join(tmpDirCargo, 'src'));
    fs.writeFileSync(path.join(tmpDirCargo, 'src', 'lib.rs'), '// Copyright 2024 Cargo Eco Author\n');

    fs.writeFileSync(path.join(tmpDirNpm, 'LICENSE'), 'MIT License\n\nPermission is hereby granted...\n');
    fs.writeFileSync(path.join(tmpDirNpm, 'index.js'), '// Copyright 2024 Npm Eco Author\n');

    const cargoComp = {
      name: 'shared-name',
      version: '1.0.0',
      source: 'cargo',
      license: 'MIT',
      authors: ['Cargo Author'],
      repository: 'https://example.com/cargo',
      packageDir: tmpDirCargo,
      licenseFiles: [{ filename: 'LICENSE', content: 'MIT License\n\nPermission is hereby granted...\n' }],
      noticeFiles: [],
    };

    const npmComp = {
      name: 'shared-name',
      version: '1.0.0',
      source: 'npm',
      license: 'MIT',
      authors: ['Npm Author'],
      repository: 'https://example.com/npm',
      packageDir: tmpDirNpm,
      licenseFiles: [{ filename: 'LICENSE', content: 'MIT License\n\nPermission is hereby granted...\n' }],
      noticeFiles: [],
    };

    const out = generateNoticesMarkdown([cargoComp], [npmComp]);
    assert.match(out, /- `shared-name@1\.0\.0` \(cargo, file: `LICENSE`\)\n\s+- Copyright 2024 Cargo Eco Author \(src\/lib\.rs\)/);
    assert.match(out, /- `shared-name@1\.0\.0` \(npm, file: `LICENSE`\)\n\s+- Copyright 2024 Npm Eco Author \(index\.js\)/);
  } finally {
    fs.rmSync(tmpDirCargo, { recursive: true, force: true });
    fs.rmSync(tmpDirNpm, { recursive: true, force: true });
  }
});

test('hasCopyrightHolderInFiles: a list item or prose after the keyword is not a holder, a dated or named one is', () => {
  const holder = (content) =>
    hasCopyrightHolderInFiles({ licenseFiles: [{ filename: 'LICENSE', content }], noticeFiles: [] });
  // Apache-2.0 section 4(c) and MPL-2.0 2.1(c) are list items, not holders.
  assert.strictEqual(holder('     (c) You must retain, in the Source form of any Derivative Works\n'), false);
  assert.strictEqual(holder('(c) under Patent Claims infringed by Covered Software in the absence of\n'), false);
  // Prose that starts with the keyword names no one.
  assert.strictEqual(holder('Copyright has been waived.\n'), false);
  assert.strictEqual(holder('copyright assignment is required to contribute to the "group" library.\n'), false);
  // List items starting with the keyword: CC0 numbers its sections this way.
  assert.strictEqual(holder('1. Copyright and Related Rights. A Work made available under CC0 may be\n'), false);
  // Holders, with and without a year.
  assert.strictEqual(holder('(c) 2020 Alice Example\n'), true);
  assert.strictEqual(holder('Copyright (c) The camino Contributors\n'), true);
  assert.strictEqual(holder('Copyright Mozilla Foundation\n'), true);
  assert.strictEqual(holder('Copyright (c) zune-image developers\n'), true);
  assert.strictEqual(holder('Copyright (c) .NET Foundation and Contributors\n'), true);
  assert.strictEqual(holder('Copyright (c) anatawa12 2023\n'), true);
  assert.strictEqual(
    holder('This work is derived in part from the `linked-hash-map` crate, Copyright (c)\n2015 The Rust Project Developers\n'),
    true
  );
  assert.strictEqual(holder('© Kornel Lesiński\n'), true);
  assert.strictEqual(holder('// Copyright Alice Example\n'), true);
  assert.strictEqual(holder('Copyrights in the `rustix` project are retained by their contributors.\n'), true);
  // The full Apache-2.0 text with its appendix placeholder names no holder.
  assert.strictEqual(
    holder(
      'TERMS AND CONDITIONS\n     (c) You must retain, in the Source form of any Derivative Works\n   Copyright [yyyy] [name of copyright owner]\n'
    ),
    false
  );
});

test('hasCopyrightHolderInFiles: distinguishes true holders from CC0-1.0 non-holder boilerplate and respects start-of-line semantics', () => {
  const cc0Text = `Creative Commons Legal Code\n\nCC0 1.0 Universal\n\nStatement of Purpose\n\nThe laws of most jurisdictions throughout the world automatically confer exclusive Copyright and Related Rights (defined below) upon the creator and subsequent owner(s) (each and all, an "owner") of an original work of authorship and/or a database (each, a "Work").\n\n1. Copyright and Related Rights. A Work made available under CC0 may be protected by copyright and related or neighboring rights ("Copyright and Related Rights"). Copyright and Related Rights include, but are not limited to, the following:\n\n2. Waiver. To the greatest extent permitted by, but not in contravention of, applicable law, Affirmer hereby overtly, fully, permanently, irrevocably and unconditionally waives, abandons, and surrenders all of Affirmer's Copyright and Related Rights...\n\n3. Public License Fallback. ...irrevocable and unconditional license to exercise Affirmer's Copyright and Related Rights in the Work...\n`;

  // Full CC0-1.0 text gives false
  assert.strictEqual(
    hasCopyrightHolderInFiles({
      licenseFiles: [{ filename: 'LICENSE-CC0', content: cc0Text }],
      noticeFiles: [],
    }),
    false
  );

  // Required true cases:
  // 1. Portions Copyright 2014 X
  assert.strictEqual(
    hasCopyrightHolderInFiles({
      licenseFiles: [{ filename: 'LICENSE', content: 'Portions Copyright 2014 X\n' }],
      noticeFiles: [],
    }),
    true
  );

  // 2. SPDX-FileCopyrightText: 2020 X
  assert.strictEqual(
    hasCopyrightHolderInFiles({
      licenseFiles: [{ filename: 'LICENSE', content: 'SPDX-FileCopyrightText: 2020 X\n' }],
      noticeFiles: [],
    }),
    true
  );

  // 3. gtk-rs line
  assert.strictEqual(
    hasCopyrightHolderInFiles({
      licenseFiles: [
        {
          filename: 'COPYRIGHT',
          content: 'Copyrights in the gtk-rs Project project are retained by their contributors.\n',
        },
      ],
      noticeFiles: [],
    }),
    true
  );

  // 4. unrar line
  assert.strictEqual(
    hasCopyrightHolderInFiles({
      licenseFiles: [
        {
          filename: 'license.txt',
          content:
            '   1. All copyrights to RAR and the utility UnRAR are exclusively owned by the author - Alexander Roshal.\n',
        },
      ],
      noticeFiles: [],
    }),
    true
  );

  // Also verify in THIRD-PARTY-NOTICES.md that the 5 CC0 crates have "No copyright notice is shipped in the package"
  const noticesFile = path.join(ROOT, 'THIRD-PARTY-NOTICES.md');
  const noticesContent = fs.readFileSync(noticesFile, 'utf8');
  for (const crate of ['async_io_stream@0.3.3', 'ws_stream_wasm@0.7.5', 'dunce@1.0.5', 'notify@8.2.1', 'whisper-rs@0.16.0']) {
    assert.match(
      noticesContent,
      new RegExp(`- \`${crate.replace('+', '\\+')}\`[^\n]*\n\\s+- No copyright notice is shipped in the package`)
    );
  }
});

test('markdown fences: links immediately after an empty fenced block are kept while quoted links inside fences are ignored', () => {
  const afterEmptyBackticks = '```\n```\nSee [Notices](THIRD-PARTY-NOTICES.md).\n';
  assert.deepStrictEqual(linksOutsideFences(afterEmptyBackticks), ['THIRD-PARTY-NOTICES.md']);
  const afterEmptyTildes = '~~~\n~~~\nSee [Notices](THIRD-PARTY-NOTICES.md).\n';
  assert.deepStrictEqual(linksOutsideFences(afterEmptyTildes), ['THIRD-PARTY-NOTICES.md']);
  const quotedOnly = '```md\n[Quoted](THIRD-PARTY-NOTICES.md)\n```\n';
  assert.deepStrictEqual(linksOutsideFences(quotedOnly), []);
});

test('continuation errors: never copy unreviewed source lines or secrets into error output', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'continuation-secret-fixture-'));
  try {
    fs.writeFileSync(
      path.join(tmpDir, 'cfg.js'),
      '// Copyright (c) 2024\nconst DSN = "postgres://admin:super-secret-pass-42@db.internal/app";\n'
    );
    let thrown;
    try {
      findPackageCopyrightNotices(tmpDir);
    } catch (err) {
      thrown = err;
    }
    assert.ok(thrown, 'expected continuation error');
    assert.match(thrown.message, /cfg\.js:1 may continue on its next line/);
    assert.doesNotMatch(thrown.message, /super-secret-pass-42/);
    assert.doesNotMatch(thrown.message, /postgres:\/\//);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('optional npm packages: accounts for libc mismatch instead of blindly prescribing npm ci', () => {
  const hostLibc = currentNodeLibc();
  const foreignLibc = hostLibc === 'musl' ? 'glibc' : 'musl';
  assert.strictEqual(npmInstallsHere([foreignLibc], hostLibc), false);
  assert.strictEqual(npmInstallsHere([`!${foreignLibc}`], hostLibc), true);
  if (hostLibc) {
    assert.strictEqual(npmInstallsHere([hostLibc], hostLibc), true);
    assert.strictEqual(npmInstallsHere([`!${hostLibc}`], hostLibc), false);
  }

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'libc-optional-fixture-'));
  try {
    fs.writeFileSync(
      path.join(tmpDir, 'package-lock.json'),
      JSON.stringify({
        lockfileVersion: 3,
        packages: {
          '': { name: 'fixture', version: '1.0.0' },
          'node_modules/native-libc-only': {
            version: '1.0.0',
            license: 'MIT',
            optional: true,
            os: [process.platform],
            cpu: [process.arch],
            libc: [foreignLibc],
          },
        },
      })
    );
    assert.throws(
      () => collectNpmDependencies(path.join(tmpDir, 'package-lock.json'), tmpDir),
      (err) =>
        /native-libc-only@1\.0\.0/.test(err.message) &&
        new RegExp(`libc ${foreignLibc}`).test(err.message) &&
        !/npm ci without --omit=optional/.test(err.message)
    );
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('license coverage: BSD-3-Clause with an empty holder slot requires the standard fallback, a named holder does not', () => {
  const standard = JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'));
  const bsd3 = standard['BSD-3-Clause'];
  const withHolder = bsd3
    .replace(/Copyright \(c\) <year> <copyright holders>/i, 'Copyright (c) 2024 Acme Corp')
    .replace(/Neither the name of the copyright holder nor/i, 'Neither the name of Acme Corp nor');
  const withSingleLetterHolder = bsd3
    .replace(/Copyright \(c\) <year> <copyright holders>/i, 'Copyright (c) 2024 Q')
    .replace(/Neither the name of the copyright holder nor/i, 'Neither the name of Q nor');
  const withEmptySlot = withHolder.replace(/Neither the name of Acme Corp nor/i, 'Neither the name of nor');
  assert.notStrictEqual(withHolder, bsd3);
  assert.notStrictEqual(withEmptySlot, withHolder);
  assert.strictEqual(carriesWholeStandardText('BSD-3-Clause', bsd3, withHolder), true);
  assert.strictEqual(carriesWholeStandardText('BSD-3-Clause', bsd3, withSingleLetterHolder), true);
  assert.strictEqual(carriesWholeStandardText('BSD-3-Clause', bsd3, withEmptySlot), false);

  const mkComp = (name, text) => ({
    name,
    version: '1.0.0',
    license: 'BSD-3-Clause',
    source: 'cargo',
    authors: ['Acme Corp'],
    licenseFiles: [{ filename: 'LICENSE', content: text }],
    noticeFiles: [],
  });
  const fb = {
    ...PINNED_FALLBACK_ATTRIBUTIONS,
    'empty-slot-crate@1.0.0': { author: 'Acme Corp', source: 'test fixture' },
  };
  const fallbackMd = generateNoticesMarkdown([mkComp('empty-slot-crate', withEmptySlot)], [], SPDX_STANDARD_LICENSES_FILE, KNOWN_NON_FREE_EXCEPTIONS, undefined, fb);
  assert.match(fallbackMd, /### Standard License: BSD-3-Clause/);
  const fullMd = generateNoticesMarkdown([mkComp('named-slot-crate', withHolder)], [], SPDX_STANDARD_LICENSES_FILE, KNOWN_NON_FREE_EXCEPTIONS, undefined, fb);
  assert.doesNotMatch(fullMd, /### Standard License: BSD-3-Clause/);
});

test('vendored headers: retains yearless copyright notices covered by LICENSES/<id>.txt and deduplicates equivalent notices', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'vendored-yearless-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', shippedLicense('MIT License\n\nCopyright (c) 2024 Wrapper Corp', 'MIT'));
    write('vendor/lib/LICENSES/curl.txt', `curl license\n\n${reviewedLicenseTerms()['curl']}`);
    write('vendor/lib/a.c', '/*\n * \u00a9 Alice Example\n * SPDX-License-Identifier: curl\n */\nint a;\n');
    write('vendor/lib/b.c', '/*\n * Copyright (c) 2020 Bob Builder\n * SPDX-License-Identifier: curl\n */\nint b;\n');
    write('vendor/lib/c.c', '/*\n * \u00a9 Alice Example.\n * SPDX-License-Identifier: curl\n */\nint c;\n');
    const comp = {
      name: 'vend-curl-pkg',
      version: '1.0.0',
      license: 'MIT',
      source: 'cargo',
      packageDir: tmpDir,
      authors: ['Wrapper Corp'],
      ...readPackageFiles(tmpDir, 'vend-curl-pkg@1.0.0'),
    };
    const md = generateNoticesMarkdown([comp], []);
    assert.match(md, / {2}- \u00a9 Alice Example \(vendor\/lib\/a\.c\)/, 'yearless vendored header notice must be rendered');
    assert.strictEqual(md.split('Alice Example').length - 1, 1, 'equivalent yearless vendored notice must appear only once');
    assert.strictEqual(md.split('Bob Builder').length - 1, 1, 'dated vendored notice found by both scans must appear only once');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('fallback attribution review: production packages named test-* are never waived without an explicit reviewed record', () => {
  const comp = {
    name: 'test-widget',
    version: '1.0.0',
    license: '0BSD',
    source: 'npm',
    licenseFiles: [],
    noticeFiles: [],
    authors: ['Widget Author'],
  };
  assert.throws(
    () => generateNoticesMarkdown([], [comp]),
    /Component test-widget@1\.0\.0 requires an entry in PINNED_FALLBACK_ATTRIBUTIONS/
  );
  const reviewed = {
    ...PINNED_FALLBACK_ATTRIBUTIONS,
    'test-widget@1.0.0': { author: 'Widget Author', source: 'test fixture' },
  };
  const md = generateNoticesMarkdown([], [comp], SPDX_STANDARD_LICENSES_FILE, KNOWN_NON_FREE_EXCEPTIONS, undefined, reviewed);
  assert.match(md, /Widget Author \(test-widget\)/);
});

test('GPL audit: rejected license values containing credentials never reach stderr and exit nonzero', () => {
  const secret = 'super-secret-cred-98765';
  const badDep = {
    source: 'npm',
    name: 'cred-license-pkg',
    version: '1.0.0',
    license: `https://user:${secret}@private.example.com/LICENSE`,
  };
  const lines = [];
  const ok = auditGplCompatibility([badDep], (line) => lines.push(line));
  assert.strictEqual(ok, false);
  const joined = lines.join('\n');
  assert.match(joined, /cred-license-pkg@1\.0\.0/);
  assert.doesNotMatch(joined, new RegExp(secret));
  assert.doesNotMatch(joined, /private\.example\.com/);

  // Verify nonzero exit code and secret-free stderr through Node execution.
  const tsxCli = createRequire(path.join(ROOT, 'package.json')).resolve('tsx/cli');
  const script = `import { auditGplCompatibility } from './scripts/gen-third-party-notices.ts'; if (!auditGplCompatibility([${JSON.stringify(badDep)}])) process.exit(1);`;
  const run = spawnSync(process.execPath, [tsxCli, '-e', script], { cwd: ROOT, encoding: 'utf8' });
  assert.strictEqual(run.status, 1);
  assert.match(run.stderr, /cred-license-pkg@1\.0\.0/);
  assert.doesNotMatch(run.stderr, new RegExp(secret));
});
