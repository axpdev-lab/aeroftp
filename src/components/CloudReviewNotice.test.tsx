// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later

import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it, vi } from 'vitest';
import { CloudReviewNotice } from './CloudReviewNotice';
import en from '../i18n/locales/en.json';

vi.mock('../i18n', () => ({ useTranslation: () => (_key: string, args: { count: number }) =>
    en.translations.cloud.untrackedDestinationFiles.replace('{count}', String(args.count)) }));

describe('preserved AeroCloud destination files', () => {
    it('names all preserved files, counts only decisions, and explains the confirming route', () => {
        const html = renderToStaticMarkup(<CloudReviewNotice entries={[
            { path: 'phone/photo.jpg', reason: '', is_dir: false, needs_review: true },
            { path: 'phone/notes.txt', reason: '', is_dir: false, needs_review: true },
            { path: 'ordinary-skip', reason: '', is_dir: false },
        ]} />);
        expect(html).toContain('role="alert"');
        expect(html).toContain('2 destination files');
        expect(html).toContain('phone/photo.jpg');
        expect(html).toContain('phone/notes.txt');
        expect(html).not.toContain('ordinary-skip');
        expect(html).toContain('AeroSync');
        expect(html).toContain('Mirror with One-way Update');
        expect(html).toContain('confirm any intended deletions');
    });

    it('displays safety errors even without preserved files or a changed sync status', () => {
        const html = renderToStaticMarkup(<CloudReviewNotice entries={[]} errors={['Blocked 20 deletes while conflicts remain']} />);
        expect(html).toContain('role="alert"');
        expect(html).toContain('Blocked 20 deletes while conflicts remain');
    });

    it('leaves old and ordinary results without a warning', () => {
        expect(renderToStaticMarkup(<CloudReviewNotice entries={[]} />)).toBe('');
        expect(renderToStaticMarkup(<CloudReviewNotice entries={[
            { path: 'folder', reason: 'directory kept', is_dir: true },
        ]} />)).toBe('');
    });
});
