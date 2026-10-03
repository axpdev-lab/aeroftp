// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import appSource from '../App.tsx?raw';
import s3Source from '../../src-tauri/src/providers/s3.rs?raw';
import azureSource from '../../src-tauri/src/providers/azure.rs?raw';
import {
    AZURE_ACCESS_TIERS,
    S3_RESTORE_TIERS,
    S3_STORAGE_CLASSES,
    changeS3StorageClass,
    loadS3Tags,
    needsGlacierRestore,
    restoreFromGlacier,
    saveS3Tags,
    setAzureTier,
} from './cloudTiers';

/**
 * 3.1.4 shipped the S3 storage class, object tagging and Glacier restore
 * commands and Azure's blob tier, announced them (with an `S3TagsDialog` that
 * never existed), and the context menu reached none of them.
 */
describe('S3 storage class, tags and restore; Azure tier', () => {
    it('sends each choice to its command with the arguments it takes', async () => {
        const invoke = vi.fn(async () => undefined);
        await changeS3StorageClass(invoke as never, '/a.bin', 'GLACIER_IR');
        await restoreFromGlacier(invoke as never, '/a.bin', 7.9, 'Bulk');
        await setAzureTier(invoke as never, '/b.bin', 'Archive');
        expect(invoke).toHaveBeenNthCalledWith(1, 's3_change_storage_class', { path: '/a.bin', storageClass: 'GLACIER_IR' });
        expect(invoke).toHaveBeenNthCalledWith(2, 's3_glacier_restore', { path: '/a.bin', days: 7, tier: 'Bulk' });
        expect(invoke).toHaveBeenNthCalledWith(3, 'azure_set_blob_tier', { path: '/b.bin', tier: 'Archive' });
    });

    it('reads tags sorted, writes them back, and clears instead of writing an empty set', async () => {
        const invoke = vi.fn(async (cmd: string) => (cmd === 's3_get_object_tags' ? { team: 'ops', env: 'prod' } : undefined));
        expect(await loadS3Tags(invoke as never, '/a')).toEqual([{ key: 'env', value: 'prod' }, { key: 'team', value: 'ops' }]);
        await saveS3Tags(invoke as never, '/a', [{ key: ' env ', value: 'dev' }, { key: '', value: 'dropped' }]);
        expect(invoke).toHaveBeenCalledWith('s3_set_object_tags', { path: '/a', tags: { env: 'dev' } });
        await saveS3Tags(invoke as never, '/a', [{ key: '  ', value: 'x' }]);
        expect(invoke).toHaveBeenLastCalledWith('s3_delete_object_tags', { path: '/a' });
    });

    it('refuses more than ten tags before calling AWS', async () => {
        const invoke = vi.fn();
        const rows = Array.from({ length: 11 }, (_, i) => ({ key: `k${i}`, value: 'v' }));
        await expect(saveS3Tags(invoke as never, '/a', rows)).rejects.toThrow(/10/);
        expect(invoke).not.toHaveBeenCalled();
    });

    it('refuses a key given twice instead of keeping only one of the rows', async () => {
        const invoke = vi.fn();
        const rows = [{ key: 'env', value: 'prod' }, { key: ' env ', value: 'dev' }];
        await expect(saveS3Tags(invoke as never, '/a', rows)).rejects.toThrow(/env/);
        expect(invoke).not.toHaveBeenCalled();
    });

    it('keeps a __proto__ tag key instead of deleting every tag', async () => {
        // S3 allows the key; assigned on a plain object it vanished, the tag
        // set looked empty and the object's tags were deleted.
        const invoke = vi.fn();
        await saveS3Tags(invoke as never, '/a', [{ key: '__proto__', value: 'x' }]);
        expect(invoke).toHaveBeenCalledTimes(1);
        const [command, args] = invoke.mock.calls[0] as [string, { tags: Record<string, string> }];
        expect(command).toBe('s3_set_object_tags');
        expect(JSON.stringify(args.tags)).toBe('{"__proto__":"x"}');
    });

    it('offers a restore only for objects that need one', () => {
        expect(needsGlacierRestore('GLACIER')).toBe(true);
        expect(needsGlacierRestore('DEEP_ARCHIVE')).toBe(true);
        expect(needsGlacierRestore('GLACIER_IR')).toBe(false);
        expect(needsGlacierRestore(undefined)).toBe(false);
    });

    it('offers exactly what the backend allow-lists accept', () => {
        const rustList = (src: string, name: string) => {
            const block = src.slice(src.indexOf(`pub const ${name}`), src.indexOf('];', src.indexOf(`pub const ${name}`)));
            return [...block.matchAll(/"([A-Za-z_]+)"/g)].map((m) => m[1]);
        };
        expect([...S3_STORAGE_CLASSES]).toEqual(rustList(s3Source, 'S3_TARGET_STORAGE_CLASSES'));
        expect([...S3_RESTORE_TIERS]).toEqual(rustList(s3Source, 'S3_RESTORE_TIERS'));
        expect([...AZURE_ACCESS_TIERS]).toEqual(rustList(azureSource, 'AZURE_ACCESS_TIERS'));
    });

    it('is reachable from the S3 and Azure context menus', () => {
        expect(appSource).toContain("mode: 's3-class'");
        expect(appSource).toContain("mode: 's3-restore'");
        expect(appSource).toContain("mode: 'azure-tier'");
        expect(appSource).toContain('setS3TagsTarget({ path: file.path');
    });
});
