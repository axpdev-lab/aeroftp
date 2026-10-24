import { expect, it } from 'vitest';
import type { OpenRequest } from '../types';
import { planLocalOpen } from './openRequestPlan';

const file = (folder: string, name: string): OpenRequest => ({
    kind: 'local', path: `${folder}/${name}`, folder, name, isDir: false,
});
const dir = (path: string): OpenRequest => ({ kind: 'local', path, folder: path, name: '', isDir: true });
const images = (name: string) => name.endsWith('.png');

it('selects every file of a one-folder selection and previews the first picture', () => {
    expect(planLocalOpen([file('/a', 'notes.bin'), file('/a', 'x.png'), file('/a', 'y.png')], images)).toEqual({
        folder: '/a', names: ['notes.bin', 'x.png', 'y.png'], preview: 'x.png', otherFolders: [],
    });
});

it('keeps every other folder of the batch for a tab of its own', () => {
    const plan = planLocalOpen([file('/a', 'x.png'), file('/b', 'y.png'), dir('/c'), file('/b', 'z.png')], images);
    expect(plan).toEqual({ folder: '/a', names: ['x.png'], preview: 'x.png', otherFolders: ['/b', '/c'] });
});

it('opens a folder on itself with nothing selected', () => {
    expect(planLocalOpen([dir('/c')], images)).toEqual({ folder: '/c', names: [], preview: null, otherFolders: [] });
    expect(planLocalOpen([], images)).toBeNull();
});
