import { expect, it } from 'vitest';
import type { OpenRequest } from '../types';
import type { LocalTab } from '../types/aerofile';
import { planLocalOpen, takeTabSelection } from './openRequestPlan';

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
