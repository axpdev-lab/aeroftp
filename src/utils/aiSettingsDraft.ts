// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { AISettings } from '../types/ai';

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);
const record = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v) && !(v instanceof Date);

/** Apply only deliberately edited fields onto the latest queued public record. */
function merge(base: unknown, edited: unknown, latest: unknown): unknown {
    if (same(base, edited)) return latest;
    if (Array.isArray(base) && Array.isArray(edited) && Array.isArray(latest) &&
        [...base, ...edited, ...latest].every(v => record(v) && typeof v.id === 'string')) {
        const before = new Map(base.map(v => [v.id, v]));
        const after = new Map(edited.map(v => [v.id, v]));
        const result = latest.filter(v => !before.has(v.id) || after.has(v.id)).map(v =>
            before.has(v.id) && after.has(v.id) ? merge(before.get(v.id), after.get(v.id), v) : v);
        for (const v of edited) {
            if (!before.has(v.id) && !result.some(current => (current as { id: string }).id === v.id)) result.push(v);
        }
        return result;
    }
    if (record(base) && record(edited) && record(latest)) {
        const result = { ...latest };
        for (const key of new Set([...Object.keys(base), ...Object.keys(edited)])) {
            // Keys and prototype markers are never public draft changes.
            if (['apiKey', '__proto__', 'constructor', 'prototype'].includes(key)) continue;
            if (same(base[key], edited[key])) continue;
            if (!Object.prototype.hasOwnProperty.call(edited, key)) delete result[key];
            else result[key] = merge(base[key], edited[key], latest[key]);
        }
        return result;
    }
    return edited;
}

export function mergeAiSettingsDraft(base: AISettings, edited: AISettings, latest: AISettings): AISettings {
    const result = merge(base, edited, latest) as AISettings;
    return { ...result, providers: result.providers.map(p => ({ ...p, apiKey: undefined })) };
}

/** A completed write owns only the edit generation that it actually wrote. */
export class DirtyApiKeyEdits {
    private generation = 0;
    private readonly edits = new Map<string, number>();
    mark(id: string): void { this.edits.set(id, ++this.generation); }
    capture(id: string): number | undefined { return this.edits.get(id); }
    complete(id: string, generation: number): void {
        if (this.edits.get(id) === generation) this.edits.delete(id);
    }
}
