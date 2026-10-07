// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Shared Settings handlers for the GUI controller. App wires these to the
// real UI state setters; tests wire them to fixtures. The update paths are
// deliberately NOT the panels' whole-Save handlers:
//
// - general merges allowlisted keys through updateAppSettings, the same
//   public-preference writer the UI hooks use. SettingsPanel.handleSave also
//   rewrites saved server profiles, hydrated OAuth client secrets and native
//   menu/autostart state, so it is never called from here.
// - ai merges a validated bounded update through the serialized public-blob
//   queue, which strips apiKey fields defensively and never enqueues,
//   overwrites or clears an `ai_apikey_*` keyring record.

import type { GuiHandlers } from './controller';
import {
    validateSettingsSet, type AiSettingsProjection, type AiSettingsUpdate,
} from './settingsSchema';
import { updateAppSettings } from '../utils/appSettings';
import { applyAgentAiSettingsUpdate, readAiPublicProjection } from '../utils/aiSettingsStore';
import { bindSettingsScope } from './settingsScope';

export interface SettingsHandlerDeps {
    /** Open the general Settings panel (its safe surface) and close the AI one. */
    openGeneral(): void;
    /** Open the AI Settings modal through the DevTools chain, closing general. */
    openAi(): void;
    /** Close every owned Settings surface. */
    closeAll(): void;
    /** Publish a freshly read public AI projection into the GUI source. */
    onAiProjection(projection: AiSettingsProjection): void;
}

export function createSettingsHandlers(
    deps: SettingsHandlerDeps,
): Required<Pick<GuiHandlers, 'settingsOpen' | 'settingsClose' | 'settingsRead' | 'settingsUpdate'>> {
    return {
        settingsOpen: async (area, parent) => {
            const scope = await bindSettingsScope(parent);
            await scope.step(() => { if (area === 'general') deps.openGeneral(); else deps.openAi(); });
        },
        settingsClose: async parent => {
            const scope = await bindSettingsScope(parent);
            await scope.step(deps.closeAll);
        },
        settingsRead: async (area, parent) => {
            const scope = await bindSettingsScope(parent);
            // The general projection is the live useSettings state, already
            // authoritative; the AI projection is re-read from the persisted
            // public blob on demand (no key hydration anywhere on this path).
            if (area === 'ai') {
                const projection = await readAiPublicProjection(scope);
                scope.assert(); deps.onAiProjection(projection);
            }
        },
        settingsUpdate: async (area, set, parent) => {
            const validated = validateSettingsSet(area, set);
            const scope = await bindSettingsScope(parent);
            if (area === 'general') {
                const applied = validated as Record<string, unknown>;
                await updateAppSettings(existing => ({ ...(existing || {}), ...applied }), scope);
            } else {
                await applyAgentAiSettingsUpdate(validated as AiSettingsUpdate, scope);
            }
        },
    };
}
