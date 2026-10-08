// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

export const PALETTES = ['light', 'dark', 'truedark', 'tokyo', 'cyber', 'green', 'ice', 'redhorse'] as const;
export type EffectiveTheme = typeof PALETTES[number];
export type Theme = EffectiveTheme | 'auto';
export type AppearanceMode = 'manual' | 'system' | 'schedule';
export interface ThemeSchedule {
    enabled: boolean;
    start: string;
    end: string;
    dayTheme: EffectiveTheme;
    nightTheme: EffectiveTheme;
}
export const THEME_KEY = 'aeroftp-theme';
export const SCHEDULE_KEY = 'aeroftp-theme-schedule';
export const APPEARANCE_CHANGED = 'aeroftp-appearance-changed';
export const DEFAULT_SCHEDULE: ThemeSchedule = { enabled: false, start: '19:00', end: '07:00', dayTheme: 'light', nightTheme: 'dark' };

export function isPalette(value: unknown): value is EffectiveTheme {
    return PALETTES.includes(value as EffectiveTheme);
}
export function parseTheme(value: unknown): Theme {
    return value === 'auto' || isPalette(value) ? value : 'auto';
}
export function timeMinutes(value: string): number | null {
    if (!/^([01]\d|2[0-3]):[0-5]\d$/.test(value)) return null;
    const [hour, minute] = value.split(':').map(Number);
    return hour * 60 + minute;
}
export function validSchedule(schedule: Pick<ThemeSchedule, 'start' | 'end'>): boolean {
    return timeMinutes(schedule.start) !== null && timeMinutes(schedule.end) !== null && schedule.start !== schedule.end;
}
export function parseSchedule(raw: string | null): ThemeSchedule {
    try {
        const value = JSON.parse(raw ?? 'null');
        if (!value || typeof value !== 'object') return { ...DEFAULT_SCHEDULE };
        return {
            enabled: value.enabled === true,
            start: validSchedule(value) ? value.start : DEFAULT_SCHEDULE.start,
            end: validSchedule(value) ? value.end : DEFAULT_SCHEDULE.end,
            dayTheme: isPalette(value.dayTheme) ? value.dayTheme : DEFAULT_SCHEDULE.dayTheme,
            nightTheme: isPalette(value.nightTheme) ? value.nightTheme : DEFAULT_SCHEDULE.nightTheme,
        };
    } catch { return { ...DEFAULT_SCHEDULE }; }
}
export function getEffectiveTheme(theme: Theme, prefersDark: boolean): EffectiveTheme {
    return theme === 'auto' ? (prefersDark ? 'dark' : 'light') : theme;
}
export function resolveAppearance(theme: Theme, schedule: ThemeSchedule, prefersDark: boolean, now = new Date()): EffectiveTheme {
    if (!schedule.enabled || !validSchedule(schedule)) return getEffectiveTheme(theme, prefersDark);
    const start = timeMinutes(schedule.start)!;
    const end = timeMinutes(schedule.end)!;
    const minute = now.getHours() * 60 + now.getMinutes();
    const night = start < end ? minute >= start && minute < end : minute >= start || minute < end;
    return night ? schedule.nightTheme : schedule.dayTheme;
}
export function isDarkTheme(theme: EffectiveTheme): boolean {
    return theme !== 'light' && theme !== 'ice';
}
export function readAppearance() {
    return { preference: parseTheme(localStorage.getItem(THEME_KEY)), schedule: parseSchedule(localStorage.getItem(SCHEDULE_KEY)) };
}
export function applyThemeClasses(theme: EffectiveTheme) {
    const html = document.documentElement;
    html.classList.toggle('dark', isDarkTheme(theme));
    for (const palette of PALETTES) {
        if (palette !== 'light' && palette !== 'dark') html.classList.toggle(palette, theme === palette);
    }
}
