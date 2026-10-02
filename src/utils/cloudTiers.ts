// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * S3 storage class, object tags and Glacier restore; Azure blob access tier.
 *
 * 3.1.4 shipped the backend commands and announced the features, including
 * an `S3TagsDialog` that never existed; the context menu reached none of them.
 * The lists mirror the backend allow-lists (`S3_TARGET_STORAGE_CLASSES`,
 * `S3_RESTORE_TIERS` in providers/s3.rs, `AZURE_ACCESS_TIERS` in
 * providers/azure.rs), which refuse anything else.
 */

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

export const S3_STORAGE_CLASSES = [
    'STANDARD',
    'INTELLIGENT_TIERING',
    'STANDARD_IA',
    'ONEZONE_IA',
    'GLACIER_IR',
    'GLACIER',
    'DEEP_ARCHIVE',
] as const;
export type S3StorageClass = (typeof S3_STORAGE_CLASSES)[number];

export const S3_RESTORE_TIERS = ['Expedited', 'Standard', 'Bulk'] as const;
export type S3RestoreTier = (typeof S3_RESTORE_TIERS)[number];

export const AZURE_ACCESS_TIERS = ['Hot', 'Cool', 'Cold', 'Archive'] as const;
export type AzureAccessTier = (typeof AZURE_ACCESS_TIERS)[number];

/** AWS caps an object at 10 tags. */
export const S3_MAX_TAGS = 10;

/** An object in these classes must be restored before it can be read. */
export function needsGlacierRestore(storageClass: string | undefined | null): boolean {
    return storageClass === 'GLACIER' || storageClass === 'DEEP_ARCHIVE';
}

export async function changeS3StorageClass(invoke: Invoke, path: string, storageClass: S3StorageClass): Promise<void> {
    await invoke('s3_change_storage_class', { path, storageClass });
}

export async function restoreFromGlacier(invoke: Invoke, path: string, days: number, tier: S3RestoreTier): Promise<void> {
    await invoke('s3_glacier_restore', { path, days: Math.trunc(days), tier });
}

export async function setAzureTier(invoke: Invoke, path: string, tier: AzureAccessTier): Promise<void> {
    await invoke('azure_set_blob_tier', { path, tier });
}

export interface TagRow {
    key: string;
    value: string;
}

export async function loadS3Tags(invoke: Invoke, path: string): Promise<TagRow[]> {
    const tags = await invoke<Record<string, string>>('s3_get_object_tags', { path });
    return Object.entries(tags ?? {})
        .map(([key, value]) => ({ key, value }))
        .sort((a, b) => a.key.localeCompare(b.key));
}

/**
 * Rows with an empty key are dropped. A key given twice is refused: the tags
 * of an object have unique keys, so writing both rows would keep one value and
 * lose the other without a word. An empty set removes the tagging
 * (`s3_delete_object_tags`) rather than writing an empty one.
 */
export async function saveS3Tags(invoke: Invoke, path: string, rows: TagRow[]): Promise<void> {
    const entries = new Map<string, string>();
    for (const { key, value } of rows) {
        const k = key.trim();
        if (!k) continue;
        if (entries.has(k)) {
            throw new Error(`Tag key "${k}" is used more than once`);
        }
        entries.set(k, value);
    }
    if (entries.size > S3_MAX_TAGS) {
        throw new Error(`S3 allows at most ${S3_MAX_TAGS} tags per object`);
    }
    if (entries.size === 0) {
        await invoke('s3_delete_object_tags', { path });
    } else {
        // fromEntries defines own properties, so a "__proto__" key is kept
        // where an assignment on a plain object would set the prototype.
        await invoke('s3_set_object_tags', { path, tags: Object.fromEntries(entries) });
    }
}
