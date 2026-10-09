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
        <div data-gui-controller-badge role="status" aria-live="polite" className="fixed bottom-12 left-1/2 -translate-x-1/2 z-[9998] flex max-w-[95vw] flex-wrap items-center gap-3 rounded-xl border border-amber-400 bg-amber-50 px-4 py-3 text-amber-950 shadow-xl dark:bg-gray-900 dark:text-amber-200">
            <Bot size={20} aria-hidden="true" />
            <div>
                <p className="text-sm font-medium">{t('guiController.banner', { agent: lease.owner.label })}</p>
                {lease.intent && <p className="text-xs">{t(actionKey)}</p>}
                {control?.paused && <p className="text-xs font-semibold">{t('guiController.paused')}</p>}
                {lease.intent && point.fallback && <p className="text-xs">{t('guiController.fallback')}</p>}
            </div>
            {control && <>
                <div className="text-xs">
                    <label className="flex items-center gap-2">{t('guiController.speed')} {control.speed_percent}%
                        <input data-agent="deny" data-gui-controller-control type="range" min={MIN_GUI_SPEED} max={MAX_GUI_SPEED} step={1}
                            aria-label={t('guiController.speed')} value={control.speed_percent}
                            onChange={event => { if (trusted(event)) onSpeed(Number(event.target.value)); }} />
                    </label>
                    <p>{t(`guiController.speedSource.${control.speed_source}`)}</p>
                    <div className="flex gap-2">{preferences.presets.map((speed, i) => <button key={i} type="button" data-agent="deny" data-gui-controller-control
                        aria-label={`${t('guiController.preset', { index: i + 1 })}: ${speed}%`}
                        onClick={event => { if (trusted(event)) onSpeed(speed); }} className="rounded border px-1">{speed}%</button>)}</div>
                </div>
                <button data-agent="deny" data-gui-controller-control type="button" onClick={event => { if (trusted(event)) onPause(!control.paused); }} className="flex items-center gap-1 rounded-lg border px-3 py-2 text-sm">
                    {control.paused ? <Play size={14} /> : <Pause size={14} />}{t(control.paused ? 'guiController.resume' : 'guiController.pause')}
                </button>
            </>}
            <button data-gui-controller-stop type="button" onClick={onStop} className="flex items-center gap-1 rounded-lg bg-red-600 px-3 py-2 text-sm text-white hover:bg-red-700">
                <Square size={14} aria-hidden="true" />{t('guiController.actions.stop')}
            </button>
        </div>
    </>;
}
