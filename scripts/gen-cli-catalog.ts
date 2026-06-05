#!/usr/bin/env npx tsx
/**
 * Generate the CLI provider catalog (issue #224).
 *
 * The GUI Add Service "All" / list view is driven by the single source of
 * truth `src/components/providerCatalog.ts`. The Rust CLI cannot import that
 * TypeScript, so this script re-projects the same data into a JSON file that
 * `aeroftp-cli catalog` embeds via `include_str!`. Run it whenever the catalog
 * changes; a vitest drift guard (`src/components/__tests__/cliCatalog.test.ts`)
 * fails the test gate if the committed JSON falls out of sync.
 *
 * Usage: npm run gen:cli-catalog
 */

import * as fs from 'fs';
import * as path from 'path';
import { fileURLToPath } from 'url';
import { buildCliCatalog } from '../src/components/providerCatalog';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const TARGET = path.join(__dirname, '../src-tauri/src/cli_catalog.json');

const json = JSON.stringify(buildCliCatalog(), null, 2) + '\n';
fs.writeFileSync(TARGET, json, 'utf8');
console.log(`Wrote ${buildCliCatalog().length} companies to ${path.relative(process.cwd(), TARGET)}`);
