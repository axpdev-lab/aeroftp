// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect, useId, useState } from 'react';
import { useTranslation } from '../../i18n';
import { useAppearance } from '../../hooks/useAppearance';
import { PALETTES, validSchedule, type AppearanceMode, type EffectiveTheme } from '../../utils/appearance';

export function AppearanceModeControls() {
    const t = useTranslation();
    const id = useId();
    const { mode, schedule, setMode, setSchedule } = useAppearance();
    const [start, setStart] = useState(schedule.start);
    const [end, setEnd] = useState(schedule.end);
    useEffect(() => { setStart(schedule.start); setEnd(schedule.end); }, [schedule.start, schedule.end]);
    const valid = validSchedule({ start, end });
    const themeLabel = (theme: EffectiveTheme) => t(`settings.theme${theme === 'redhorse' ? 'Redlava' : theme[0].toUpperCase() + theme.slice(1)}Label`);
    return (
        <fieldset className="space-y-3">
            <legend className="text-sm font-medium mb-2">{t('settings.appearanceMode')}</legend>
            <div className="flex flex-wrap gap-4">
                {(['manual', 'system', 'schedule'] as AppearanceMode[]).map(value => (
                    <label key={value} className="flex items-center gap-2 text-sm">
                        <input type="radio" name={`${id}-mode`} value={value} checked={mode === value} onChange={() => setMode(value)} />
                        {t(value === 'manual' ? 'settings.appearanceManual' : value === 'system' ? 'settings.autoTheme' : 'settings.appearanceSchedule')}
                    </label>
                ))}
            </div>
            {mode === 'system' && <p className="text-xs text-gray-500">{t('settings.autoThemeDesc')}</p>}
            {mode === 'schedule' && (
                <div className="space-y-3 rounded-lg border border-gray-200 dark:border-gray-700 p-3">
                    <p className="text-xs text-gray-500">{t('settings.scheduleLocalTime')}</p>
                    <div className="grid grid-cols-2 gap-3">
                        {(['start', 'end'] as const).map(boundary => (
                            <label key={boundary} className="text-sm space-y-1">
                                <span className="block">{t(boundary === 'start' ? 'settings.scheduleNightStart' : 'settings.scheduleNightEnd')}</span>
                                <input type="time" step={60} value={boundary === 'start' ? start : end} aria-invalid={!valid} aria-describedby={!valid ? `${id}-error` : undefined}
                                    className="w-full px-2 py-1 rounded border border-gray-300 dark:border-gray-600 bg-transparent"
                                    onChange={event => {
                                        const nextStart = boundary === 'start' ? event.target.value : start;
                                        const nextEnd = boundary === 'end' ? event.target.value : end;
                                        setStart(nextStart); setEnd(nextEnd);
                                        if (validSchedule({ start: nextStart, end: nextEnd })) setSchedule({ ...schedule, start: nextStart, end: nextEnd });
                                    }} />
                            </label>
                        ))}
                        {(['dayTheme', 'nightTheme'] as const).map(period => (
                            <label key={period} className="text-sm space-y-1">
                                <span className="block">{t(period === 'dayTheme' ? 'settings.scheduleDayTheme' : 'settings.scheduleNightTheme')}</span>
                                <select value={schedule[period]} className="w-full px-2 py-1 rounded border border-gray-300 dark:border-gray-600 bg-[var(--color-bg-primary)]"
                                    onChange={event => setSchedule({ ...schedule, [period]: event.target.value as EffectiveTheme })}>
                                    {PALETTES.map(theme => <option key={theme} value={theme}>{themeLabel(theme)}</option>)}
                                </select>
                            </label>
                        ))}
                    </div>
                    {!valid && <p id={`${id}-error`} role="alert" className="text-xs text-red-500">{t('settings.scheduleInvalidTime')}</p>}
                    <button type="button" className="text-xs text-blue-500 hover:underline" onClick={() => setSchedule({ ...schedule, enabled: false })}>{t('settings.scheduleRestoreMode')}</button>
                </div>
            )}
        </fieldset>
    );
}
