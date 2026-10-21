import { describe, it, expect, vi } from 'vitest';
import { matchCanonicalName, resolveCanonicalNavigation } from './canonicalNames';
import { compareEntries } from './compareEndpoints';
import { adaptFileComparisons } from './recursiveCompare';
import { buildMirrorSyncInput } from './presetToSyncRun';
import { filesFromJournal } from './remoteSyncRunner';
import type { SyncJournal } from '../types';

const nfc = 'εσωτερικός';
const nfd = nfc.normalize('NFD');
const dir = (name: string) => ({ name, isDir: true });

describe('canonical Unicode correspondence (#1326)', () => {
    it.each([nfc, 'café', '🇯🇵/café'])('compares canonical file spellings: %s', name => {
        const left = { name: name.normalize('NFD'), isDir: false, size: 8, mtimeMs: 1000 };
        const right = { ...left, name: name.normalize('NFC') };
        const result = compareEntries([left], [right]);
        expect(result.stats.same.count).toBe(1);
        expect(result.entries[0].leftRelativePath).toBe(left.name);
        expect(result.entries[0].rightRelativePath).toBe(right.name);
    });
    it('compares empty directories and retains exact twins', () => {
        expect(compareEntries([dir(nfd)], [dir(nfc)]).stats.same.count).toBe(1);
        const result = compareEntries([dir(nfd), dir(nfc)], [dir(nfd), dir(nfc)]);
        expect(result.stats.same.count).toBe(2);
    });
    it('prefers exact pairs and blocks the unmatched twin', () => {
        const result = compareEntries([dir(nfc), dir(nfd)], [dir(nfc)]);
        expect(result.stats.same.count).toBe(1);
        expect(result.buckets.conflict[0].nameMatchBlocked).toBe(true);
        expect(buildMirrorSyncInput(result.buckets.conflict, 'left', true).files).toEqual([]);
    });
    it('records null sizes and times on a blocked canonical row', () => {
        const left = { name: nfd, isDir: false, size: 8, mtimeMs: 1000 };
        const exact = { name: nfc, isDir: false, size: 4, mtimeMs: 2000 };
        const blocked = compareEntries([left, exact], [exact]).buckets.conflict
            .find(row => row.nameMatchBlocked);
        expect(blocked).toMatchObject({
            leftSize: 8,
            rightSize: null,
            leftMtimeMs: 1000,
            rightMtimeMs: null,
        });
    });
    it('does not collapse compatibility characters or case', () => {
        expect(matchCanonicalName('Ａ', [dir('Ａ')], [dir('A')])).toBeUndefined();
        expect(matchCanonicalName('a', [dir('a')], [dir('A')])).toBeUndefined();
    });
    it('preserves distinct paths through recursive adapter, transfer and resume', () => {
        const row = adaptFileComparisons([{
            relative_path: `${nfd}/a.txt`, local_relative_path: `${nfd}/a.txt`, remote_relative_path: `${nfc}/a.txt`,
            status: 'local_newer', is_dir: false, sync_reason: '',
            local_info: { name: 'a.txt', path: `/local/${nfd}/a.txt`, size: 8, modified: null, is_dir: false, checksum: null },
            remote_info: { name: 'a.txt', path: `/remote/${nfc}/a.txt`, size: 4, modified: null, is_dir: false, checksum: null },
        }], true).entries[0];
        const input = buildMirrorSyncInput([row], 'left', true);
        expect(input.files[0]).toMatchObject({ relativePath: `${nfc}/a.txt`, sourcePath: `${nfd}/a.txt`, action: 'upload' });
        expect(filesFromJournal({ entries: [{ relative_path: `${nfc}/a.txt`, source_path: `${nfd}/a.txt`, action: 'upload' }] } as SyncJournal)[0])
            .toMatchObject({ relativePath: `${nfc}/a.txt`, sourcePath: `${nfd}/a.txt` });
    });
    it.each([true, false])('resolves actual destination spelling in both directions (%s)', async reverse => {
        const source = reverse ? nfc : nfd;
        const dest = reverse ? nfd : nfc;
        const from = vi.fn(async (p: string) => p === '/source' ? [dir('🇯🇵')] : [dir(source)]);
        const to = vi.fn(async (p: string) => p === '/dest' ? [dir('🇯🇵')] : [dir(dest)]);
        expect(await resolveCanonicalNavigation(`🇯🇵/${source}`, '/source', '/dest', from, to))
            .toEqual({ path: `/dest/🇯🇵/${dest}`, missing: false });
    });
    it('adds a new child under the destination parent spelling', async () => {
        const list = (parent: string, spelling: string) => async (p: string) => p === parent ? [dir(spelling)] : [];
        expect(await resolveCanonicalNavigation(`${nfd}/new`, '/s', '/d', list('/s', nfd), list('/d', nfc)))
            .toEqual({ path: `/d/${nfc}/new`, missing: true });
    });
    it('rejects ambiguous ancestors, listing failures and file collisions', async () => {
        const from = async () => [dir(nfc), dir(nfd)];
        await expect(resolveCanonicalNavigation(nfd, '/s', '/d', from, async () => [dir(nfc)] )).rejects.toThrow('Ambiguous');
        await expect(resolveCanonicalNavigation(nfd, '/s', '/d', from, async () => { throw Error('offline'); })).rejects.toThrow('offline');
        await expect(resolveCanonicalNavigation(nfd, '/s', '/d', async () => [dir(nfd)], async () => [{ name: nfc, isDir: false }])).rejects.toThrow('not a directory');
    });
});
