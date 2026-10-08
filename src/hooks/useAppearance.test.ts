// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { act, createElement as h } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { beforeEach, afterEach, expect, it, vi } from 'vitest';
import { useTheme, getLogTheme, getMonacoTheme } from './useTheme';
import { IconThemeProvider, useIconTheme } from './useIconTheme';
import { DEFAULT_SCHEDULE, SCHEDULE_KEY, THEME_KEY } from '../utils/appearance';
vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => false }));
let api: ReturnType<typeof useTheme>;
let iconApi: ReturnType<typeof useIconTheme>;
let root: Root;
let host: HTMLDivElement;
const media = { matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() };
function Harness() { api = useTheme(); iconApi = useIconTheme(); return h('div', {}, api.effectiveTheme); }
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    vi.useFakeTimers(); vi.setSystemTime(new Date('2026-10-08T18:59:00'));
    localStorage.clear(); document.documentElement.className = '';
    vi.stubGlobal('matchMedia', () => media);
    vi.stubGlobal('requestAnimationFrame', () => 1); vi.stubGlobal('cancelAnimationFrame', vi.fn());
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.clearAllMocks(); });
const mount = async () => { await act(async () => root.render(h(IconThemeProvider, { children: h(Harness) }))); };
it.each(['auto', 'green'] as const)('restores the previous %s preference and retains parameters after an explicit override', async preference => {
    localStorage.setItem(THEME_KEY, preference); await mount();
    await act(async () => api.setMode('schedule'));
    expect(api.mode).toBe('schedule'); expect(localStorage.getItem(THEME_KEY)).toBe(preference);
    await act(async () => api.setSchedule({ ...api.schedule, enabled: false }));
    expect(api.theme).toBe(preference); expect(api.mode).toBe(preference === 'auto' ? 'system' : 'manual');
    await act(async () => { api.setMode('schedule'); });
    await act(async () => api.setTheme('ice'));
    expect(api.mode).toBe('manual'); expect(api.effectiveTheme).toBe('ice');
    expect(JSON.parse(localStorage.getItem(SCHEDULE_KEY)!)).toEqual(DEFAULT_SCHEDULE);
});
it('initializes secondary windows and editor/log consumers from the schedule, then reevaluates the boundary', async () => {
    localStorage.setItem(SCHEDULE_KEY, JSON.stringify({ ...DEFAULT_SCHEDULE, enabled: true, nightTheme: 'truedark' }));
    await mount(); expect(api.effectiveTheme).toBe('light');
    await act(async () => vi.advanceTimersByTime(60_000));
    expect(api.effectiveTheme).toBe('truedark'); expect(document.documentElement.classList.contains('truedark')).toBe(true);
    expect(getLogTheme(api.theme, api.isDark)).toBe('truedark'); expect(getMonacoTheme(api.theme, api.isDark)).toBe('truedark');
});
it('catches up on resume, imported settings and storage changes without resetting an explicit icon override', async () => {
    localStorage.setItem('aeroftp-icon-theme', 'outline'); await mount();
    localStorage.setItem(SCHEDULE_KEY, JSON.stringify({ ...DEFAULT_SCHEDULE, enabled: true, nightTheme: 'cyber' }));
    vi.setSystemTime(new Date('2026-10-08T20:00:00'));
    await act(async () => window.dispatchEvent(new Event('focus')));
    expect(api.effectiveTheme).toBe('cyber'); expect(iconApi.iconTheme).toBe('outline');
    localStorage.setItem(THEME_KEY, 'ice'); localStorage.removeItem(SCHEDULE_KEY);
    await act(async () => window.dispatchEvent(new Event('aeroftp-localstorage-restored')));
    expect(api.theme).toBe('ice'); expect(document.documentElement.classList.contains('cyber')).toBe(false);
    localStorage.setItem(THEME_KEY, 'redhorse');
    await act(async () => window.dispatchEvent(new StorageEvent('storage', { key: THEME_KEY })));
    expect(api.theme).toBe('redhorse');
});
it('lets unchosen icon defaults follow scheduled special palettes and cleans up timers/listeners', async () => {
    const removed = vi.spyOn(window, 'removeEventListener');
    const intervals = vi.spyOn(window, 'setInterval');
    const cleared = vi.spyOn(window, 'clearInterval'); await mount();
    await act(async () => api.setSchedule({ ...DEFAULT_SCHEDULE, enabled: true, dayTheme: 'tokyo' }));
    expect(iconApi.iconTheme).toBe('minimal');
    await act(async () => root.unmount());
    for (const result of intervals.mock.results) expect(cleared).toHaveBeenCalledWith(result.value);
    expect(removed).toHaveBeenCalledWith('focus', expect.any(Function));
    expect(media.removeEventListener).toHaveBeenCalledWith('change', expect.any(Function));
    root = createRoot(host);
});
