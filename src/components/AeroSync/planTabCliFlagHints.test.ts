// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import planRaw from './PlanTabContent.tsx?raw';
import syncRaw from './SyncTabContent.tsx?raw';
import cliRaw from '../../../src-tauri/src/bin/aeroftp_cli.rs?raw';

/**
 * The Plan tab names the `aeroftp-cli sync` flag next to the option it sets,
 * so the GUI teaches the CLI (Ehud, #347). The flag is a separate element
 * after the translated label, never part of the translation, and it opts out
 * of the label's `uppercase`: an uppercased `(--EXCLUDE)` is not a flag the
 * CLI accepts.
 */
describe('Plan tab shows the CLI flag next to its option (#347)', () => {
    /** The `<label>` element that renders the translation key `key`. */
    const labelFor = (raw: string, key: string): string => {
        const at = raw.indexOf(`t('${key}')`);
        expect(at, `${key} is rendered`).toBeGreaterThan(-1);
        const start = raw.lastIndexOf('<label', at);
        const end = raw.indexOf('</label>', at);
        expect(start, `${key} sits inside a <label>`).toBeGreaterThan(-1);
        expect(end, `${key} label is closed`).toBeGreaterThan(at);
        return raw.slice(start, end);
    };

    const expectHintAfterLabel = (key: string, flag: string, raw: string = planRaw): void => {
        const label = labelFor(raw, key);
        // Right after `{t('key') || 'Fallback'}`, with nothing but whitespace
        // in between, still inside the same <label>.
        const pattern = new RegExp(
            `\\{t\\('${key.replace(/\./g, '\\.')}'\\) \\|\\| '[^']*'\\}\\s*`
            + `<span className="([^"]*)">\\(${flag}\\)</span>`,
        );
        const match = label.match(pattern);
        expect(match, `(${flag}) follows the ${key} label`).toBeTruthy();
        const classes = (match?.[1] ?? '').split(/\s+/);
        expect(classes, `(${flag}) is not uppercased by its label`).toContain('normal-case');
        expect(classes, `(${flag}) reads as code`).toContain('font-mono');
    };

    it('names --exclude after Exclude patterns', () => {
        expectHintAfterLabel('aerosync.exclude.label', '--exclude');
    });

    it('names --dry-run after Canary Mode', () => {
        expectHintAfterLabel('syncPanel.canaryMode', '--dry-run');
    });

    it('names --exclude and --dry-run in the Local mirror tab too', () => {
        // The same sync command runs a local-to-local mirror, and this tab's
        // Dry run checkbox is the literal --dry-run.
        expectHintAfterLabel('aerosync.sync.excludeLabel', '--exclude', syncRaw);
        expectHintAfterLabel('aerosync.sync.dryRun', '--dry-run', syncRaw);
    });

    it('names flags that `aeroftp-cli sync` really takes', () => {
        const start = cliRaw.indexOf('/// Synchronize local and remote directories\n    Sync {');
        expect(start, 'the sync subcommand is declared in aeroftp_cli.rs').toBeGreaterThan(-1);
        const sync = cliRaw.slice(start, cliRaw.indexOf('\n    },', start));
        // clap derives `--dry-run` and `--exclude` from these field names.
        expect(sync).toMatch(/#\[arg\(long\)\]\s+dry_run: bool,/);
        expect(sync).toMatch(/#\[arg\(long, short\)\]\s+exclude: Vec<String>,/);
    });
});
