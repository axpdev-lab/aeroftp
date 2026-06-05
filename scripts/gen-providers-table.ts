#!/usr/bin/env npx tsx
/**
 * Generate the canonical providers table (issue #270 17104681).
 *
 * The cloud-drive provider tables drifted between README.md, aeroftp.app and
 * docs.aeroftp.app. `src/components/providerCatalog.ts` is the single source of
 * truth (issue #224); this script projects it into a markdown table and injects
 * it between the `PROVIDERS-TABLE` anchors in README.md and docs/PROVIDERS.md so
 * both stay in sync. A vitest drift guard (`src/components/providerCatalog.test.ts`)
 * fails the test gate if either committed copy falls out of sync.
 *
 * The site (aeroftp.app) and docs (docs.aeroftp.app) can mirror docs/PROVIDERS.md
 * as the authoritative list, killing the drift at the source.
 *
 * Usage: npm run gen:providers-table
 */

import * as fs from 'fs';
import * as path from 'path';
import { fileURLToPath } from 'url';
import { buildProvidersMarkdown } from '../src/components/providerCatalog';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const ROOT = path.join(__dirname, '..');

const BEGIN = '<!-- BEGIN PROVIDERS-TABLE -->';
const END = '<!-- END PROVIDERS-TABLE -->';

function inject(relFile: string, block: string): void {
    const abs = path.join(ROOT, relFile);
    const src = fs.readFileSync(abs, 'utf8');
    const b = src.indexOf(BEGIN);
    const e = src.indexOf(END);
    if (b === -1 || e === -1 || e < b) {
        throw new Error(`${relFile}: missing "${BEGIN}" ... "${END}" anchors`);
    }
    const next = `${src.slice(0, b + BEGIN.length)}\n\n${block}\n\n${src.slice(e)}`;
    if (next !== src) {
        fs.writeFileSync(abs, next, 'utf8');
        console.log(`Updated ${relFile}`);
    } else {
        console.log(`${relFile} already up to date`);
    }
}

const block = buildProvidersMarkdown();
inject('README.md', block);
inject('docs/PROVIDERS.md', block);
