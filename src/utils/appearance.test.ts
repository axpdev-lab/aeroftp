// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { describe, expect, it } from 'vitest';
import { DEFAULT_SCHEDULE, getEffectiveTheme, isDarkTheme, parseSchedule, parseTheme, resolveAppearance, validSchedule } from './appearance';
const at = (time: string) => new Date(`2026-10-08T${time}:00`);
describe('appearance resolver', () => {
    it.each([['06:59', 'dark'], ['07:00', 'light'], ['18:59', 'light'], ['19:00', 'dark'], ['00:00', 'dark']])('resolves overnight boundary %s to %s', (time, expected) => {
        expect(resolveAppearance('cyber', { ...DEFAULT_SCHEDULE, enabled: true }, false, at(time))).toBe(expected);
    });
    it('handles same-day night ranges and custom palettes', () => {
        const schedule = { ...DEFAULT_SCHEDULE, enabled: true, start: '08:15', end: '10:45', nightTheme: 'redhorse' as const, dayTheme: 'ice' as const };
        expect(resolveAppearance('auto', schedule, true, at('08:14'))).toBe('ice');
        expect(resolveAppearance('auto', schedule, true, at('08:15'))).toBe('redhorse');
        expect(resolveAppearance('auto', schedule, true, at('10:45'))).toBe('ice');
    });
    it('preserves legacy manual and system preferences without a schedule', () => {
        expect(resolveAppearance('green', DEFAULT_SCHEDULE, false, at('20:00'))).toBe('green');
        expect(getEffectiveTheme('auto', true)).toBe('dark');
        expect(parseTheme('truedark')).toBe('truedark');
        expect(parseTheme('unexpected')).toBe('auto');
    });
    it('rejects equal and invalid times, and repairs imported malformed schedules', () => {
        for (const start of ['', '24:00', '9:00', '12:60', '07:00']) expect(validSchedule({ start, end: '07:00' })).toBe(false);
        expect(parseSchedule('{invalid')).toEqual(DEFAULT_SCHEDULE);
        expect(parseSchedule('{"enabled":true,"start":"07:00","end":"07:00","dayTheme":"auto"}')).toEqual({ ...DEFAULT_SCHEDULE, enabled: true });
    });
    it.each(['dark', 'truedark', 'tokyo', 'cyber', 'green', 'redhorse'] as const)('classifies %s as dark', theme => expect(isDarkTheme(theme)).toBe(true));
    it('classifies Ice and Light as light', () => { expect(isDarkTheme('ice')).toBe(false); expect(isDarkTheme('light')).toBe(false); });
});
