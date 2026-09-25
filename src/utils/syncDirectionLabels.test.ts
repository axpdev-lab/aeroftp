// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import en from '../i18n/locales/en.json';
import {
    planDirectionLabel,
    presetForTemplate,
    presetNameLabel,
    presetTaglineLabel,
    resolveLabel,
    templateDirectionLabel,
    type LabelRef,
} from './syncDirectionLabels';
import type { SyncPreset } from './syncPresets';

const lookup = (key: string): string => {
    const value = key.split('.').reduce<unknown>(
        (node, part) => (node && typeof node === 'object' ? (node as Record<string, unknown>)[part] : undefined),
        en.translations,
    );
    return typeof value === 'string' ? value : key;
};
const say = (label: LabelRef) => resolveLabel(lookup, label);

describe('one vocabulary for sync directions', () => {
    // A template stores `local_to_remote`; the Plan tab shows left/right. Both
    // must read the same to the user for the same pair.
    it('names the Plan toggle and the template field with the same words', () => {
        expect(say(planDirectionLabel('left-to-right', 'local-remote'))).toBe(say(templateDirectionLabel('local_to_remote')));
        expect(say(planDirectionLabel('right-to-left', 'local-remote'))).toBe(say(templateDirectionLabel('remote_to_local')));
        expect(say(planDirectionLabel('left-to-right', 'remote-local'))).toBe(say(templateDirectionLabel('remote_to_local')));
        expect(say(templateDirectionLabel('local_to_remote'))).toBe('Local → Remote');
        expect(say(templateDirectionLabel('bidirectional'))).toBe('Both ways');
    });

    it('keeps Left/Right for two local folders, where both sides are local', () => {
        expect(say(planDirectionLabel('left-to-right', 'local-local'))).toBe('Left to Right');
    });

    it('maps a template to the Plan mode it stands for', () => {
        expect(presetForTemplate('local_to_remote', true)).toBe('mirror');
        expect(presetForTemplate('local_to_remote', false)).toBe('backup');
        expect(presetForTemplate('bidirectional', true)).toBe('bisync');
    });

    it('has every label it uses in the English locale', () => {
        const presets: SyncPreset[] = ['mirror', 'backup', 'update', 'bisync'];
        const labels: LabelRef[] = [
            ...presets.flatMap((p) => [presetNameLabel(p), presetTaglineLabel(p)]),
            planDirectionLabel('left-to-right', 'local-remote'),
            planDirectionLabel('right-to-left', 'local-remote'),
            planDirectionLabel('left-to-right', 'local-local'),
            planDirectionLabel('right-to-left', 'local-local'),
            templateDirectionLabel('bidirectional'),
        ];
        for (const label of labels) expect(lookup(label.key), label.key).not.toBe(label.key);
    });

    it('falls back to English when `t` answers with the key itself', () => {
        expect(resolveLabel((k) => k, presetNameLabel('bisync'))).toBe('Two-way sync');
        expect(say(templateDirectionLabel('sideways'))).toBe('sideways');
    });
});
