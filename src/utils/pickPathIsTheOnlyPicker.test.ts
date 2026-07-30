// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// The other half of the #510 pin, and the durable one.
//
// `pickPath.test.ts` proves the helper tells the user. This proves the app has no
// way around it. Without this test the fix decays by addition rather than by
// edit: the next feature imports `open` from `@tauri-apps/plugin-dialog`, gets a
// `null` on a portal-less host, and the silent button is back in one file while
// the other 68 call sites stay correct -- and nothing goes red.
//
// It is a source check rather than a lint rule so it runs in the same
// `npm run test:unit` everything else does, with no new tooling and nothing to
// remember to enable.
//
// It reads the sources through `import.meta.glob(..., '?raw')` rather than
// `node:fs`. Not incidental: `tsconfig.json` types `src/` as a browser
// (`lib: ES2020, DOM`) and `@types/node` is deliberately not installed, so a
// `node:child_process` import here fails `tsc`, which sits on the production
// build chain (`build: tsc && vite build`). The glob is typed by `vite/client`,
// needs no dependency, and sees exactly the files the bundler sees -- including
// ones not yet committed, so a bad import goes red before it lands.
//
// It asserts on real import statements, not on mentions of the module path. A
// first version searched for the string, which meant a passing prose comment
// could fail the build, and -- worse -- mangling the helper's own import to
// `'@tauri-apps/plugin-dialogX'` still counted as "the helper wraps the plugin",
// so the second test below passed while the picker was broken.
//
// Verified by breaking it, three ways:
//   - `import { open } from '@tauri-apps/plugin-dialog';` in a component
//     -> the first test fails, naming that file
//   - the helper's own import specifier mangled -> the second test fails
//   - a file that only mentions the plugin in a comment -> still green

import { describe, expect, it } from 'vitest';

const MODULE = '@tauri-apps/plugin-dialog';

/** The single module allowed to reach the plugin, because it is the one that wraps it. */
const HELPER = '/src/utils/pickPath.ts';

/** A static import, a re-export, a dynamic import, or a require of the plugin. */
const IMPORTS_PLUGIN =
    /(?:from|import|require)\s*\(?\s*['"]@tauri-apps\/plugin-dialog['"]/;

/**
 * Every non-test source file, as text.
 *
 * Test files are excluded by the glob, not allowlisted afterwards:
 * `pickPath.test.ts` has to mock the plugin, and this very file has to contain
 * the string it searches for. Neither can put a silent button in front of a
 * user, which is what the rule is about.
 */
const SOURCES = import.meta.glob<string>(
    ['/src/**/*.ts', '/src/**/*.tsx', '!/src/**/*.test.ts', '!/src/**/*.test.tsx'],
    { query: '?raw', import: 'default', eager: true },
);

describe('the picker helper is the only door to the dialog plugin', () => {
    it('no module other than the helper imports the plugin', () => {
        const offenders = Object.entries(SOURCES)
            .filter(([file, text]) => file !== HELPER && IMPORTS_PLUGIN.test(text))
            .map(([file]) => file)
            .sort();

        expect(
            offenders,
            `These files import ${MODULE} directly. Import { pickFile, pickSave } `
                + `from src/utils/pickPath instead: a bare open()/save() resolves to null on a host `
                + `with no desktop portal, which is indistinguishable from the user pressing Cancel, `
                + `so the button goes silent again (see #510).`,
        ).toEqual([]);
    });

    it('the helper itself still wraps the plugin', () => {
        // Guards against the check above passing for the wrong reason: if the
        // helper stopped importing the plugin, the offender list would be empty
        // and the rule would look satisfied while no picker worked at all.
        const helper = SOURCES[HELPER];
        expect(helper, `${HELPER} was not found by the source glob`).toBeTypeOf('string');
        expect(
            IMPORTS_PLUGIN.test(helper),
            `${HELPER} no longer imports ${MODULE}, so the rule above is vacuous.`,
        ).toBe(true);
    });
});
