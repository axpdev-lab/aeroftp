// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { expect, it } from 'vitest';
import { DirtyApiKeyEdits, mergeAiSettingsDraft } from './aiSettingsDraft';
import { getDefaultAISettings } from '../types/ai';

it('completion of an old key write preserves the next human edit', async () => {
    const dirty = new DirtyApiKeyEdits();
    dirty.mark('p'); const first = dirty.capture('p')!;
    let release!: () => void;
    const write = new Promise<void>(resolve => { release = resolve; }).then(() => dirty.complete('p', first));
    dirty.mark('p'); const second = dirty.capture('p')!;
    release(); await write;
    expect(dirty.capture('p')).toBe(second);
    dirty.complete('p', second); expect(dirty.capture('p')).toBeUndefined();
});

it('merges unrelated human and agent advanced preferences in either save order', () => {
    const base = getDefaultAISettings();
    const human = { ...base, advancedSettings: { ...base.advancedSettings, topP: 0.3 } };
    const agent = { ...base, advancedSettings: { ...base.advancedSettings, temperature: 0.2 } };
    for (const [edited, latest] of [[human, agent], [agent, human]]) {
        expect(mergeAiSettingsDraft(base, edited, latest).advancedSettings).toMatchObject({ topP: 0.3, temperature: 0.2 });
    }
});

it('merges provider/model records by identity and strips keys from human drafts', () => {
    const base = getDefaultAISettings();
    base.providers = [{ id: 'p', name: 'Fixture', type: 'openai', baseUrl: '', apiKey: 'KEY-A',
        isEnabled: true, isDefault: true, createdAt: new Date(0), updatedAt: new Date(0) }];
    const human = { ...base, providers: base.providers.map(p => ({ ...p, name: 'Edited', apiKey: 'KEY-B' })) };
    const agent = { ...base, providers: base.providers.map(p => ({ ...p, isEnabled: false })) };
    const result = mergeAiSettingsDraft(base, human, agent);
    expect(result.providers[0]).toMatchObject({ name: 'Edited', isEnabled: false });
    expect(JSON.stringify(result)).not.toMatch(/KEY-A|KEY-B|apiKey/);
});

it('a human deletion affects only its record and preserves concurrent additions', () => {
    const base = getDefaultAISettings();
    base.providers = [{ id: 'p', name: 'Fixture', type: 'openai', baseUrl: '', isEnabled: true,
        isDefault: true, createdAt: new Date(0), updatedAt: new Date(0) }];
    const edited = { ...base, providers: [] };
    const latest = { ...base, providers: [...base.providers, { ...base.providers[0], id: 'new' }] };
    expect(mergeAiSettingsDraft(base, edited, latest).providers.map(p => p.id)).toEqual(['new']);
});
