// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet (see AI-TRANSPARENCY.md)

import test from 'node:test';
import assert from 'node:assert';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import {
  isGplCompatible,
  tokenizeSpdx,
  extractSpdxLicenseIds,
  collectNpmDependencies,
  readPackageFiles,
  groupLicenseTexts,
  generateNoticesMarkdown,
  SPDX_STANDARD_LICENSES_FILE,
  PINNED_CRATE_LICENSES,
  KNOWN_NON_FREE_EXCEPTIONS,
  PINNED_FALLBACK_ATTRIBUTIONS,
  formatDiff,
} from '../../scripts/gen-third-party-notices.ts';

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

test('fallback placeholders: no <copyright holders> or unpopulated placeholders remain in output', () => {
  const noticesFile = path.join(ROOT, 'THIRD-PARTY-NOTICES.md');
  const content = fs.readFileSync(noticesFile, 'utf8');

  assert.doesNotMatch(content, /<copyright holders>/);
  assert.doesNotMatch(content, /see repository maintainers/);
  // Verifies reviewed attribution is present for cx448
  assert.match(
    content,
    /`cx448@0\.1\.1` - SPDX: `BSD-3-Clause` \(authors: Friedel Ziegelmayer \(port to RustCrypto\), based on crate-crypto\/x448 by Kevaundray Wedderburn\)/
  );
  assert.match(
    content,
    /No copyright notice is shipped upstream; authors per https:\/\/github\.com\/dignifiedquire\/cx448 .*: Friedel Ziegelmayer \(port to RustCrypto\), based on crate-crypto\/x448 by Kevaundray Wedderburn \(cx448\)/
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

  // Standard declarations for crates without physical files
  assert.match(content, /## Standard License Texts for Components without Dedicated License Files/);
  assert.match(content, /The copyright holders for the following components are the authors listed in their package manifests/);

  // Exclusions (dev deps and unused optional crates like aws-lc-fips-sys)
  assert.doesNotMatch(content, /\| vitest \|/);
  assert.doesNotMatch(content, /\| typescript \|/);
  assert.doesNotMatch(content, /\| aeroftp \|/);
  assert.doesNotMatch(content, /\| aws-lc-fips-sys \|/);
});

test('fallback attributions: all 32 pinned entries declare a non-empty source and fail fast if missing', () => {
  const keys = Object.keys(PINNED_FALLBACK_ATTRIBUTIONS);
  assert.strictEqual(keys.length, 32, 'Expected exactly 32 pinned fallback attributions');
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
