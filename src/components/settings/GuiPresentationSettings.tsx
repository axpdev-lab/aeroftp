// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { useTranslation } from '../../i18n';
import { useEffect, useState } from 'react';
import { MIN_GUI_SPEED, MAX_GUI_SPEED, isGuiSpeed, type GuiPresentationSettings as Preferences } from '../../gui/presentation';
function SpeedInput({ value, onCommit }: { value: number; onCommit: (value: number) => void }) {
    const [draft, setDraft] = useState(String(value));
    useEffect(() => setDraft(String(value)), [value]);
    return <input type="number" min={MIN_GUI_SPEED} max={MAX_GUI_SPEED} step={1} value={draft}
        onChange={event => setDraft(event.target.value)}
        onBlur={() => { const n = Number(draft); if (isGuiSpeed(n)) onCommit(n); else setDraft(String(value)); }}
        onKeyDown={event => { if (event.key === 'Enter') event.currentTarget.blur(); }}
        className="w-24 rounded border bg-transparent p-1" />;
}
export function GuiPresentationSettings({ value, onChange }: { value: Preferences; onChange: (value: Preferences) => void }) {
    const t = useTranslation();
    return <fieldset data-agent="deny" data-gui-presentation-settings className="space-y-3 rounded-lg border border-gray-200 p-4 dark:border-gray-700">
        <legend className="px-1 font-medium">{t('guiController.preferences')}</legend>
        <p className="text-xs text-gray-500">{t('guiController.preferencesDescription')}</p>
        <label className="flex items-center justify-between gap-2">{t('guiController.defaultSpeed')}
            <SpeedInput value={value.defaultSpeed} onCommit={defaultSpeed => onChange({ ...value, defaultSpeed })} />
        </label>
        <p className="text-sm font-medium">{t('guiController.presets')}</p>
        {value.presets.map((speed, i) => <label key={i} className="flex items-center justify-between gap-2">{t('guiController.preset', { index: i + 1 })}
            <SpeedInput value={speed} onCommit={n => {
                const presets: Preferences['presets'] = [...value.presets]; presets[i] = n; onChange({ ...value, presets }); }} />
        </label>)}
    </fieldset>;
}
