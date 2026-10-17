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
            // "Prod (2)" is asked for and free: it is kept, the numbers go around it.
            'Staging', 'Prod (3)', 'PROD (4)', 'Staging (1)', 'Prod (2)', 'lab ',
        ]);
    });

    it('never gives a number that is a name another profile keeps', () => {
        const previous = [{ id: 'a', name: 'Lab' }, { id: 'b', name: 'Lab' }, { id: 'c', name: 'Lab (1)' }];
        expect(namesOf(uniqueProfileNames(previous, previous))).toEqual(['Lab', 'Lab (2)', 'Lab (1)']);
        const older = previous.slice(0, 2);
        expect(namesOf(uniqueProfileNames(older, [...older, { id: 'x', name: 'Lab (1)' }]))).toEqual(['Lab', 'Lab (2)', 'Lab (1)']);
    });

    it('never changes a name only one profile has, and leaves no two names equal', () => {
        const pool = ['Lab', 'lab', 'Lab (1)', 'Lab (2)', 'Prod'];
        for (let x = 0; x < 125; x++) {
            const picked = [pool[x % 5], pool[Math.floor(x / 5) % 5], pool[Math.floor(x / 25) % 5]];
            const list = picked.map((name, i) => ({ id: String(i), name }));
            for (let kept = 0; kept <= list.length; kept++) {
                const out = namesOf(uniqueProfileNames(list.slice(0, kept), list)) as string[];
                expect(new Set(out.map(n => n.toLowerCase())).size).toBe(list.length);
                picked.forEach((name, i) => {
                    if (picked.filter(n => n.toLowerCase() === name.toLowerCase()).length === 1) expect(out[i]).toBe(name);
                });
            }
        }
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
