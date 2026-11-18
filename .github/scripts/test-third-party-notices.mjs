// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet (see AI-TRANSPARENCY.md)

import test from 'node:test';
import assert from 'node:assert';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync, execSync } from 'node:child_process';
import {
  isGplCompatible,
  tokenizeSpdx,
  extractSpdxLicenseIds,
  collectCargoDependencies,
  collectNpmDependencies,
  readPackageFiles,
  groupLicenseTexts,
  generateNoticesMarkdown,
  SPDX_STANDARD_LICENSES_FILE,
  PINNED_CRATE_LICENSES,
  KNOWN_NON_FREE_EXCEPTIONS,
  PINNED_FALLBACK_ATTRIBUTIONS,
  formatDiff,
  findPackageCopyrightNotices,
  hasCopyrightHolderInFiles,
  findBundledCodeUnderOtherLicenses,
  licenseTermsOf,
  vendoredFolders,
  isSpdxHeaderLine,
  holdersOf,
  assertNoGeneratedPlaceholders,
  KNOWN_SPDX_TAG_MENTIONS,
  renderNotices,
} from '../../scripts/gen-third-party-notices.ts';

// The committed notices with the blocks the generator reproduces as shipped,
// from a fresh generation of the same tree (check:licenses runs first in CI,
// so the two are equal). Checks of the committed file read the text the
// notices write: content minus exactly those blocks.
let committedNotices;
function committedNoticesWithVerbatim() {
  if (!committedNotices) {
    const content = fs.readFileSync(path.join(ROOT, 'THIRD-PARTY-NOTICES.md'), 'utf8').replace(/\r\n/g, '\n');
    const { output, verbatimBlocks } = renderNotices(collectCargoDependencies(), collectNpmDependencies());
    assert.strictEqual(content, output, 'THIRD-PARTY-NOTICES.md is up to date (npm run gen:licenses)');
    let generated = content;
    for (const block of verbatimBlocks) generated = generated.split(block).join('');
    committedNotices = { content, verbatimBlocks, generated };
  }
  return committedNotices;
}

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const ROOT = path.resolve(__dirname, '..', '..');

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
    assert.strictEqual(result.licenseFiles[0].filename, 'terms.txt');
    assert.strictEqual(result.licenseFiles[0].content, 'Custom terms and conditions');

    // Fails fast if declared license_file does not exist
    assert.throws(
      () => readPackageFiles(tmpDir, 'custom-crate@1.0.0', 'legal/missing-file.txt'),
      /Crate custom-crate@1\.0\.0 declared license_file "legal\/missing-file\.txt" but the file does not exist/
    );
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

  const md = generateNoticesMarkdown([dummyComponent], []);
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
    write('vendor/lib/LICENSES/FSFULLR.txt', 'Copyright 2010 Free Software Foundation. This file is free software; the Free Software Foundation gives unlimited permission to copy, distribute and modify it.');
    write('vendor/lib/src/y.c', '/* SPDX-License-Identifier: FSFULLR */\n');
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
    fs.writeFileSync(path.join(withNotice, 'LICENSE-MIT'), 'MIT License\n\nCopyright (c) 2020 Notice Holder\n\nPermission is hereby granted.');
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
    write('LICENSE', 'ISC License\n\nCopyright (c) 2015 Wrapper Author\n\nPermission to use, copy, modify.');
    write('lib/index.js', '/**\n * @license QR Code generator library\n * Copyright (c) Project Nayuki.\n * SPDX-License-Identifier: MIT\n */\nvar a;\n/**\n * Copyright (c) Wrapper Author\n * SPDX-License-Identifier: ISC\n */\n');
    write('lib/either.js', '// SPDX-License-Identifier: ISC OR MIT\n');
    write('lib/typo.js', '// SPDX-License-Identifier: Apache-2.inner\n');
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
    write('own-and-vendored/LICENSE-MIT', 'MIT License\n\nCopyright (c) 2020 Own Holder\n\nPermission is hereby granted.');
    // "Abc/" sorts before "LICENSE-MIT", so its group comes first.
    write('own-and-vendored/Abc/COPYING', 'Copyright (c) 2004 Vendored Holder\nRedistribution and use in source and binary forms, vendored.');
    write('holder-free-own/LICENSE', 'Permission is hereby granted, free of charge, to any person.');
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
    // A package whose only license text is vendored is anchored at that text (unrar_sys).
    assert.strictEqual(after('notice-only-vendored'), 'Copyright (c) 2004 Bar Project');
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
    const mit = 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted, free of charge.';
    write('LICENSE', mit);
    write('lib/vendor.js', '/*\n * Copyright (c) 2018 Vendor Author\n * SPDX-License-Identifier: MIT\n */\n');
    const comp = { name: 'declared-pkg', version: '1.0.0', license: 'MIT', source: 'npm', packageDir: tmpDir, authors: ['Wrapper Author'], ...readPackageFiles(tmpDir, 'declared-pkg@1.0.0') };
    const md = generateNoticesMarkdown([comp], []);
    assert.ok(md.includes('Copyright (c) 2020 Wrapper Author'));
    assert.match(md, /\n {2}- Copyright \(c\) 2018 Vendor Author \(in a source header under MIT\)\n/);
    assert.doesNotMatch(md, /### Bundled Code: MIT/);
    assert.strictEqual(md.split('Permission is hereby granted, free of charge.').length - 1, 1, 'the MIT text appears once');
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
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
    const shippedTerms = 'Permission to use, copy, modify, and distribute this software for any purpose\nwith or without fee is hereby granted.';
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
    write('LICENSE', 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted.');
    write('LICENSES/SAX-PD-2.0.txt', 'SAX 2.0 is free and in the public domain.');
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

test('declared header holders: a holder is named by the package only when it is the same complete holder', () => {
  assert.deepStrictEqual(holdersOf('MIT License\n\nCopyright (c) 2020 Alice Smith\n\n  - Intel Corporation (Copyright 2012-2021)\nPermission is hereby granted.'), ['alice smith', 'intel corporation']);
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'exact-holder-fixture-'));
  try {
    const write = (rel, content) => {
      fs.mkdirSync(path.dirname(path.join(tmpDir, rel)), { recursive: true });
      fs.writeFileSync(path.join(tmpDir, rel), content);
    };
    write('LICENSE', 'MIT License\n\nCopyright (c) 2020 Alice Smith\n\n  - Intel Corporation (Copyright 2012-2021)\n\nPermission is hereby granted, free of charge.');
    write('src/a.c', '/* Copyright (c) 2021 Alice\n * SPDX-License-Identifier: MIT */\n');
    write('src/b.c', '/* Copyright (C) 2019 Alice Smith.\n * SPDX-License-Identifier: MIT */\n');
    write('src/c.c', '/* Copyright (c) 2014, Intel Corporation. All Rights Reserved.\n * SPDX-License-Identifier: MIT */\n');
    write('build.bat', '@echo off\nREM -- Copyright Batch Vendor\nREM -- SPDX-License-Identifier: MIT\n');
    const comp = { name: 'exact-holder', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: [], ...readPackageFiles(tmpDir, 'exact-holder@1.0.0') };
    const md = generateNoticesMarkdown([comp], []);
    assert.match(md, /^ {2}- Copyright \(c\) 2021 Alice \(in a source header under MIT\)$/m, 'Alice is not Alice Smith');
    assert.doesNotMatch(md, /Alice Smith\. \(in a source header/, 'the same holder is attributed by the package');
    assert.doesNotMatch(md, /Intel Corporation\. All Rights Reserved\. \(in a source header/, 'a holder of the credits list is attributed by the package');
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
    write('LICENSE', 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted, free of charge.');
    write('src/menu.rs', '// Copyright 2022-2022 Typo Header Author\n// SPDX-License-Identifier: Apache-2.inner\n');
    write('src/items.rs', '// Copyright 2020 Wrapper Author\n// SPDX-License-Identifier: Apache-2.inner\n');
    write('src/bare.rs', '// SPDX-License-Identifier: Apache-2.inner\n');
    const comp = { name: 'typo-holder', version: '1.0.0', license: 'MIT', source: 'cargo', packageDir: tmpDir, authors: [], ...readPackageFiles(tmpDir, 'typo-holder@1.0.0') };
    const known = new Set(Object.keys(JSON.parse(fs.readFileSync(SPDX_STANDARD_LICENSES_FILE, 'utf8'))));
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(comp, known).map((b) => [b.license, b.notices, b.files, b.headers.length, b.declared]), [['Apache-2.inner', ['Copyright 2020 Wrapper Author', 'Copyright 2022-2022 Typo Header Author'], ['src/items.rs', 'src/menu.rs'], 0, true]]);
    const md = generateNoticesMarkdown([comp], []);
    assert.match(md, /^ {2}- Copyright 2022-2022 Typo Header Author \(in a source header under Apache-2\.inner\)$/m);
    assert.doesNotMatch(md, /Copyright 2020 Wrapper Author \(in a source header/, 'the package names this holder itself');
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
    write('LICENSE', 'MIT License\n\nCopyright (c) 2020 Wrapper Author\n\nPermission is hereby granted, free of charge.');
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
  ]) {
    assert.ok(!header(l, rel), l);
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
    assert.throws(() => findBundledCodeUnderOtherLicenses(comp, known), /malformed-pkg@1\.0\.0 has a malformed SPDX-License-Identifier in lib\/broken\.c: 'BSD-2-Clause AND'\. If the line only mentions the tag, add "malformed-pkg@1\.0\.0:lib\/broken\.c:\/\* SPDX-License-Identifier: BSD-2-Clause AND \*\/" with the reason to KNOWN_SPDX_TAG_MENTIONS/);
    fs.rmSync(path.join(tmpDir, 'lib/broken.c'));
    // A comment that mentions the tag fails like a malformed header: the remedy
    // is a reviewed entry, never a guess from the words before the tag.
    write('lib/example.js', '// Example: SPDX-License-Identifier: MIT (preferred)\n');
    assert.throws(() => findBundledCodeUnderOtherLicenses(comp, known), /in lib\/example\.js: 'MIT \(preferred\)'/);
    const listed = { 'malformed-pkg@1.0.0:lib/example.js:// Example: SPDX-License-Identifier: MIT (preferred)': 'a usage example in a comment, not a header' };
    assert.deepStrictEqual(findBundledCodeUnderOtherLicenses(comp, known, listed).filter((b) => !b.declared).map((b) => [b.license, b.files]), [['BSD-2-Clause', ['lib/arm.S', 'lib/one.c', 'lib/valid.c']]]);
    // Only the listed file is skipped: the same line elsewhere still fails.
    write('lib/other.js', '// Example: SPDX-License-Identifier: MIT (preferred)\n');
    assert.throws(() => findBundledCodeUnderOtherLicenses(comp, known, listed), /in lib\/other\.js/);
    fs.rmSync(path.join(tmpDir, 'lib/other.js'));
    // The entry names one line: a malformed header in the same file still fails.
    write('lib/example.js', '// Example: SPDX-License-Identifier: MIT (preferred)\n/* SPDX-License-Identifier: BSD-2-Clause AND */\n');
    assert.throws(() => findBundledCodeUnderOtherLicenses(comp, known, listed), /in lib\/example\.js: 'BSD-2-Clause AND'/);
    write('lib/example.js', '// Example: SPDX-License-Identifier: MIT (preferred)\n');
    // And one version: the same line in another version of the package fails.
    assert.throws(() => findBundledCodeUnderOtherLicenses({ ...comp, version: '2.0.0' }, known, listed), /malformed-pkg@2\.0\.0 has a malformed SPDX-License-Identifier in lib\/example\.js/);
    assert.deepStrictEqual(KNOWN_SPDX_TAG_MENTIONS, {}, 'no reviewed mention is needed on the current tree');
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
        ['LICENSE.txt', 'Custom terms', false],
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

test('THIRD-PARTY-NOTICES.md exists and is up to date with lockfiles', () => {
  const noticesFile = path.join(ROOT, 'THIRD-PARTY-NOTICES.md');
  assert.strictEqual(fs.existsSync(noticesFile), true, 'THIRD-PARTY-NOTICES.md must exist in repo root');

  const out = execFileSync('npx', ['tsx', 'scripts/gen-third-party-notices.ts', '--check'], {
    cwd: ROOT,
    encoding: 'utf8',
  });
  assert.match(out, /OK: .* is up to date \(\d+ Cargo, \d+ npm\)/);
});

test('check:licenses diff reporting: prints unified diff with line numbers on mismatch', () => {
  const existing = 'line 1\nline 2\nline 3\n';
  const generated = 'line 1\nline 2 modified\nline 3\n';
  const diff = formatDiff(existing, generated);
  assert.match(diff, /--- existing \(line 2\)/);
  assert.match(diff, /\+\+\+ generated \(line 2\)/);
  assert.match(diff, /-line 2/);
  assert.match(diff, /\+line 2 modified/);
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
  assert.match(content, /\[Notice Entry \d+\]\(#notice-unrar-sys\)/);
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
  const noticesFile = path.join(ROOT, 'THIRD-PARTY-NOTICES.md');
  const content = fs.readFileSync(noticesFile, 'utf8');

  // A block quoted as shipped may keep an upstream placeholder; the text the
  // notices write may not, standard texts included. The generator's own set.
  const { verbatimBlocks } = committedNoticesWithVerbatim();
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

  // Standard declarations for crates without physical files (asserts source statement and NO ownership claim)
  assert.match(content, /## Standard License Texts for Components without Dedicated License Files/);
  assert.match(content, /Authors for the following components as listed in package manifests or reviewed attributions:/);
  assert.doesNotMatch(committedNoticesWithVerbatim().generated, /The copyright holders for (?:the following|these) components are the authors/);

  // Exclusions (dev deps and unused optional crates like aws-lc-fips-sys)
  assert.doesNotMatch(content, /\| vitest \|/);
  assert.doesNotMatch(content, /\| typescript \|/);
  assert.doesNotMatch(content, /\| aeroftp \|/);
  assert.doesNotMatch(content, /\| aws-lc-fips-sys \|/);
});

test('fallback attributions: all 33 pinned entries declare a non-empty source and fail fast if missing', () => {
  const keys = Object.keys(PINNED_FALLBACK_ATTRIBUTIONS);
  assert.strictEqual(keys.length, 33, 'Expected exactly 33 pinned fallback attributions');
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

test('README.md notice links resolve to valid anchors in THIRD-PARTY-NOTICES.md', () => {
  const readmeContent = fs.readFileSync(path.join(ROOT, 'README.md'), 'utf8');
  const noticesContent = fs.readFileSync(path.join(ROOT, 'THIRD-PARTY-NOTICES.md'), 'utf8');

  const linkRegex = /\[([^\]]+)\]\(THIRD-PARTY-NOTICES\.md#([a-zA-Z0-9_-]+)\)/g;
  const matches = Array.from(readmeContent.matchAll(linkRegex));
  assert.ok(matches.length > 0, 'README.md must contain at least one link to THIRD-PARTY-NOTICES.md anchor');

  for (const match of matches) {
    const anchor = match[2];
    const anchorTag = `<a id="${anchor}"></a>`;
    assert.ok(
      noticesContent.includes(anchorTag),
      `Anchor #${anchor} linked in README.md must be present in THIRD-PARTY-NOTICES.md as ${anchorTag}`
    );
  }

  // Specifically verify notice-unrar-sys is present and linked
  assert.ok(readmeContent.includes('THIRD-PARTY-NOTICES.md#notice-unrar-sys'));
  assert.ok(noticesContent.includes('<a id="notice-unrar-sys"></a>'));
});

test('THIRD-PARTY-NOTICES.md: every anchor id in the generated file is unique', () => {
  const noticesContent = fs.readFileSync(path.join(ROOT, 'THIRD-PARTY-NOTICES.md'), 'utf8');
  const proseOutsideCodeBlocks = noticesContent.replace(/(?:^|\n)(`{3,})[^\n]*\n[\s\S]*?\n\1(?:\n|$)/g, '\n');
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
  const out = generateNoticesMarkdown([comp], [], SPDX_STANDARD_LICENSES_FILE, []);
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
  assert.match(out, /<a id="notice-crate-with-id-in-license"><\/a>/);
});

test('alloc-stdlib: copyright line matches upstream license and separates author from copyright holder', () => {
  const content = fs.readFileSync(path.join(ROOT, 'THIRD-PARTY-NOTICES.md'), 'utf8');
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

test('componentsWithoutFiles copyright notices: extracts upstream source header notices and marks packages without notices', () => {
  const noticesContent = fs.readFileSync(path.join(ROOT, 'THIRD-PARTY-NOTICES.md'), 'utf8');
  const mitSection = noticesContent.split('### Standard License: MIT')[1]?.split('### Standard License:')[0] || '';

  // qrcodegen@1.8.0 must retain its upstream copyright notice from src/lib.rs
  assert.match(mitSection, /`qrcodegen@1\.8\.0` - SPDX: `MIT`/);
  assert.match(mitSection, /Copyright \(c\) Project Nayuki\. \(src\/lib\.rs\)/);

  // r-efi@5.3.0 must retain its upstream copyright notices from AUTHORS
  assert.match(mitSection, /`r-efi@5\.3\.0`/);
  assert.match(mitSection, /Copyright \(C\) 2017-2023 Red Hat, Inc\. \(AUTHORS\)/);
  assert.match(mitSection, /Copyright \(C\) 2019-2023 Microsoft Corporation \(AUTHORS\)/);
  assert.match(mitSection, /Copyright \(C\) 2022-2023 David Rheinsberg \(AUTHORS\)/);

  // A component without copyright notice shipped in its package must show the explicit line
  assert.match(mitSection, /`qrcodegen-image@1\.5\.2` - SPDX: `MIT`/);
  assert.match(mitSection, /No copyright notice is shipped in the package/);

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

test('property: no component marked with "No copyright notice is shipped" has copyright lines in its package sources', () => {
  const noticesContent = fs.readFileSync(path.join(ROOT, 'THIRD-PARTY-NOTICES.md'), 'utf8');
  const cargo = collectCargoDependencies();
  const npm = collectNpmDependencies();
  const allComponents = [...cargo, ...npm];

  const copyrightPattern = /(?:copyright:?\s*(?:\([cC]\)|©|\d{4})|\([cC]\)\s*\d{4}|©\s*\d{4}|SPDX-FileCopyrightText:\s*\S)\s*[A-Za-z0-9]/i;
  const boilerplateRegex = /above copyright|copyright holders|copyright notice|copyright owner|copyright law|copyright (license|and|or|to|in|is|of|for|statement)|copyrighted|copyright_|copyright:? *$/i;
  const cocoaApiRegex = /NSCopyright|kMDItemCopyright|kFontCopyright|AuthorizationCopyRights/i;

  const falseNoneList = [];

  for (const c of allComponents) {
    if (!c.packageDir || !fs.existsSync(c.packageDir)) continue;

    // Check if component has "No copyright notice is shipped in the package"
    const escaped = c.name.replace(/[-/\\^$*+?.()|[\]{}]/g, '\\$&') + '@' + c.version.replace(/[-/\\^$*+?.()|[\]{}]/g, '\\$&');
    const reg = new RegExp(`- \`${escaped}\`[^\n]*\n\\s+- No copyright notice is shipped in the package`);
    if (!reg.test(noticesContent)) {
      continue;
    }

    let hits = '';
    try {
      hits = execSync(
        `grep -rIhi -E "(copyright|©|\\(c\\))" ${JSON.stringify(c.packageDir)}`,
        { encoding: 'utf8' }
      );
      const matched = hits
        .split('\n')
        .map((l) => l.trim())
        .filter((l) => l.length > 0 && copyrightPattern.test(l) && !boilerplateRegex.test(l) && !cocoaApiRegex.test(l));
      if (matched.length > 0) {
        falseNoneList.push({ comp: `${c.source} ${c.name}@${c.version}`, matched });
      }
    } catch (err) {
      if (err.status === 1) {
        // grep exit status 1 indicates no matches found, which is expected for components lacking copyright notices
      } else {
        throw new Error(
          `Grep scan failed for ${c.source} ${c.name}@${c.version} in ${c.packageDir} (status ${err.status}): ${err.message}`
        );
      }
    }
  }

  assert.deepStrictEqual(
    falseNoneList,
    [],
    `Components marked with "No copyright notice is shipped" contain real copyright lines in their packages: ${JSON.stringify(falseNoneList, null, 2)}`
  );
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
  assert.throws(
    () => {
      try {
        // Force an invalid grep option so grep exits with status 2
        execSync('grep --definitely-invalid-option . 2>/dev/null', { encoding: 'utf8' });
      } catch (err) {
        if (err.status === 1) {
          // would be swallowed if exit code was 1
        } else {
          throw new Error(`Grep scan failed (status ${err.status}): ${err.message}`);
        }
      }
    },
    /Grep scan failed \(status 2\)/
  );
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

  // Also assert ident_case@1.0.1 in the generated THIRD-PARTY-NOTICES.md
  const noticesFile = path.join(ROOT, 'THIRD-PARTY-NOTICES.md');
  const noticesContent = fs.readFileSync(noticesFile, 'utf8');
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
