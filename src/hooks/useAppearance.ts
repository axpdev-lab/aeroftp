// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useEffect, useState } from 'react';
import { isTauri } from '@tauri-apps/api/core';
import { emit, listen } from '@tauri-apps/api/event';
import { guardedUnlisten } from './useTauriListener';
import { APPEARANCE_CHANGED, THEME_KEY, SCHEDULE_KEY, readAppearance, resolveAppearance, validSchedule, type AppearanceMode, type Theme, type ThemeSchedule } from '../utils/appearance';

// Notifications contain no preferences or account data. Other windows re-read
// their own storage; a timer/focus refresh also covers a missed IPC notification.
export function notifyAppearanceChanged() {
    window.dispatchEvent(new Event(APPEARANCE_CHANGED));
    if (isTauri()) void emit(APPEARANCE_CHANGED).catch(() => {});
}

export function useAppearance({ ipcEvents = true }: { ipcEvents?: boolean } = {}) {
    const [appearance, setAppearance] = useState(readAppearance);
    const [clock, setClock] = useState(() => ({ now: new Date(), prefersDark: window.matchMedia('(prefers-color-scheme: dark)').matches }));
    useEffect(() => {
        let disposed = false;
        const reload = () => {
            if (disposed) return;
            setAppearance(readAppearance());
            setClock({ now: new Date(), prefersDark: window.matchMedia('(prefers-color-scheme: dark)').matches });
        };
        const storageChanged = (event: StorageEvent) => {
            if (event.key === null || event.key === THEME_KEY || event.key === SCHEDULE_KEY) reload();
        };
        const media = window.matchMedia('(prefers-color-scheme: dark)');
        media.addEventListener('change', reload);
        window.addEventListener(APPEARANCE_CHANGED, reload);
        window.addEventListener('aeroftp-localstorage-restored', reload);
        window.addEventListener('focus', reload);
        window.addEventListener('pageshow', reload);
        window.addEventListener('storage', storageChanged);
        document.addEventListener('visibilitychange', reload);
        const timer = window.setInterval(reload, 60_000);
        const off = ipcEvents && isTauri() ? guardedUnlisten(listen(APPEARANCE_CHANGED, reload).catch(() => () => {})) : () => {};
        return () => {
            disposed = true;
            window.clearInterval(timer);
            off();
            media.removeEventListener('change', reload);
            window.removeEventListener(APPEARANCE_CHANGED, reload);
            window.removeEventListener('aeroftp-localstorage-restored', reload);
            window.removeEventListener('focus', reload);
            window.removeEventListener('pageshow', reload);
            window.removeEventListener('storage', storageChanged);
            document.removeEventListener('visibilitychange', reload);
        };
    }, [ipcEvents]);
    const effectiveTheme = resolveAppearance(appearance.preference, appearance.schedule, clock.prefersDark, clock.now);
    const setTheme = useCallback((theme: Theme) => {
        // Explicit selection always stops automation, retaining its parameters.
        const current = readAppearance();
        localStorage.setItem(SCHEDULE_KEY, JSON.stringify({ ...current.schedule, enabled: false }));
        localStorage.setItem(THEME_KEY, theme);
        notifyAppearanceChanged();
    }, []);
    const setSchedule = useCallback((schedule: ThemeSchedule) => {
        if (!validSchedule(schedule)) return;
        localStorage.setItem(SCHEDULE_KEY, JSON.stringify(schedule));
        notifyAppearanceChanged();
    }, []);
    const setMode = useCallback((mode: AppearanceMode) => {
        const current = readAppearance();
        if (mode === 'schedule') setSchedule({ ...current.schedule, enabled: true });
        else setTheme(mode === 'system' ? 'auto' : current.preference === 'auto'
            ? resolveAppearance(current.preference, { ...current.schedule, enabled: false }, window.matchMedia('(prefers-color-scheme: dark)').matches)
            : current.preference);
    }, [setTheme, setSchedule]);
    const mode: AppearanceMode = appearance.schedule.enabled ? 'schedule' : appearance.preference === 'auto' ? 'system' : 'manual';
    // Keep the legacy Theme contract: automation never reaches a palette consumer.
    const theme: Theme = mode === 'schedule' ? effectiveTheme : appearance.preference;
    return { theme, effectiveTheme, mode, schedule: appearance.schedule, setTheme, setMode, setSchedule };
}
