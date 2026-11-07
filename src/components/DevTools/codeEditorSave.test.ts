// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it, vi } from 'vitest';

/**
 * Greptile on #1526: a save the app refused (the file's server is no longer
 * the live one) or that failed still cleared the editor's modified mark, so
 * the unsaved text looked saved. CodeEditor's own `handleSave` is run from
 * its source (Monaco is too heavy to mount for one handler).
 */
const handleSave = (onSave: (content: string) => Promise<void>) => {
    const src = readFileSync(resolve(process.cwd(), 'src/components/DevTools/CodeEditor.tsx'), 'utf8');
    const start = src.indexOf('const handleSave = async () => {');
    expect(start).toBeGreaterThan(-1);
    const end = src.indexOf('\n    };\n', start);
    const body = src.slice(start, end) + '\n    };\n    return handleSave;';
    const state = { hasChanges: true, saving: false, original: 'old' };
    const scope = {
        editorRef: { current: { getValue: () => 'new' } },
        onSave,
        setIsSaving: (value: boolean) => { state.saving = value; },
        setOriginalContent: (value: string) => { state.original = value; },
        setHasChanges: (value: boolean) => { state.hasChanges = value; },
    };
    const run = new Function('scope', `with (scope) { ${body} }`)(scope) as () => Promise<void>;
    return { run, state };
};

describe('the code editor save', () => {
    it('keeps the buffer modified when the save is refused or fails', async () => {
        const refused = handleSave(vi.fn(async () => { throw new Error('toast.saveOnAnotherServer'); }));
        await refused.run();
        expect(refused.state.hasChanges).toBe(true);
        expect(refused.state.original).toBe('old');
        expect(refused.state.saving).toBe(false);
    });

    it('marks the buffer saved once the save went through', async () => {
        const saved = handleSave(vi.fn(async () => undefined));
        await saved.run();
        expect(saved.state.hasChanges).toBe(false);
        expect(saved.state.original).toBe('new');
    });
});
