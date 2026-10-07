// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { readdir, readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const assets = new URL('../dist/assets/', import.meta.url);
const forbidden = ['__aeroftpController', 'Dev harness', 'GUI target denied'];
const files = (await readdir(assets)).filter(name => name.endsWith('.js'));
if (!files.length) throw new Error('Production JavaScript assets are missing');
for (const name of files) {
    const path = new URL(name, assets);
    const source = await readFile(path, 'utf8');
    for (const marker of forbidden) {
        if (source.includes(marker)) throw new Error(`Development GUI harness leaked into ${fileURLToPath(path)}: ${marker}`);
    }
}
console.log(`GUI production boundary: ${files.length} bundles checked, no development harness`);
