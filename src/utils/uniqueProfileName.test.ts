import { describe, expect, it } from 'vitest';
import { withUniqueProfileNames } from './uniqueProfileName';

const named = (...names: string[]) => names.map((name, i) => ({ id: `p${i}`, name }));

describe('withUniqueProfileNames', () => {
    // The cases `src-tauri/src/util/profile_name.rs` holds the CLI to.
    it('gives a taken name the first free number', () => {
        const out = withUniqueProfileNames(named('Prod', 'prod (1)', 'Lab'), named('Staging', 'Prod', 'PROD', 'Staging', 'Prod (2)', 'lab '));
        expect(out.map(p => p.name)).toEqual(['Staging', 'Prod (2)', 'PROD (3)', 'Staging (1)', 'Prod (2) (1)', 'lab ']);
    });

    it('leaves a profile whose name is free untouched, and the existing list alone', () => {
        const existing = named('Proton API');
        const incoming = named('Other', 'Proton API');
        const out = withUniqueProfileNames(existing, incoming);
        expect(out[0]).toBe(incoming[0]);
        expect(out[1]).toEqual({ id: 'p1', name: 'Proton API (1)' });
        expect(incoming[1].name).toBe('Proton API');
        expect(existing[0].name).toBe('Proton API');
    });
});
