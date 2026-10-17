import { describe, expect, it } from 'vitest';
import { uniqueProfileNames } from './uniqueProfileName';

const named = (prefix: string, ...names: string[]) => names.map((name, i) => ({ id: `${prefix}${i}`, name }));
const namesOf = (list: readonly { name?: string }[]) => list.map(p => p.name);

// The cases `src-tauri/src/util/profile_name.rs` holds `unique_profile_names` to.
describe('uniqueProfileNames', () => {
    it('lets the profile that had the name keep it, wherever it sits in the list', () => {
        const previous = [{ id: 'a', name: 'Lab' }, { id: 'b', name: 'Prod' }];
        // "b" renamed to "lab", placed before "a", and a new "Lab" at the end.
        const profiles = [{ id: 'b', name: 'lab' }, { id: 'a', name: 'Lab' }, { id: 'c', name: 'Lab' }];
        expect(namesOf(uniqueProfileNames(previous, profiles))).toEqual(['lab (1)', 'Lab', 'Lab (2)']);
    });

    it('puts right an older list with two of one name on the next write', () => {
        const previous = [{ id: 'a', name: 'Proton API' }, { id: 'b', name: 'Proton API' }];
        const stored = uniqueProfileNames(previous, previous);
        expect(namesOf(stored)).toEqual(['Proton API', 'Proton API (1)']);
        const again = uniqueProfileNames(stored, stored);
        expect(again).toEqual(stored);
        again.forEach((p, i) => expect(p).toBe(stored[i]));
    });

    it('gives a taken name the first free number', () => {
        const held = named('p', 'Prod', 'prod (1)', 'Lab');
        const added = named('n', 'Staging', 'Prod', 'PROD', 'Staging', 'Prod (2)', 'lab ');
        const out = uniqueProfileNames(held, [...held, ...added]);
        expect(namesOf(out)).toEqual([
            'Prod', 'prod (1)', 'Lab',
            'Staging', 'Prod (2)', 'PROD (3)', 'Staging (1)', 'Prod (2) (1)', 'lab ',
        ]);
    });

    it('returns a kept profile as it was and leaves its inputs alone', () => {
        const previous = [{ id: 'a', name: 'Proton API' }];
        const profiles = [{ id: 'x', name: 'Other' }, { id: 'b', name: 'Proton API' }, { id: 'a', name: 'Proton API' }, { id: 'e' }];
        const out = uniqueProfileNames(previous, profiles);
        expect(out[0]).toBe(profiles[0]);
        expect(out[1]).toEqual({ id: 'b', name: 'Proton API (1)' });
        expect(out[2]).toBe(profiles[2]);
        expect(out[3]).toBe(profiles[3]);
        expect(profiles[1].name).toBe('Proton API');
        expect(previous[0].name).toBe('Proton API');
    });
});
