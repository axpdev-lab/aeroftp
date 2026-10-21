// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { describe, expect, it, vi } from 'vitest';
import { mkdtempSync, rmSync, statSync, readdirSync, writeFileSync } from 'node:fs';
import { parentLocalPath } from './parentLocalPath';
import { parentDir } from './transferQueueJournal';
import { splitPath } from '../components/BreadcrumbBar';
const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
import { splitPathsByType } from '../components/vault/pathStaging';

describe.skipIf(process.platform !== 'linux')('POSIX double-root paths', () => {
    it('keeps a navigable parent for an existing directory', () => {
        const dir = mkdtempSync('/tmp/aeroftp-local-boundary-');
        try {
            expect(statSync(`/${dir}`).isDirectory()).toBe(true);
            expect(parentLocalPath(`/${dir}`)).toBe('//tmp');
        } finally { rmSync(dir, { recursive: true }); }
    });

    it('retains the filesystem ancestors in breadcrumbs', () => {
        expect(splitPath('//tmp/folder').map(s => s.fullPath)).toEqual(['/', '/tmp', '/tmp/folder']);
    });

    it('stages a selected directory by reading its actual parent', async () => {
        const dir = mkdtempSync('/tmp/aeroftp-local-boundary-');
        try {
            writeFileSync(`${dir}/child.txt`, 'content');
            invoke.mockImplementation(async (_command, args) => readdirSync(args.path, { withFileTypes: true })
                .map(e => ({ name: e.name, is_dir: e.isDirectory() })));
            expect(await splitPathsByType([`/${dir}`])).toEqual({ files: [], dirs: [`/${dir}`] });
        } finally { rmSync(dir, { recursive: true }); }
    });

    it('restores a queued folder with its actual parent', () => {
        expect(parentDir('//tmp/folder')).toBe('//tmp');
    });
});
