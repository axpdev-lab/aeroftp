// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { copyNameInput, copyOutputPath } from './ImageSaveDialog';

// Deep-scan 81: the copy stays in the image's folder whatever name is typed.
describe('copyOutputPath', () => {
    it('keeps a plain name', () => {
        expect(copyOutputPath('/pics', 'holiday', 'orig', 'png')).toBe('/pics/holiday_edited.png');
    });

    it('reduces a name with separators or parent steps to its last component', () => {
        expect(copyOutputPath('/pics', '../../etc/evil', 'orig', 'jpg')).toBe('/pics/evil_edited.jpg');
        expect(copyOutputPath('/pics', 'sub\\..\\..\\x', 'orig', 'jpg')).toBe('/pics/x_edited.jpg');
        expect(copyOutputPath('/pics', '/abs/name', 'orig', 'webp')).toBe('/pics/name_edited.webp');
    });

    it('falls back to the original name for an empty or dot name', () => {
        for (const name of ['', '  ', '.', '..', 'a/..', 'x/']) {
            expect(copyOutputPath('/pics', name, 'orig', 'png')).toBe('/pics/orig_edited.png');
        }
    });

    it('falls back to the last component of a base name that holds a folder', () => {
        expect(copyOutputPath('/pics', '..', 'subdir/photo', 'png')).toBe('/pics/photo_edited.png');
        expect(copyOutputPath('/pics', '', 'a\\b', 'png')).toBe('/pics/b_edited.png');
    });

    it('keeps no separator in the typed name, so the name shown is the name used', () => {
        expect(copyNameInput('sub/holiday')).toBe('subholiday');
        expect(copyNameInput('..\\x')).toBe('..x');
        expect(copyNameInput('plain name')).toBe('plain name');
    });
});
