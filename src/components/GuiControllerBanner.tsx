// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { useLayoutEffect, useState } from 'react';
import { Square, Bot, Pause, Play, MousePointer2 } from 'lucide-react';
import { useTranslation } from '../i18n';
import type { GuiLease } from '../gui/controller';
import { MIN_GUI_SPEED, MAX_GUI_SPEED, type GuiPresentationSettings } from '../gui/presentation';
import { presentationPoint } from '../gui/presentationTarget';

export function GuiControllerBanner({ lease, onStop, onSpeed, onPause, preferences }: {
    lease: GuiLease | null; onStop: () => void; onSpeed: (value: number) => void;
    onPause: (value: boolean) => void; preferences: GuiPresentationSettings;
}) {
    const t = useTranslation();
    const [point, setPoint] = useState({ x: 24, y: 24, fallback: true });
    useLayoutEffect(() => {
        if (!lease?.intent) return;
        const update = () => setPoint(presentationPoint(lease));
        // Let the initial cursor origin paint before its first movement.
        let frame = requestAnimationFrame(() => { frame = requestAnimationFrame(update); });
        window.addEventListener('resize', update); window.addEventListener('scroll', update, true);
        return () => { cancelAnimationFrame(frame); window.removeEventListener('resize', update); window.removeEventListener('scroll', update, true); };
    }, [lease]);
    if (!lease) return null;
    const control = lease.control;
    const trusted = (event: { nativeEvent: Event }) => import.meta.env.DEV || event.nativeEvent.isTrusted;
    const actionKey = lease.intent === 'connect' ? 'common.connect' : `guiController.actions.${lease.intent}`;
    return <>
        {<div data-gui-controller-cursor aria-hidden="true" className="pointer-events-none fixed z-[9999] text-amber-600 transition-[left,top] motion-reduce:transition-none"
            // Align the SVG pointer tip and the ripple centre with the semantic target.
            style={{ left: point.x, top: point.y, transform: 'translate(-4px, -4px)', transitionDuration: `${lease.durationMs ?? 0}ms` }}>
            {control?.phase === 'press' && <span className="absolute -left-4 -top-4 h-10 w-10 rounded-full border-2 border-amber-500 animate-ping motion-reduce:animate-none" />}
            <MousePointer2 size={26} fill="currentColor" stroke="white" />
        </div>}
        <div data-gui-controller-badge role="region" aria-live="polite" aria-label={t('guiController.preferences')}
            className="flex h-9 shrink-0 items-center gap-2 border-t border-gray-200 bg-gray-100 px-3 text-xs text-gray-600 select-none dark:border-gray-700 dark:bg-gray-800 dark:text-gray-400">
            <div className="flex min-w-0 flex-1 items-center gap-2 overflow-x-auto">
                <span data-gui-controller-agent className="inline-flex shrink-0 items-center gap-1.5 rounded bg-amber-400 px-2 py-0.5 font-medium text-amber-950">
                    <Bot size={14} aria-hidden="true" className="shrink-0" />
                    {t('guiController.banner', { agent: lease.owner.label })}
                </span>
                {lease.intent && <span className="shrink-0">{t(actionKey)}</span>}
                {control?.paused && <span className="shrink-0 font-semibold">{t('guiController.paused')}</span>}
                {lease.intent && point.fallback && <span className="shrink-0">{t('guiController.fallback')}</span>}
                {control && <>
                    <div className="h-4 w-px shrink-0 bg-gray-300 dark:bg-gray-600" />
                    <label className="flex shrink-0 items-center gap-2">
                        {t('guiController.speed')}
                        <input data-agent="deny" data-gui-controller-control type="range" min={MIN_GUI_SPEED} max={MAX_GUI_SPEED} step={1}
                            className="w-24" aria-label={t('guiController.speed')} value={control.speed_percent}
                            onChange={event => { if (trusted(event)) onSpeed(Number(event.target.value)); }} />
                        <span className="tabular-nums">{control.speed_percent}%</span>
                    </label>
                    <div className="h-4 w-px shrink-0 bg-gray-300 dark:bg-gray-600" />
                    <span className="shrink-0">{t(`guiController.speedSource.${control.speed_source}`)}:</span>
                    <div className="flex shrink-0 items-center gap-1">{preferences.presets.map((speed, i) => <button key={i} type="button" data-agent="deny" data-gui-controller-control
                        aria-label={`${t('guiController.preset', { index: i + 1 })}: ${speed}%`}
                        onClick={event => { if (trusted(event)) onSpeed(speed); }}
                        className={`rounded px-1.5 py-0.5 ${speed === control.speed_percent ? 'bg-gray-200 dark:bg-gray-700' : 'hover:bg-gray-200 dark:hover:bg-gray-700'}`}>{speed}%</button>)}</div>
                </>}
            </div>
            {control && <>
                <button data-agent="deny" data-gui-controller-control type="button" onClick={event => { if (trusted(event)) onPause(!control.paused); }}
                    className="flex shrink-0 items-center gap-1 rounded px-2 py-0.5 hover:bg-gray-200 dark:hover:bg-gray-700">
                    {control.paused ? <Play size={12} /> : <Pause size={12} />}{t(control.paused ? 'guiController.resume' : 'guiController.pause')}
                </button>
                <div className="h-4 w-px shrink-0 bg-gray-300 dark:bg-gray-600" />
            </>}
            <button data-gui-controller-stop type="button" onClick={onStop} className="flex shrink-0 items-center gap-1 rounded bg-red-600 px-2 py-0.5 text-white hover:bg-red-700">
                <Square size={12} aria-hidden="true" />{t('guiController.actions.stop')}
            </button>
        </div>
    </>;
}
