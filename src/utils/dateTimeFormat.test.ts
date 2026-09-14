import { afterEach, describe, expect, it, vi } from 'vitest';
import { createDateTimeFormatter } from './dateTimeFormat';
import { formatDate } from './formatters';

const options: Intl.DateTimeFormatOptions = {
    day: '2-digit', month: 'short', year: 'numeric',
    hour: '2-digit', minute: '2-digit', hour12: false,
};
function spyFormatter() {
    const Native = Intl.DateTimeFormat;
    return vi.spyOn(Intl, 'DateTimeFormat').mockImplementation(function (locales, options) {
        return new Native(locales, options);
    } as typeof Intl.DateTimeFormat);
}

const date = new Date('2026-09-14T10:30:00Z');

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.useRealTimers(); });

describe('date formatter reuse', () => {
    it('formats a file list with one ICU construction while preserving output', () => {
        const expected = new Intl.DateTimeFormat('it-IT', options).format(date);
        const constructor = spyFormatter();
        const format = createDateTimeFormatter(options);
        for (let i = 0; i < 1000; i++) expect(format(date, 'it-IT')).toBe(expected);
        expect(constructor).toHaveBeenCalledTimes(1);
    });

    it('changes language immediately and formats different dates with the reused formatter', () => {
        const format = createDateTimeFormatter(options);
        for (const locale of ['it-IT', 'en-US', 'ja-JP', 'it-IT']) {
            for (const input of [date, new Date('2025-01-02T01:00:00Z')]) {
                expect(format(input, locale)).toBe(new Intl.DateTimeFormat(locale, options).format(input));
            }
        }
    });

    it('renews on use after a minute or clock rollback, without scheduling work', () => {
        vi.useFakeTimers();
        vi.setSystemTime(100_000);
        const constructor = spyFormatter();
        const format = createDateTimeFormatter(options);
        format(date, 'it-IT');
        vi.setSystemTime(159_999);
        format(date, 'it-IT');
        expect(constructor).toHaveBeenCalledTimes(1);
        vi.setSystemTime(160_000);
        format(date, 'it-IT');
        expect(constructor).toHaveBeenCalledTimes(2);
        vi.setSystemTime(90_000);
        format(date, 'it-IT');
        expect(constructor).toHaveBeenCalledTimes(3);
        expect(vi.getTimerCount()).toBe(0);
    });

    it('keeps parsing and explicit date preferences independent of the cache', () => {
        const root = { lang: 'it-IT', dataset: { dateFormat: 'localized' } };
        vi.stubGlobal('document', { documentElement: root });
        expect(formatDate('2026-09-14T10:30:00Z')).toBe(new Intl.DateTimeFormat('it-IT', options).format(date));
        root.lang = 'en-US';
        expect(formatDate(date)).toBe(new Intl.DateTimeFormat('en-US', options).format(date));
        root.dataset.dateFormat = 'iso';
        expect(formatDate('2026-09-14 10:30:00')).toBe('2026-09-14 10:30:00');
        root.dataset.dateFormat = 'dmy';
        expect(formatDate('2026-09-14 10:30:00')).toBe('14/09/2026 10:30:00');
        root.dataset.dateFormat = 'mdy';
        expect(formatDate('2026-09-14 10:30:00')).toBe('09/14/2026 10:30:00');
        expect(formatDate(null)).toBe('');
        expect(formatDate('unparsed')).toBe('unparsed');
    });
});
