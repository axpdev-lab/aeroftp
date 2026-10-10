import { describe, expect, it } from 'vitest';
import { buildGallery, galleryStep, parentDir, sortByName } from './gallery';

const f = (name: string, dir = '/photos', is_dir = false) => ({ name, path: `${dir}/${name}`, is_dir });

describe('preview gallery', () => {
    const folder = [f('trip'), f('b.JPG'), f('notes.pdf'), f('a.png'), f('c.webp'), f('readme.txt')].map((e, i) => (i === 0 ? { ...e, is_dir: true } : e));

    it('pages the pictures of the folder, whatever the case of the extension', () => {
        const g = buildGallery(f('b.JPG'), folder);
        expect(g?.files.map((e) => e.name)).toEqual(['b.JPG', 'a.png', 'c.webp']);
        expect(g?.index).toBe(0);
    });

    it('has nothing to page when the file is not in the list or is alone', () => {
        expect(buildGallery(f('x.png', '/elsewhere'), folder)).toBeNull();
        expect(buildGallery(f('notes.pdf'), folder)).toBeNull();
    });

    it('goes round at both ends', () => {
        expect(galleryStep(2, 3, 1)).toBe(0);
        expect(galleryStep(0, 3, -1)).toBe(2);
        expect(galleryStep(1, 3, 1)).toBe(2);
        expect(galleryStep(0, 0, 1)).toBe(-1);
    });

    it('reads a folder from disk in name order, numbers as numbers', () => {
        expect(sortByName([f('img10.jpg'), f('img2.jpg'), f('Img1.jpg')]).map((e) => e.name)).toEqual(['Img1.jpg', 'img2.jpg', 'img10.jpg']);
    });

    it('finds the folder of a Unix or a Windows path', () => {
        expect(parentDir('/home/u/a.jpg')).toBe('/home/u');
        expect(parentDir('/a.jpg')).toBe('/');
        expect(parentDir('C:\\Users\\u\\a.jpg')).toBe('C:\\Users\\u');
        expect(parentDir('C:\\a.jpg')).toBe('C:\\');
        expect(parentDir('a.jpg')).toBe('');
    });
});
