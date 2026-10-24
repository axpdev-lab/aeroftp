import { expect, it } from 'vitest';
import type { OpenRequest } from '../types';
import type { LocalTab } from '../types/aerofile';
import { addFolderTabs, findTabFor, mergeOpenBatch, openInFolderTab, planLocalOpen, sameLocalFolder, settleReusedTab, takeTabSelection } from './openRequestPlan';

const file = (folder: string, name: string): OpenRequest => ({
    kind: 'local', path: `${folder}/${name}`, folder, name, isDir: false,
});
const dir = (path: string): OpenRequest => ({ kind: 'local', path, folder: path, name: '', isDir: true });
const images = (name: string) => name.endsWith('.png');

it('selects every file of a one-folder selection and previews the first picture', () => {
    expect(planLocalOpen([file('/a', 'notes.bin'), file('/a', 'x.png'), file('/a', 'y.png')], images)).toEqual([
        { folder: '/a', names: ['notes.bin', 'x.png', 'y.png'], preview: 'x.png' },
    ]);
});

it('keeps the files of every other folder for the tab that opens it', () => {
    const plan = planLocalOpen([file('/a', 'x.png'), file('/b', 'y.png'), dir('/c'), file('/b', 'z.txt')], images);
    expect(plan).toEqual([
        { folder: '/a', names: ['x.png'], preview: 'x.png' },
        { folder: '/b', names: ['y.png', 'z.txt'], preview: 'y.png' },
        { folder: '/c', names: [], preview: null },
    ]);
});

it('opens a folder on itself with nothing selected', () => {
    expect(planLocalOpen([dir('/c')], images)).toEqual([{ folder: '/c', names: [], preview: null }]);
    expect(planLocalOpen([], images)).toEqual([]);
});

it('applies a tab selection once and keeps every other tab as it is', () => {
    const tabs: LocalTab[] = [
        { id: 'a', path: '/a', label: 'a', scrollTop: 0 },
        { id: 'b', path: '/b', label: 'b', scrollTop: 0, select: ['y.png', 'z.txt'] },
    ];
    const first = takeTabSelection(tabs, 'b');
    expect(first.select).toEqual(['y.png', 'z.txt']);
    expect(first.tabs).toEqual([tabs[0], { id: 'b', path: '/b', label: 'b', scrollTop: 0 }]);
    expect(takeTabSelection(first.tabs, 'b').select).toEqual([]);
    expect(takeTabSelection(tabs, 'a')).toEqual({ select: [], tabs });
});

it('joins the one-process-per-file batches of an Explorer multi-selection', () => {
    // Three processes a moment apart, as Explorer launches them with "%1".
    const first = { panelId: 'local', folder: 'C:\\Pics', names: ['a.png'], at: 1000 };
    const second = mergeOpenBatch(first, 'local', 'C:\\Pics', ['b.png'], 1400);
    expect(second).toEqual(['a.png', 'b.png']);
    expect(mergeOpenBatch({ ...first, names: second!, at: 1400 }, 'local', 'C:\\Pics', ['data.bin', 'a.png'], 2100))
        .toEqual(['a.png', 'b.png', 'data.bin']);
});

it('keeps a batch alone in another folder or panel, after the window, or for a folder', () => {
    const last = { panelId: 'local', folder: '/a', names: ['x.png'], at: 1000 };
    expect(mergeOpenBatch(last, 'local', '/b', ['y.png'], 1100)).toBeNull();
    expect(mergeOpenBatch(last, 'local2', '/a', ['y.png'], 1100)).toBeNull();
    expect(mergeOpenBatch(last, 'local', '/a', ['y.png'], 4001)).toBeNull();
    expect(mergeOpenBatch(last, 'local', '/a', [], 1100)).toBeNull();
    expect(mergeOpenBatch(null, 'local', '/a', ['y.png'], 1100)).toBeNull();
});

it('reuses the tab a folder already has instead of opening a second one', () => {
    const tabs: LocalTab[] = [
        { id: 'a', path: '/a', label: 'a', scrollTop: 0 },
        { id: 'b', path: '/b/', label: 'b', scrollTop: 0, select: ['y.png'] },
    ];
    expect(findTabFor(tabs, '/b')?.id).toBe('b');
    const newTab = (g: { folder: string; names: string[] }): LocalTab => ({ id: `new-${g.folder}`, path: g.folder, label: g.folder, scrollTop: 0, select: g.names });
    const next = addFolderTabs(tabs, [
        { folder: '/b', names: ['y.png', 'z.txt'], preview: 'y.png' },
        { folder: '/c', names: ['w.png'], preview: 'w.png' },
    ], newTab);
    expect(next.tabs.map(t => t.id)).toEqual(['a', 'b', 'new-/c']);
    expect(next.tabs[1].select).toEqual(['y.png', 'z.txt']);
    expect(next.skipped).toBe(0);
});

it('counts the folders left out at the tab limit', () => {
    const tabs: LocalTab[] = [{ id: 'a', path: '/a', label: 'a', scrollTop: 0 }];
    const newTab = (g: { folder: string }): LocalTab => ({ id: `new-${g.folder}`, path: g.folder, label: g.folder, scrollTop: 0 });
    const placed = addFolderTabs(tabs, [
        { folder: '/a', names: ['x.png'], preview: 'x.png' },
        { folder: '/b', names: [], preview: null },
        { folder: '/c', names: [], preview: null },
        { folder: '/d', names: [], preview: null },
    ], newTab, 2);
    expect(placed.tabs.map(t => t.id)).toEqual(['a', 'new-/b']);
    // The existing /a tab takes its files; /c and /d find no room.
    expect(placed.skipped).toBe(2);
});

it('gives the tab active before a reuse its own folder back', () => {
    // The panel reached /b while tab a was still active, so a followed it.
    const tabs: LocalTab[] = [
        { id: 'a', path: '/b', label: 'b', scrollTop: 0 },
        { id: 'b', path: '/b', label: 'b', scrollTop: 0, select: ['x.png'] },
    ];
    expect(settleReusedTab(tabs, 'b', { id: 'a', path: '/home/me/a' })).toEqual([
        { id: 'a', path: '/home/me/a', label: 'a', scrollTop: 0 },
        { id: 'b', path: '/b', label: 'b', scrollTop: 0 },
    ]);
    // Reusing the active tab itself changes nothing else.
    expect(settleReusedTab(tabs, 'b', { id: 'b', path: '/old' })[1]).toEqual({ id: 'b', path: '/b', label: 'b', scrollTop: 0 });
});

it('compares folders the way each platform names them', () => {
    // Windows: separators unified, case ignored, trailing separator dropped.
    expect(sameLocalFolder('C:\\Users\\Me\\Pics', 'c:/users/me/pics/')).toBe(true);
    expect(sameLocalFolder('C:\\Pics\\', 'C:\\Pics')).toBe(true);
    expect(sameLocalFolder('\\\\host\\share\\A', '\\\\HOST\\share\\a\\')).toBe(true);
    expect(sameLocalFolder('C:\\', 'c:/')).toBe(true);
    expect(sameLocalFolder('C:\\Pics', 'C:\\Pics2')).toBe(false);
    // Unix: a backslash is part of a name, and case matters.
    expect(sameLocalFolder('/home/me/b\\', '/home/me/b')).toBe(false);
    expect(sameLocalFolder('/home/me/b/', '/home/me/b')).toBe(true);
    expect(sameLocalFolder('/home/Me', '/home/me')).toBe(false);
    expect(sameLocalFolder('/', '/')).toBe(true);
    // The join and the tab reuse go through the same comparison.
    expect(mergeOpenBatch({ panelId: 'local', folder: 'C:\\Pics', names: ['a.png'], at: 0 }, 'local', 'c:/pics/', ['b.png'], 100))
        .toEqual(['a.png', 'b.png']);
    expect(findTabFor([{ id: 't', path: 'C:/Pics/', label: 'Pics', scrollTop: 0 }], 'c:\\pics')?.id).toBe('t');
});

it('keeps the files a tab was waiting to select when a later open reuses it', () => {
    // A batch made a tab for x in /b; y in /b is opened before the tab is shown.
    const tabs: LocalTab[] = [
        { id: 'a', path: '/a', label: 'a', scrollTop: 0 },
        { id: 'b', path: '/b', label: 'b', scrollTop: 0, select: ['x.png'] },
    ];
    const opened = openInFolderTab(tabs, '/b/', ['y.png']);
    expect(opened.tab?.id).toBe('b');
    expect(opened.names).toEqual(['x.png', 'y.png']);
    expect(openInFolderTab(tabs, '/b', ['x.png', 'y.png']).names).toEqual(['x.png', 'y.png']);
    expect(opened.tabs[1]).toEqual({ id: 'b', path: '/b', label: 'b', scrollTop: 0 });
    expect(openInFolderTab(tabs, '/c', ['z.png'])).toEqual({ tab: undefined, names: ['z.png'], tabs });
});
