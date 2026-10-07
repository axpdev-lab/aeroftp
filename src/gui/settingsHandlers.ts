// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Shared Settings handlers for the GUI controller. App wires these to the
// real UI state setters; tests wire them to fixtures. The update paths are
// deliberately NOT the panels' whole-Save handlers:
//
// - general commits the original approved delta through the backend broker,
//   serialized with the public-preference writer the UI hooks use. SettingsPanel.handleSave also
//   rewrites saved server profiles, hydrated OAuth client secrets and native
//   menu/autostart state, so it is never called from here.
// - ai commits its original approved delta through the backend and serialized public-blob
//   queue, which strips apiKey fields defensively and never enqueues,
//   overwrites or clears an `ai_apikey_*` keyring record.

import { invoke } from '@tauri-apps/api/core';
import type { AISettings } from '../types/ai';
import { GuiError } from './errors';
import type { GuiHandlers } from './controller';
import {
    validateSettingsSet, type AiSettingsProjection,
} from './settingsSchema';
import { commitAppSettings } from '../utils/appSettings';
import { commitAiSettingsBlob, readAiPublicProjection } from '../utils/aiSettingsStore';
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
            validateSettingsSet(area, set);
            // A harness or arbitrary caller cannot turn a frontend handler into write authority.
            if (!parent.brokerId) throw new GuiError('blocked');
            const remove = parent.onCancel(() => { void invoke('gui_intent_cancel', { id: parent.brokerId }).catch(() => {}); });
            try {
                const scope = await bindSettingsScope(parent);
                const commit = async () => {
                    let result: { area: string; value: Record<string, unknown> };
                    try { result = await invoke('gui_settings_commit', { id: parent.brokerId }); }
                    catch (error) {
                        const code = String(error);
                        if (['invalid_args', 'locked', 'busy', 'gui_timeout'].includes(code)) throw new GuiError(code as 'invalid_args' | 'locked' | 'busy' | 'gui_timeout');
                        if (['gui_scope_changed', 'gui_cancelled', 'gui_unknown_request'].includes(code)) throw new GuiError('lease_interrupted');
                        throw new GuiError('action_failed');
                    }
                    if (result.area !== area || !result.value || typeof result.value !== 'object' || Array.isArray(result.value)) throw new GuiError('action_failed');
                    return result.value;
                };
                if (area === 'general') await commitAppSettings(commit, scope);
                else await commitAiSettingsBlob(async () => await commit() as unknown as AISettings, scope);
            } finally { remove(); }
        },
    };
}
