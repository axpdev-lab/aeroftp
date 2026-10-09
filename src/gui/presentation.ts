// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
export const MIN_GUI_SPEED = 10;
export const MAX_GUI_SPEED = 400;
export interface GuiPresentationSettings { defaultSpeed: number; presets: [number, number, number]; }
// Compatibility fallback for an unset preference; factory production choice is still under review.
export const DEFAULT_GUI_PRESENTATION: GuiPresentationSettings = { defaultSpeed: 100, presets: [50, 100, 200] };
export const isGuiSpeed = (value: unknown): value is number => Number.isInteger(value) && Number(value) >= MIN_GUI_SPEED && Number(value) <= MAX_GUI_SPEED;
export function normalizeGuiPresentation(value: unknown): GuiPresentationSettings {
    const v = value && typeof value === 'object' ? value as Partial<GuiPresentationSettings> : {};
    return { defaultSpeed: isGuiSpeed(v.defaultSpeed) ? v.defaultSpeed : DEFAULT_GUI_PRESENTATION.defaultSpeed,
        presets: [0, 1, 2].map(i => isGuiSpeed(v.presets?.[i]) ? v.presets![i] : DEFAULT_GUI_PRESENTATION.presets[i]) as [number, number, number] };
}
export type GuiPhase = 'ready' | 'move' | 'press' | 'running' | 'dwell';
export interface GuiControl { speed_percent: number; speed_source: 'default' | 'agent' | 'human'; paused: boolean; phase: GuiPhase; }
export const phaseDuration = (phase: GuiPhase, speed: number): number => Math.round(({ ready: 0, move: 250, press: 100, running: 0, dwell: 500 }[phase]) * 100 / speed);
