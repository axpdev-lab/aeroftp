// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, it } from 'vitest';
import { TID } from './testIds';

function components(dir: string): string[] {
    return readdirSync(dir, { withFileTypes: true }).flatMap(entry =>
        entry.isDirectory() ? components(join(dir, entry.name)) :
            entry.name.endsWith('.tsx') ? [readFileSync(join(dir, entry.name), 'utf8')] : []);
}
const sources = [readFileSync('src/App.tsx', 'utf8'), ...components('src/components')];

it('keeps the GUI address registry unique and locale independent', () => {
    expect(new Set(Object.values(TID)).size).toBe(Object.keys(TID).length);
    for (const value of Object.values(TID)) expect(value).toMatch(/^[a-z]+(?:[.-][a-z]+)*$/);
});

it('has a rendered component reference for every registered address', () => {
    for (const key of Object.keys(TID)) {
        expect(sources.some(source => source.includes(`TID.${key}`)), key).toBe(true);
    }
});

it('marks secret eye controls and gives them no public GUI address', () => {
    let checked = 0;
    for (const source of sources) {
        for (const match of source.matchAll(/<button\b(?:(?!<button\b)[\s\S])*?<\/button>/g)) {
            const button = match[0];
            if (!/<Eye(?:Off)? /.test(button) ||
                !/set(?:Show(?:Password|Passphrase|Secret|FilenApiKey|AeroCryptPassword|AeroCryptSalt|MasterPassword|OAuthSecrets|ApiKey|UnlockPassphrase)|Revealed|Visible|PassphraseForm)\b/.test(button)) continue;
            checked++;
            expect(button).toContain('data-agent="deny"');
            expect(button).not.toContain('data-testid');
        }
    }
    expect(checked).toBeGreaterThan(40);
});
